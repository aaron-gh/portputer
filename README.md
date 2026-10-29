# Portputer

Portputer controls a Mac or Windows computer from an Android phone, an iPhone or iPad, or another computer. It sends every key of a physical keyboard, plays the computer's audio on the phone, sends the phone's microphone to the computer, and keeps the two clipboards in step. It can also restart the computer and type at the Mac login window. It works with screen readers throughout: TalkBack, VoiceOver and NVDA.

## Getting started

1. Install Portputer on the computer you want to control and open it. On a Mac it runs in the menu bar; on Windows, in the notification area. Turn on Open at Login (Mac) or Start at login (Windows) so it's always available.
2. On a Mac, allow the permissions it asks for: Accessibility, for typing, and System Audio Recording, for sending the computer's sound. Both are in Settings, Permissions.
3. Install Portputer on the phone. In Android Settings, Accessibility, turn on Portputer Keyboard.
4. On the computer, choose Pair New Device. On the phone, choose the computer in the list and enter the code the computer shows. Pairing is needed once per computer.

After that, choosing the computer on the phone connects to it.

## On the phone

- **Computers:** paired computers, and computers running Portputer on the same network, with who is logged in. A paired computer elsewhere shows how it can be reached: through Tailscale, on another network, or through the internet relay. Wake (Wake on LAN) and Forget are actions on each computer.
- **Send Keyboard to Computer:** keys from an external keyboard go to the computer, system shortcuts included. Control-Meta-Escape switches the keyboard between computer and phone; other shortcuts are in Settings. Keys go by physical position by default, so the computer's keyboard layout applies. The phone's own buttons, such as volume, stay on the phone.
- **Computer Audio:** the computer's sound plays on the phone, with a volume control. Mute Computer Speakers keeps the sound off the computer's own speakers while it plays on the phone. The audio buffer is set automatically from the network; Settings, Audio buffer, has fixed sizes.
- **Send Microphone:** the phone's microphone becomes an input called Portputer Microphone on a Mac, for calls and recording. It needs the Portputer Microphone driver: on the Mac, Settings, Microphone, Install.
- **Share Clipboard:** text copied on either side can be pasted on the other. Copied files are sent too, up to 100 MB. Android lets only the app in front read the clipboard, so the phone's clipboard is sent while Portputer is open, and when it comes to the front.
- **Files:** browse the computer's folders and download files to Download/Portputer on the phone. Send Files puts files from the phone in Downloads on the computer.
- **Keys and Shortcuts:** keys a phone keyboard lacks, such as Command, Option, Fn, F1 to F24, media keys, Insert and Print Screen, and the computer's system shortcuts. Modifiers can be held.
- **Type Text:** types a piece of text on the computer.
- **Power:** Restart, Shut Down, Sleep, Lock Screen, Log Out, Force Restart, and on a Mac, Restart Without FileVault Unlock.
- **Use Internet Relay:** connects through the internet relay even when the computer can be reached directly. It's off for each new connection.

The connection stays up with the screen off, and reconnects by itself: after a restart to the Mac login window, and after login to the user's session.

With TalkBack on, TalkBack also receives the external keyboard's keys, so a TalkBack keyboard shortcut also acts on the phone.

Settings, at the bottom, shows the Portputer version.

## On an iPhone or iPad

The same as on an Android phone, with these differences, which iOS sets:

- **Send Keyboard to Computer:** a switch, kept across connections. While it's on, keys from a hardware keyboard go to the computer while Portputer is open. There is no shortcut to switch back. iOS keeps its own shortcuts, such as Command-Tab, Command-Space and Globe key combinations, and while VoiceOver is on, VoiceOver's keyboard commands, so VoiceOver is usually turned off while the keyboard is sent.
- **In the background:** the connection, Computer Audio and Send Microphone continue while the computer's audio or the microphone is on. Otherwise iOS suspends Portputer soon after it leaves the screen, and it reconnects when opened.
- **Share Clipboard:** the clipboard is read when it changes while Portputer is open, and when Portputer comes to the front. iOS asks before each read unless Paste from Other Apps is set to Allow, in iOS Settings, Apps, Portputer.
- **Files:** downloads go to Portputer's folder in the Files app, under On My iPhone.
- **Wake** (Wake on LAN) isn't available: iOS needs an entitlement from Apple for the broadcast packets it sends.

Needs iOS 17 or later. Updates come through the App Store or TestFlight.

## On a Mac

Portputer is in the menu bar. The menu shows the version.

