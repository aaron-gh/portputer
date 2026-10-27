// Portputer's internet relay: a Cloudflare Worker that joins a paired
// device's WebSocket to its computer's, for computers not on the device's
// network.
//
// Each computer has a room: a random 256-bit name only it and its paired
// devices know. There is no way to list rooms, and every refusal looks the
// same as a room that doesn't exist, so rooms can't be found by guessing.
//
//   GET /listen/<room>?key=<k>&session=<user|login_window>
//       The computer, all the time. The first listener claims the room with
//       its key; later listeners need the same key. It sends the hashes of
//       its paired devices' tokens ({"type":"allow","tokens":[...]}) and is
//       told of each device that connects ({"type":"incoming","conn":...}).
//   GET /connect/<room>?token=<t>
//       A paired device: one relayed connection. Only tokens the computer
//       allows get in.
//   GET /accept/<room>/<conn>?key=<k>
//       The computer's end of that connection.
//   GET /status/<room>?token=<t>
//       For a paired device: {"online": true} while the computer listens.
//       Plain HTTP, so devices can show which computers they can reach.
//
// Bytes are passed through as they are. Portputer's own encryption (Noise)
// runs inside, end to end: the relay sees only ciphertext.

const NOT_FOUND = () => new Response("Not found\n", { status: 404 });

async function sha256(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const parts = url.pathname.split("/").filter(Boolean);
    if (parts.length === 0) {
      return new Response("Portputer relay\n");
    }
    const [kind, room] = parts;
    if (!["listen", "connect", "accept", "status"].includes(kind) || !/^[0-9a-f]{64}$/.test(room ?? "")) {
      return NOT_FOUND();
    }
    if (kind !== "status" && request.headers.get("Upgrade") !== "websocket") {
      return new Response("WebSocket only\n", { status: 426 });
    }
    return env.ROOMS.get(env.ROOMS.idFromName(room)).fetch(request);
  },
};

export class Room {
  constructor(state) {
    this.state = state;
    // Messages from a device before the computer's end of its connection
    // arrives, by connection.
    this.waiting = new Map();
    // The computer's keep-alives are answered without waking the object.
    state.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  async keyMatches(key) {
    if (!key) return false;
    const claim = await this.state.storage.get("claim");
    return claim !== undefined && claim === (await sha256(key));
  }

  async fetch(request) {
    const url = new URL(request.url);
    const [kind, , conn] = url.pathname.split("/").filter(Boolean);
    if (kind === "status") {
      const token = url.searchParams.get("token");
      const allowed = (await this.state.storage.get("allow")) ?? [];
      if (!token || !allowed.includes(await sha256(token))) return NOT_FOUND();
      const online = this.state.getWebSockets("listen").length > 0;
      return new Response(JSON.stringify({ online }), { headers: { "content-type": "application/json", "cache-control": "no-store" } });
    }
    const { 0: outside, 1: inside } = new WebSocketPair();

    if (kind === "listen") {
      const key = url.searchParams.get("key");
      if (!key || key.length < 32) return NOT_FOUND();
      const claim = await this.state.storage.get("claim");
      if (claim === undefined) {
        await this.state.storage.put("claim", await sha256(key));
      } else if (claim !== (await sha256(key))) {
        return NOT_FOUND();
      }
      const session = url.searchParams.get("session") === "login_window" ? "login_window" : "user";
      for (const old of this.state.getWebSockets("listen:" + session)) {
        old.close(4000, "Another listener took over");
      }
      this.state.acceptWebSocket(inside, ["listen", "listen:" + session]);
    } else if (kind === "connect") {
      const token = url.searchParams.get("token");
      const allowed = (await this.state.storage.get("allow")) ?? [];
      if (!token || !allowed.includes(await sha256(token))) return NOT_FOUND();
      // The logged-in user's server first, then the one at the login window.
      const listener = this.state.getWebSockets("listen:user")[0] ?? this.state.getWebSockets("listen:login_window")[0];
      if (!listener) {
        return new Response("The computer isn't connected to the relay\n", { status: 503 });
      }
      const id = crypto.randomUUID();
      this.state.acceptWebSocket(inside, ["client:" + id]);
      this.waiting.set(id, []);
      listener.send(JSON.stringify({ type: "incoming", conn: id }));
    } else if (kind === "accept") {
      if (!(await this.keyMatches(url.searchParams.get("key")))) return NOT_FOUND();
      if (!conn || this.state.getWebSockets("client:" + conn).length === 0) {
        return new Response("No such connection\n", { status: 410 });
      }
      this.state.acceptWebSocket(inside, ["server:" + conn]);
      for (const message of this.waiting.get(conn) ?? []) {
        inside.send(message);
      }
      this.waiting.delete(conn);
    } else {
      return NOT_FOUND();
    }
    return new Response(null, { status: 101, webSocket: outside });
  }

  /** The other end of a relayed connection, if it's there. */
  peer(ws) {
    const tag = this.state.getTags(ws).find((t) => t.startsWith("client:") || t.startsWith("server:"));
    if (!tag) return {};
    const [side, conn] = tag.split(":");
    const other = (side === "client" ? "server:" : "client:") + conn;
    return { side, conn, peer: this.state.getWebSockets(other)[0] ?? null };
  }

  async webSocketMessage(ws, message) {
    if (this.state.getTags(ws).includes("listen")) {
      // The computer's list of paired devices' token hashes.
      if (typeof message === "string") {
        try {
          const m = JSON.parse(message);
          if (m.type === "allow" && Array.isArray(m.tokens)) {
            const tokens = m.tokens.filter((t) => typeof t === "string" && /^[0-9a-f]{64}$/.test(t)).slice(0, 256);
            await this.state.storage.put("allow", tokens);
          }
        } catch {}
      }
      return;
    }
    const { side, conn, peer } = this.peer(ws);
    if (!side) return;
    if (peer) {
      peer.send(message);
    } else if (side === "client" && this.waiting.has(conn)) {
      this.waiting.get(conn).push(message);
    }
  }

  webSocketClose(ws) {
    const { conn, peer } = this.peer(ws);
    if (conn) this.waiting.delete(conn);
    if (peer) peer.close(1000, "The other side closed");
  }

  webSocketError(ws) {
    this.webSocketClose(ws);
  }
}
