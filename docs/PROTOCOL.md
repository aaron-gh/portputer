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
| `welcome` | `server_id`, `name`, `os`, `session`, `user`, `paired`, `version`, `power` (actions available), `audio` (bool), `mute` (bool: speakers can be muted while streaming), `mic` (bool: the Portputer Microphone driver is installed), `files` (bool: clipboard and file transfer) |
| `paired` | `proof` |
| `error` | `code`, `message` |
| `notice` | `code`, `message` (such as a missing permission on the server) |
| `audio_ready` | `port` (UDP), `key` (hex, 32 bytes), `sample_rate`, `channels`, `frames` (per packet), `turn` (optional: the server's relayed address at the TURN service) |
| `audio_stopped` | `reason` |
| `mic_ready` | `port` (UDP), `key` (hex, 32 bytes), `turn` (optional, as for `audio_ready`) |
| `mic_stopped` | `reason` |
| `relay` | `url`, `room`, `token`: the server's room at the internet relay and this client's token there; sent to paired clients only |
| `addresses` | `addresses`, `port`: every address the server has (local network, Tailscale and other VPNs); sent to paired clients only |
| `clipboard_text` | `text` copied on the server (up to 60,000 bytes) |
| `clipboard_files` | `files`: `name` and `size` of each file copied on the server |
| `pong` | `n` |

Error codes: `not_paired`, `pairing_failed`, `no_pairing_code`, `unsupported`, `power_failed`, `audio_failed`, `mute_failed`, `mic_failed`, `bad_message`, `version`.

The speakers are muted while any client streaming audio asks for it, and unmuted when none does or the stream stops.

### Messages from the client

| Type | Fields |
|---|---|
| `key` | `code`, `down`, `repeat` (optional; repeated key-downs of a held key) |
| `tap` | `code`, `modifiers` (codes held while the key is pressed and released) |
| `text` | `text` (typed as Unicode, independent of keyboard layout) |
| `release_all` | |
| `audio_start` | `mute` (optional): keep the sound from the computer's speakers while it streams; `turn` (optional, relayed clients): the client's relayed address at the TURN service |
| `audio_stop` | |
| `mute_speakers` | `muted` |
| `clipboard_watch` | `on`: the server sends its clipboard as it changes |
| `clipboard_text` | `text`: put on the server's clipboard |
| `mic_start` / `mic_stop` | the client's microphone, to the server's Portputer Microphone; `mic_start` takes `turn` as `audio_start` does |
| `power` | `action`: `restart`, `shutdown`, `sleep`, `log_out`, `lock`, `force_restart`, `unlocked_restart`; `user` and `password` for `unlocked_restart` |
| `ping` | `n`, every 5 seconds |

Keys are W3C `KeyboardEvent.code` values: the physical key, not the character. The server's system applies its own keyboard layout. `crates/portputer-core/src/keys.rs` maps each code to Linux input codes (Android scan codes), Android key codes, macOS virtual key codes and Windows scan codes or virtual keys. Codes Portputer adds: `Fn`, `BrightnessUp`, `BrightnessDown`, `KeyboardBacklightUp`, `KeyboardBacklightDown`, `MissionControl`, `Launchpad`, `Eject`.

The server releases every key a client holds when it disconnects. A client connecting to a paired server tries, all at once, the address it was given, the last one that worked and every address from `addresses`; the first to answer wins. Only if none does, it uses the relay. A relayed connection moves to a direct one when an address answers, unless Use Internet Relay is on; turning it on moves a direct connection to the relay (off for each new session). The client reconnects for up to 20 minutes after an unexpected disconnect, to the server's current announcement (user session first, then login window) or its last address, and drops key events while disconnected.

## Audio

Started with `audio_start`. The server opens a UDP socket per client and answers `audio_ready`. The client sends a keep-alive to that port every 500 ms; the server sends audio to the address the latest valid keep-alive came from, and stops 5 seconds after the last.

Packet: `0x50`, kind, sequence number (32-bit big-endian), then ChaCha20-Poly1305 ciphertext of the payload with the 6-byte header as associated data. Nonce: kind, 7 zero bytes, sequence number.

| Kind | Direction | Payload |
|---|---|---|
| 1 | server to client | PCM, 16-bit little-endian, interleaved, `frames` frames (5 ms) |
| 2 | client to server | empty keep-alive |
| 3 | client to server | microphone: PCM, 16-bit little-endian, 48 kHz mono, 240 frames (5 ms), to the `mic_ready` port, with its key |

The client's jitter buffer orders packets by sequence number, holds a set amount (20 to 250 ms) or, by default, an automatic amount: the 98th percentile of the last 4 seconds' arrival jitter plus 6 ms, 12 to 300 ms, raised at once and lowered gradually. It drops packets that arrive after their turn, replaces a lost packet with the previous one at half volume (at most three times, then silence), and resamples to the output device's rate with a speed correction of up to 1% to keep the buffer at its target as the two clocks drift. When the buffer's low point stays more than a packet above the target for half a second (after a stall on a TCP path delivers a burst), the excess packets are dropped with a one-packet crossfade.

## File transfer

A second connection, with the same handshake and `"transfer": true` in the hello, for each operation. After the welcome, the client sends a request and the server answers. File data goes in data frames: Noise transport messages whose plaintext is a zero byte and up to 65,518 bytes of the file. Other frames are JSON.

| Request | Answer |
|---|---|
| `list` `path` (empty: home) | `listing`: `path`, `parent`, `entries` (`name`, `path`, `dir`, `size`, `modified`), `places` (`name`, `path`) |
| `download` `path` | `file_start` (`name`, `size`), data frames, `file_end`, `done` |
| `upload` `name`, `size`, `clipboard` (optional copy number), then data frames | `done` `path`: saved in Downloads, or with `clipboard`, in a cache folder and put on the clipboard with the copy's other files |
| `clipboard_files` | each file on the server's clipboard as `file_start`, data, `file_end`; then `done` |

Errors are `error` `message`. Received file names keep only their last component, and an existing name gets a number ("report 2.pdf"). Transfers need a paired client and a logged-in user.

## Internet relay

`relay/README.md` describes the relay's endpoints. A relayed connection is a byte stream like TCP, so the handshake and everything after it are the same; the relay sees only Noise ciphertext. A client connects through the relay when the server isn't reachable directly and it has the server's room and its token from a `relay` message. The server makes its room (256 random bits) and room key once, and a token per paired client at pairing; it keeps the relay's list of allowed token hashes current.

A relayed client's audio and microphone packets go over TURN when the relay has it (`/turn/<room>` gives credentials): the client allocates a relayed address at the TURN service and sends it as `turn` in `audio_start` or `mic_start`; the server allocates its own, binds a channel (0x4000) to the client's and answers with its address as `turn` in `audio_ready` or `mic_ready`; the client binds a channel to that. Packets are the same sealed packets as over UDP, sent as ChannelData. The client sends keep-alives over audio's path, and the server over the microphone's, so the other end can tell the path works.

Otherwise, or when nothing arrives over TURN within 3 seconds (the client then starts the stream again without `turn`, and doesn't try TURN again in that session), a relayed client opens a datagram connection: a further relayed connection with `"datagram": "audio"` or `"datagram": "mic"` in the hello, on which each data frame is one packet, sealed with the stream's packet key as over UDP. `audio_ready` and `mic_ready` have port 0 for relayed clients.
