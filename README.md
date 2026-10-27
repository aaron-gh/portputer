# Portputer

Control a Mac or Windows computer from an Android phone or another computer: every key of a physical keyboard, the computer's audio output over UDP, power control, and typing at the Mac login window after a restart.

Rust core, with each system's own interface: Android views and an accessibility service, a SwiftUI menu bar app on the Mac, Win32 controls on Windows.

## Status

Early development. What has been verified:

- The core's unit tests, and an end-to-end test in one process: pairing, rejected wrong codes, reconnecting as a known device, refusal of a different computer at a known address, and a 1.5 s audio stream (test tone) through UDP, the jitter buffer and playback.
- Discovery between the agent and a client on a real network.
- Builds: the Android APK, Portputer.app, the agent, and the Windows app (cross-compiled and linked with MinGW-w64, not yet run).
- Pairing, keys and audio between the Android app on a Galaxy Z Fold7 and the Mac app.

Not yet run: the Mac app's client window, Mute Computer Speakers, the login window server, the helper daemon, and anything on Windows.

## Parts

| Path | What it is |
|---|---|
| `crates/portputer-core` | Protocol, Noise encryption and pairing, mDNS discovery, server, client with reconnecting, UDP audio and jitter buffer, key tables, key injection and power for macOS and Windows |
| `crates/portputer-ffi` | UniFFI bindings for Swift and Kotlin |
| `crates/portputer-agent` | Headless server: the Mac login window server, the root helper daemon, their installer |
| `crates/portputer-windows` | Windows app |
| `macos/` | Mac app |
| `android/` | Android app |
| `relay/` | Internet relay: a Cloudflare Worker |
| `docs/PROTOCOL.md` | Wire protocol |

## Android app

- Computers: paired computers and those found on the network, with their system and who is logged in. Activating a row connects. Wake (Wake on LAN, for a paired computer not on the network) and Forget are custom accessibility actions on the row; the same buttons are in the row for sighted use and hidden from screen readers. Add Computer by Address connects across networks.
- Pairing: once per computer, with the 12-character code the computer shows.
- Send Keyboard to Computer: the Portputer Keyboard accessibility service takes key events from external keyboards (built-in keyboards optionally) before the rest of the system and sends them to the computer. Keys go by physical position by default, so the computer's keyboard layout applies; Settings switches to the device's layout. The release shortcut switches between computer and phone: Control-Meta-Escape by default (Meta is the Windows or Command key), with others in Settings. The phone's own buttons, such as volume, stay on the phone.
- Computer Audio, with volume, and Mute Computer Speakers, which keeps the sound from the computer's own speakers while it plays on the phone. On a Mac the capture tap is set to muted, so the stream keeps the sound; on Windows the output device is muted, and put back as it was when the audio stops. Audio buffer in Settings.
- Send Microphone: the phone's microphone becomes Portputer Microphone, an input device on the Mac (needs the Portputer Microphone driver, below).
- Share Clipboard: text copied on either side is on the other's clipboard. Files copied on the computer (up to 100 MB in total) are transferred and put on the phone's clipboard; files copied on the phone are transferred and put on the computer's, so they paste in the Finder or File Explorer. Android lets only the app in front read the clipboard, so the phone's clipboard goes to the computer when it changes while Portputer is open, and when Portputer comes to the front.
- Files: the computer's folders, starting at the home folder, with Places (Home, Desktop, Documents, Downloads, and disks). Activating a file downloads it to Download/Portputer on the phone. Send Files uploads files from the phone to Downloads on the computer. Transfers run on a second encrypted connection, so they don't hold up keys.
- Keys and Shortcuts: keys a phone keyboard lacks (Command, Option, Fn, F1 to F24, media, brightness, Mission Control, Eject, Insert, Print Screen and others) and system shortcuts for the connected system. Activating a key presses it. Modifiers also have Hold, as a custom action and as a button hidden from screen readers.
- Type Text.
- Power: Restart, Shut Down, Sleep, Lock Screen, Log Out, Force Restart, Restart Without FileVault Unlock.
- A foreground service keeps the connection, audio and keyboard working with the screen off, with notification actions for switching the keyboard and disconnecting, and a low-latency Wi-Fi lock.
- Reconnects by itself when the connection drops: after a restart it reconnects to the login window server, and after login to the user's.

Build: `android/gradlew -p android :app:assembleDebug`. Gradle runs `android/build-rust.sh`, which needs Rust's `aarch64-linux-android` and `x86_64-linux-android` targets and the NDK.

Turn on Portputer Keyboard in Settings, Accessibility, after installing.

TalkBack also receives key events while the keyboard goes to the computer: Android delivers key events to every accessibility service that filters them, so a TalkBack keyboard shortcut also acts on the phone.

## Mac app

A menu bar app (no Dock icon; opening it again shows Settings):

