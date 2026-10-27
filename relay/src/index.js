// Portputer's internet relay: a Cloudflare Worker that joins a client's
// WebSocket to a computer's, for computers not on the client's network.
//
// One Durable Object per computer, named by its Portputer ID.
//   GET /listen/<id>         the computer, all the time: told of each client
//   GET /connect/<id>        a client: one relayed connection
//   GET /accept/<id>/<conn>  the computer's side of that connection
// Every request needs ?token=<RELAY_TOKEN> when that secret is set.
//
// Bytes are passed through as they are. Portputer's own encryption (Noise)
// runs inside, end to end: the relay sees only ciphertext, and a relay
// pretending to be the computer fails the client's key check.

const LISTENER = "listen";

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const parts = url.pathname.split("/").filter(Boolean);
    if (parts.length === 0) {
      return new Response("Portputer relay\n");
    }
    const [kind, id] = parts;
    if (!["listen", "connect", "accept"].includes(kind) || !/^[0-9a-f]{32}$/.test(id ?? "")) {
      return new Response("Not found\n", { status: 404 });
    }
    if (env.RELAY_TOKEN && url.searchParams.get("token") !== env.RELAY_TOKEN) {
      return new Response("Wrong relay token\n", { status: 403 });
    }
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("WebSocket only\n", { status: 426 });
    }
    const room = env.ROOMS.get(env.ROOMS.idFromName(id));
    return room.fetch(request);
  },
};

export class Room {
  constructor(state) {
    this.state = state;
    // Messages from a client before the computer's side of its connection
    // arrives, by connection.
    this.waiting = new Map();
    // The computer's keep-alives are answered without waking the object.
    state.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  async fetch(request) {
    const parts = new URL(request.url).pathname.split("/").filter(Boolean);
    const [kind, , conn] = parts;
    const { 0: outside, 1: inside } = new WebSocketPair();

    if (kind === "listen") {
      for (const old of this.state.getWebSockets(LISTENER)) {
        old.close(4000, "Another listener took over");
      }
      this.state.acceptWebSocket(inside, [LISTENER]);
    } else if (kind === "connect") {
      const listeners = this.state.getWebSockets(LISTENER);
      if (listeners.length === 0) {
        return new Response("The computer isn't connected to the relay\n", { status: 503 });
      }
      const id = crypto.randomUUID();
      this.state.acceptWebSocket(inside, ["client:" + id]);
      this.waiting.set(id, []);
      listeners[0].send(JSON.stringify({ type: "incoming", conn: id }));
    } else if (kind === "accept") {
      if (!conn || this.state.getWebSockets("client:" + conn).length === 0) {
        return new Response("No such connection\n", { status: 410 });
      }
      this.state.acceptWebSocket(inside, ["server:" + conn]);
      for (const message of this.waiting.get(conn) ?? []) {
        inside.send(message);
      }
      this.waiting.delete(conn);
    }
    return new Response(null, { status: 101, webSocket: outside });
  }

  /** The other end of a relayed connection, if it's there. */
  peer(ws) {
    const tag = this.state.getTags(ws).find((t) => t.includes(":"));
    if (!tag) return { tag: null, peer: null };
    const [side, conn] = tag.split(":");
    const other = (side === "client" ? "server:" : "client:") + conn;
    return { side, conn, peer: this.state.getWebSockets(other)[0] ?? null };
  }

  webSocketMessage(ws, message) {
    const { side, conn, peer } = this.peer(ws);
    if (!side) return;
    if (peer) {
      peer.send(message);
    } else if (side === "client" && this.waiting.has(conn)) {
      this.waiting.get(conn).push(message);
    }
  }

  webSocketClose(ws, code, reason) {
    const { conn, peer } = this.peer(ws);
    if (conn) this.waiting.delete(conn);
    if (peer) peer.close(1000, "The other side closed");
  }

  webSocketError(ws) {
    this.webSocketClose(ws, 1011, "error");
  }
}
