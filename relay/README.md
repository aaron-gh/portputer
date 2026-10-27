# Portputer relay

A Cloudflare Worker that connects paired devices to computers that aren't on their network. The computer keeps a WebSocket open to the relay; a device that can't reach the computer directly connects through it. Portputer's encryption runs end to end inside: the relay only passes ciphertext on, and a relay posing as the computer fails the device's key check. Audio and the microphone go through it too, over WebSockets instead of UDP, with a larger audio buffer.

Uses Durable Objects (one per computer), which the Workers Free plan includes.

## Deploying

From this folder, with Node.js:

    npm install
    npx wrangler login
    npx wrangler secret put RELAY_TOKEN    # any long random text
    npx wrangler deploy

Wrangler prints the Worker's address, such as `https://portputer-relay.<account>.workers.dev`.

Then, on each computer: Portputer Settings, Internet Relay, enter the address and the token, Save Relay Settings. The status says "Listening at the relay." Devices learn the address and token from the computer the next time they connect on the local network, or when they pair; after that, when the computer isn't on their network, they connect through the relay by themselves.

## Testing locally

    echo RELAY_TOKEN=testtoken > .dev.vars
    npx wrangler dev --port 8787
    # in another terminal, from the repository root:
    PORTPUTER_RELAY_TEST_URL=http://127.0.0.1:8787 PORTPUTER_RELAY_TEST_TOKEN=testtoken \
        cargo test -p portputer-core --test pairing through_relay

## Endpoints

All WebSocket, with `?token=<RELAY_TOKEN>`; `<id>` is the computer's Portputer ID.

| Path | Who | What |
|---|---|---|
| `/listen/<id>` | computer | Held open; receives `{"type":"incoming","conn":"<conn>"}` for each device. A newer listener replaces an older one. Text `ping` is answered `pong`. |
| `/connect/<id>` | device | One relayed connection. 503 if the computer isn't listening. |
| `/accept/<id>/<conn>` | computer | Its end of that connection. |

Binary messages are passed to the other end unchanged; closing either end closes the other.