- **Server:** paired devices connect to it. Connections are announced through VoiceOver.
- **Pair New Device:** shows a pairing code, spelled out for VoiceOver.
- **Connect to Computer:** controls another Mac or a Windows computer from this Mac. Send Keyboard to Computer takes every key, system shortcuts included, until Control-Command-Escape. Clipboards are kept in step; Send Files puts files in Downloads on the other computer.
- **Settings:**
  - Paired Devices: the devices paired with this Mac, with Remove for each.
  - Internet Relay: reach this Mac from other networks (on by default).
  - Microphone: installs the Portputer Microphone driver, which Send Microphone and Computer Audio at the login window need. Installing or removing it stops all sound for a moment.
  - Login Window: installs support for typing at the login window after a restart, before anyone logs in, and for Force Restart and Restart Without FileVault Unlock. With FileVault on, a normal restart stops at the FileVault unlock screen, where nothing can be controlled remotely; Restart Without FileVault Unlock skips it once. At the login window, Computer Audio needs the microphone driver, below.
  - About: the version.

Needs macOS 14.2 or later. Installing the microphone driver or login window support asks for an administrator's password.

## On Windows

One window, and an icon in the notification area. The window title shows the version.

- The server for paired devices, Start at login, Pair New Device, and paired devices.
- Computers on the network, Connect by Address, and the connected computer's controls: Send keyboard to computer (until Control-Windows-Escape), Computer audio, Send microphone, Use internet relay, Keys and Shortcuts, Power, Type, Send Files.
- With NVDA, status changes are spoken through NVDA.

Not available on Windows: the Portputer Microphone input, typing before login, and Restart Without Disk Unlock (BitLocker).

## Reaching a computer from anywhere

On the same network, computers are found automatically. Elsewhere, a paired device reaches its computer:

- **Directly, over Tailscale or another VPN:** the computer tells its paired devices every address it has, and they try each one.
- **Through the internet relay:** when no direct address answers. Nothing needs setting up. Only devices paired with a computer can find it or connect to it there, and unpairing a device removes its access at the relay too. Audio goes through Cloudflare's TURN service as UDP.

Everything works through either route: keys, audio, the microphone, the clipboard and files.

## Privacy and security

- Every connection is encrypted end to end (Noise, with Curve25519 and ChaCha20-Poly1305), including through the relay, which passes on data it can't read.
- Pairing uses a 12-character code shown on the computer. Each side proves it knows the code, so a device can't pair without it, and nothing can pose as a paired computer.
- Each computer and device has its own key, stored where only that account can read it.
- The relay sees which computers are connected, IP addresses, and how much data passes. `relay/README.md` has the details.

## Updates

Portputer checks for updates by itself and offers to install them:

- **Mac:** once a day. Check for Updates is in the menu bar menu and in Settings, About. After an update, if login window support or the microphone driver is from the older version, Settings shows Update… beside it.
- **Windows:** once a day. Check for Updates is in the window and the notification area menu. Portputer starts again after the update installs.
- **Android:** once a day, when Portputer opens. Check for Updates is in Settings. Android asks once for permission to install Portputer's updates, and to confirm each one.

Updates are signed, and each app checks the signature before installing.

## Telling me about a problem

Include the version: on the phone or iPad, Settings, at the bottom; on a Mac, the menu bar menu or Settings, About; on Windows, the window title.

## Building from source

| Path | What it is |
|---|---|
| `crates/portputer-core` | Protocol, encryption and pairing, discovery, server, client, audio, keys |
| `crates/portputer-ffi` | Bindings for Swift and Kotlin |
| `crates/portputer-agent` | The Mac login window server and helper |
| `crates/portputer-windows` | Windows app |
| `macos/` | Mac app, and the Portputer Microphone driver |
| `android/` | Android app |
| `ios/` | iPhone and iPad app |
| `relay/` | Internet relay, a Cloudflare Worker |
| `docs/PROTOCOL.md` | Wire protocol |

- Mac: `macos/build.sh`, into `macos/build/Portputer.app`. `macos/make-certificate.sh` makes the "Portputer Code Signing" certificate once, so macOS keeps permissions across builds.
- Android: `android/gradlew -p android :app:assembleDebug`. Needs Rust's `aarch64-linux-android` and `x86_64-linux-android` targets and the Android NDK.
- iOS: `ios/build.sh`, which builds the Rust core into `ios/Frameworks`, generates `ios/Portputer.xcodeproj` with XcodeGen (`brew install xcodegen`) and builds the app; `--rust-only` stops before Xcode, `--simulator` builds for the simulator. Needs Rust's `aarch64-apple-ios` and `aarch64-apple-ios-sim` targets, and Xcode's iOS platform.
- Windows, built on a Mac: `windows/build.sh`. Needs MinGW-w64 (`brew install mingw-w64`).
- Tests: `cargo test -p portputer-core`.

## Licence

Apache License 2.0.
