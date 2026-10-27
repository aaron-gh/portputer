# Portputer relay

A Cloudflare Worker that connects paired devices to computers that aren't on their network. Nobody sets anything up: the relay's address is built into the apps, every computer is reachable through it unless turned off, and devices learn what they need when they pair.

## Privacy

- Each computer makes a room the first time it runs: a random 256-bit name, and a key that proves the room is its own. The key never leaves the computer.
- When a device pairs, the computer gives it the room and a token for that device alone, over Portputer's encrypted connection. The computer keeps the hashes of its paired devices' tokens at the relay; unpairing a device takes its token out at once.
- The relay lets in only a token the computer allows, and only the room's key can listen or answer. There is no list of rooms. A wrong token, a wrong key and a room that doesn't exist all get the same 404, so nothing can be found by guessing.
- Portputer's own encryption (Noise) runs end to end inside each relayed connection: the relay passes on ciphertext it can't read, and a relay posing as a computer fails the device's key check. The relay sees room names, IP addresses and how much data passes.

## Deploying (once, for everyone)

From this folder, with Node.js and a Cloudflare account:

    npm install
    npx wrangler login
    npx wrangler deploy

Put the address Wrangler prints, such as `https://portputer-relay.<account>.workers.dev`, in `relay/relay-url.txt`. The build scripts (`macos/build.sh`, `windows/build.sh`, `android/build-rust.sh`) build it into the apps. A computer can use another relay instead: Settings, Internet Relay, Custom Relay.

### TURN for audio (optional)

With TURN set up, audio and microphone packets through the relay go over Cloudflare's TURN service as UDP, instead of in WebSocket messages, where one lost packet holds up the ones behind it. In the Cloudflare dashboard, Realtime, TURN Server: create a TURN key. Then, from this folder:

    npx wrangler secret put TURN_KEY_ID
    npx wrangler secret put TURN_KEY_API_TOKEN
    npx wrangler deploy

The relay gives paired devices and their computers credentials that last 24 hours. Without the secrets, `/turn` answers 404 and the apps use WebSocket messages. TURN is billed apart from Workers: the first 1,000 GB a month that Cloudflare sends out is free, then $0.05 per GB. Portputer's audio is about 0.7 GB an hour.

Durable Objects (one per computer) are included in the Workers Free plan. Audio through the relay is one WebSocket message per 5 ms packet, which counts against the plan's Durable Object requests: about 3 hours of audio a day fits the free plan; the paid plan ($5 a month) covers far more.

## Testing locally

    npx wrangler dev --port 8787
    # in another terminal, from the repository root:
    PORTPUTER_RELAY_TEST_URL=http://127.0.0.1:8787 cargo test -p portputer-core --test pairing through_relay

TURN against a local coturn instead of Cloudflare's (`brew install coturn`):

    turnserver -n --listening-ip=127.0.0.1 --relay-ip=127.0.0.1 --listening-port=34780 \
        --lt-cred-mech --user=u:p --realm=portputer --allow-loopback-peers --no-tls
    PORTPUTER_TURN_TEST=127.0.0.1:34780,u,p PORTPUTER_RELAY_TEST_URL=... cargo test -p portputer-core --test pairing --test turn

## Endpoints

All WebSocket. `<room>` is 64 hex digits.

| Path | Who | What |
|---|---|---|
| `/listen/<room>?key=<k>&session=<user\|login_window>` | computer | Held open. The first listener claims the room with its key. Sends `{"type":"allow","tokens":[<sha256 of each token>]}`; receives `{"type":"incoming","conn":"<conn>"}` for each device. Text `ping` is answered `pong`. |
| `/connect/<room>?token=<t>` | device | One relayed connection, to the logged-in user's server, or the login window's if there's no user's. 503 if neither is listening. |
| `/accept/<room>/<conn>?key=<k>` | computer | Its end of that connection. |
| `/status/<room>?token=<t>` | device | Plain HTTP: `{"online":true}` while the computer listens, so the device can say which computers it can reach. |
| `/turn/<room>?token=<t>` or `?key=<k>` | device or computer | Plain HTTP: TURN credentials, `{"urls":[...],"username":...,"credential":...}`. 404 without the TURN secrets. |

Binary messages are passed to the other end unchanged; closing either end closes the other.
