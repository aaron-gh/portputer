# Portputer protocol, version 1

## Discovery

Servers announce `_portputer._tcp` over multicast DNS. TXT record:

| Key | Value |
|---|---|
| `id` | Server ID: hex of the first 16 bytes of SHA-256 of the server's static public key |
| `name` | Computer name |
| `os` | `mac`, `windows` or `other` |
| `session` | `user` or `login_window` |
| `user` | User logged in at the console, or empty |
| `macs` | Comma-separated hardware addresses of interfaces with IPv4, for Wake on LAN |
| `ver` | Server version |

The server in a user session listens on TCP 47800, the one at the login window on 47801; each takes another port if its own is in use, and announces the port it has. Both servers on one computer have the same identity, so the same ID.

## Control connection

TCP, `TCP_NODELAY`. Every frame is a 16-bit big-endian length followed by that many bytes.

### Handshake

`Noise_XX_25519_ChaChaPoly_BLAKE2s`, prologue `portputer 1`. The client is the initiator.

1. Client to server: `e`
2. Server to client: `e, ee, s, es`
3. Client to server: `s, se`, payload `{"type":"hello","name":"<device name>","protocol":1}`

After it, both sides know each other's static public key. Every later frame is a Noise transport message (nonces counted per direction) holding one JSON object with a `type`.

### Pairing

The server shows a pairing code: 12 characters from `23456789ABCDEFGHJKLMNPQRSTUVWXYZ` (60 bits), shown as `XXXX-XXXX-XXXX`, valid for 10 minutes or 5 wrong tries. The normalised code is the typed code in upper case without separators.

- Client: `{"type":"pair","proof":hex(HMAC-SHA256(code, "client" || h))}`, where `h` is the Noise handshake hash.
- Server, if the proof matches: stores the client's static key, then `{"type":"paired","proof":hex(HMAC-SHA256(code, "server" || h))}`.
- The client checks the server's proof, then pins the server's static key under its ID.

A man in the middle has a different `h` with each side, so can't produce either proof without the code. A client sends nothing but `pair` and `ping` to a server until it is paired both ways: the server knows the client's key (`welcome.paired`) and the client has the server's key pinned.

### Messages from the server

| Type | Fields |
|---|---|
| `welcome` | `server_id`, `name`, `os`, `session`, `user`, `paired`, `version`, `power` (actions available), `audio` (bool), `mute` (bool: speakers can be muted while streaming) |
| `paired` | `proof` |
| `error` | `code`, `message` |
| `notice` | `code`, `message` (such as a missing permission on the server) |
| `audio_ready` | `port` (UDP), `key` (hex, 32 bytes), `sample_rate`, `channels`, `frames` (per packet) |
| `audio_stopped` | `reason` |
| `pong` | `n` |

Error codes: `not_paired`, `pairing_failed`, `no_pairing_code`, `unsupported`, `power_failed`, `audio_failed`, `mute_failed`, `bad_message`, `version`.

The speakers are muted while any client streaming audio asks for it, and unmuted when none does or the stream stops.

### Messages from the client

| Type | Fields |
|---|---|
| `key` | `code`, `down`, `repeat` (optional; repeated key-downs of a held key) |
| `tap` | `code`, `modifiers` (codes held while the key is pressed and released) |
| `text` | `text` (typed as Unicode, independent of keyboard layout) |
| `release_all` | |
| `audio_start` | `mute` (optional): keep the sound from the computer's speakers while it streams |
| `audio_stop` | |
| `mute_speakers` | `muted` |
| `power` | `action`: `restart`, `shutdown`, `sleep`, `log_out`, `lock`, `force_restart`, `unlocked_restart`; `user` and `password` for `unlocked_restart` |
| `ping` | `n`, every 5 seconds |

Keys are W3C `KeyboardEvent.code` values: the physical key, not the character. The server's system applies its own keyboard layout. `crates/portputer-core/src/keys.rs` maps each code to Linux input codes (Android scan codes), Android key codes, macOS virtual key codes and Windows scan codes or virtual keys. Codes Portputer adds: `Fn`, `BrightnessUp`, `BrightnessDown`, `KeyboardBacklightUp`, `KeyboardBacklightDown`, `MissionControl`, `Launchpad`, `Eject`.

The server releases every key a client holds when it disconnects. The client reconnects for up to 20 minutes after an unexpected disconnect, to the server's current announcement (user session first, then login window) or its last address, and drops key events while disconnected.

## Audio

Started with `audio_start`. The server opens a UDP socket per client and answers `audio_ready`. The client sends a keep-alive to that port every 500 ms; the server sends audio to the address the latest valid keep-alive came from, and stops 5 seconds after the last.

Packet: `0x50`, kind, sequence number (32-bit big-endian), then ChaCha20-Poly1305 ciphertext of the payload with the 6-byte header as associated data. Nonce: kind, 7 zero bytes, sequence number.

| Kind | Direction | Payload |
|---|---|---|
| 1 | server to client | PCM, 16-bit little-endian, interleaved, `frames` frames (5 ms) |
| 2 | client to server | empty keep-alive |

The client's jitter buffer orders packets by sequence number, holds a set amount (20 to 250 ms, 40 by default), drops packets that arrive after their turn, replaces a lost packet with the previous one at half volume (at most three times, then silence), and resamples to the output device's rate with a speed correction of up to 1% to keep the buffer at its target as the two clocks drift.
