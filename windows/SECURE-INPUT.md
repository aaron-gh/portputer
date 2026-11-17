# Windows secure-screen input

Portputer can optionally allow a paired device to control a Windows session while it is locked, including entering a password and responding to UAC prompts.

The Allow on Windows secure screens option is off by default. Turning it on shows a security warning and requests administrator permission. Portputer then installs and starts the bundled helper and restarts the app from its protected installation location. There are no separate setup scripts to run.

Turning the option off stops and disables the helper, removes its firewall rule and restarts Portputer as a normal user application. Existing pairings, settings and NVDA configuration are kept.

The Windows installer includes the helper and removes it during uninstallation, with administrator permission. If an update requires changes to the protected component, the Update secure-screen support option requests administrator permission to update it.

## Security

Enabling this option allows trusted paired devices to type on the Windows lock screen and interact with UAC prompts in the existing signed-in session. It does not bypass Windows authentication. Only enable it for devices you trust.

The main Portputer application still handles pairing, encrypted connections, networking, clipboard and audio. A local service running as SYSTEM launches input workers only for sessions belonging to the configured user. The service does not listen on the network.

Communication with the helper uses local named pipes that reject remote clients. The worker checks the connecting account, session and executable path. The client checks the pipe's session and verifies that its owner is SYSTEM before sending input. Identification-only SQOS is used to prevent a fake pipe server from impersonating the client.

The protected installation is owned by Administrators, and only Administrators and SYSTEM have write access. Setup backs up existing component files before replacing them. The firewall rule is specific to Portputer and restricted to local subnets and Tailscale address ranges; it is removed when the option is disabled.

The helper only accepts limited keyboard-input batches, key-release requests and diagnostic checks. It rejects unknown operations or fields, invalid encodings and oversized messages. Failed input is not replayed. Held keys are released when a connection closes, the desktop changes, the service exits or the idle timeout is reached. Diagnostics do not contain keystrokes, typed text, credentials or clipboard data, and are not uploaded.

This feature needs a security review before release. If the authorised user application is compromised or another process can inject code into it, an attacker may be able to control secure screens. Checking the installation path alone does not establish that a binary is trusted or protect against process injection. Installer and update integrity, caller-process protection, isolation between Windows sessions and recovery after crashes need particular attention.

This implementation does not install a kernel driver, request additional UIAccess privileges, weaken UAC policy or change Windows desktop ACLs.

## Scope and testing

This currently works in an existing Windows session after it has been locked. Portputer does not yet start before the first sign-in following a reboot. BitLocker unlock, biometric sign-in and secure Ctrl+Alt+Delete are not supported by this change.

Testing so far includes:

- Six passing Rust tests covering malformed and oversized requests, key encoding and release identity, rejection of a fake pipe server before input is sent, and non-blocking pipe behaviour.
- Keyboard input on the Winlogon desktop over encrypted LAN and Tailscale connections in a Windows 11 VM.
- Unlocking Windows, interacting with UAC prompts and confirming NVDA audio still works.
- Rejection of an executable outside the protected installation directory.
- Stopping the service, confirming its workers have stopped, then restarting it and confirming input works again.
- Enabling and disabling the option through the GUI, including the warning, UAC prompt and application restart.
- Successful application, helper and Windows installer builds.

Further testing and review are needed for multiple users, sign-out, crashes, reconnection and held-key edge cases. No binaries or diagnostic logs have been published as part of these checks.
