# Releasing Portputer

Each script builds its app, signs it, and adds it to an update feed at the top of the repository (`appcast.xml`, `appcast-windows.xml`, `appcast-android.json`). Then attach the files in the `dist` folders to a GitHub release named `v<version>`, and commit and push the feeds. Set the version in `Cargo.toml`; the build number is the number of commits, so commit before releasing.

- `macos/release.sh [notes.html]`: `macos/dist/Portputer-<version>.dmg`
- `windows/release.sh [notes.html]`: `windows/dist/Portputer-<version>-windows-x64-setup.exe` (needs `brew install makensis`)
- `android/release.sh [notes.txt]`: `android/dist/Portputer-<version>.apk`

The Mac and Windows updates are signed with an Ed25519 key in the login Keychain (account "portputer", made by Sparkle's `generate_keys --account portputer`); its public half is `macos/Support/sparkle-public-key.txt`. The Android app is signed with `~/.android/portputer.keystore`, whose password is in the login Keychain as "Portputer Android signing key" (`android/make-signing-key.sh`). Keep backups of both: installed copies accept only updates signed with them.