- Server for paired devices, on port 47800. Open at Login. Announces connections through VoiceOver.
- Pair New Device: shows the code, spelled out for VoiceOver.
- Settings: permissions (Accessibility for typing; System Audio Recording, which macOS requests on first use), paired devices, login window support.
- Connect to Computer: the client, for controlling another Mac or a Windows PC. Its clipboard and the other computer's are kept in step, files included; Send Files uploads to the other computer's Downloads. Send Keyboard to Computer takes every key, system shortcuts included (as AAE does), until Control-Command-Escape.

Portputer Microphone (Settings, Microphone, Install, with an administrator's password) installs Portputer's own audio driver, `/Library/Audio/Plug-Ins/HAL/PortputerMicrophone.driver`: an Audio Server plug-in, which runs in user space and needs no kernel extension. It has an input device, Portputer Microphone, which apps record from, and a hidden output device the server plays the connected device's microphone into; both share one buffer and one clock. 48 kHz mono. Installing or removing it restarts Core Audio, which stops all sound for a moment. Source: `macos/Driver/`; `macos/Driver/test.sh` loads the built driver as Core Audio would and checks its devices and loopback.

Login window support (Settings, Install, with an administrator's password) installs:

- `/Library/Application Support/Portputer/portputer-agent`
- `/Library/LaunchAgents/com.portputer.loginwindow.plist`: the server launchd runs at the login window, before anyone logs in, on port 47801
- `/Library/LaunchDaemons/com.portputer.helper.plist`: a root daemon that restarts the Mac immediately, restarts past the FileVault unlock screen (`fdesetup authrestart`, password passed on standard input), and copies the server's identity and paired devices for the login window server. It accepts power requests from the console user or an administrator and pairing copies from administrators.

With FileVault on, a normal restart stops at the FileVault unlock screen, where nothing runs that Portputer could reach. Restart Without FileVault Unlock skips it once, so the Mac starts to the login window.

Untested and uncertain: whether macOS lets the login window server post key events and capture audio without further permission. If keys don't arrive at the login window, add `/Library/Application Support/Portputer/portputer-agent` in System Settings, Privacy & Security, Accessibility. Logs: `/Library/Logs/Portputer/`.

Build: `macos/build.sh` (release) or `macos/build.sh --debug`, into `macos/build/Portputer.app`. macOS 14.2 or later (Core Audio process taps). macOS keeps permissions by code signature. build.sh signs with Portputer's own self-signed certificate, "Portputer Code Signing", which `macos/make-certificate.sh` makes in the login keychain, so permissions last across rebuilds. Without it, it uses an Apple Development certificate, or signs ad hoc, which loses permissions at every rebuild.

## Windows app

One window and a notification area icon:

- Server for paired devices, Start at login (`--background`), Pair New Device, paired devices.
- Computers on the network, Connect by Address, and the connected computer's controls (clipboard kept in step, files included; Send Files): Send keyboard to computer (a low-level keyboard hook takes every key, Windows shortcuts included, while Portputer is in front, until Control-Windows-Escape), Computer audio, Keys and Shortcuts, Power, Type.
- Status changes are spoken through NVDA's controller client if `nvdaControllerClient.dll` is beside `Portputer.exe`, and are a UI Automation live region otherwise.

System audio is captured with WASAPI loopback, keys are sent with SendInput by scan code.

Build on the Mac: `windows/build.sh`, which needs MinGW-w64 (`brew install mingw-w64`).

Not yet on Windows: the microphone driver (Windows only loads kernel audio drivers signed by Microsoft), control before login (a service running on the secure desktop), Control-Alt-Delete, and Restart Without Disk Unlock (BitLocker).

Clipboard and files are off at the Mac login window, where the server runs as root. Not yet in the Mac and Windows clients: browsing and downloading the other computer's files (the Android app has it).

## Internet relay

When a paired computer isn't on the device's network, Portputer connects through an internet relay, a Cloudflare Worker (`relay/`). Nothing is set up by the person using it: the relay's address is built into the apps, computers are reachable through it unless turned off (Mac: Settings, Internet Relay; Windows: the relay checkbox), and a device learns its computer's room and its own token when it pairs. Only devices paired with a computer can find or reach it there; unpairing a device shuts it out at the relay too. Everything works through it: keys, audio (over WebSockets, with at least 100 ms of buffer), the microphone, the clipboard and files. `relay/README.md` covers deploying it and its privacy.

## Security

- Noise XX (Curve25519, ChaCha20-Poly1305, BLAKE2s) on the control connection; each computer and device has its own key pair.
- Pairing proofs are HMACs of the code over the handshake hash, in both directions; see `docs/PROTOCOL.md`.
- Audio packets are encrypted with a random key per stream, sent over the control connection.
- Keys are stored in files only the account can read: `~/Library/Application Support/Portputer` on the Mac, `%APPDATA%\Portputer` on Windows, the app's private storage on Android.

## Licence

Apache License 2.0.
