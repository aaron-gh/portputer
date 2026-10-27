#!/bin/bash
# Makes Portputer's Android signing key, once: ~/.android/portputer.keystore,
# with a random password kept in the login Keychain ("Portputer Android
# signing key"). Android installs an update only when it's signed with the
# same key as the installed app, so keep a backup of both: without them,
# installed copies can't be updated.
set -euo pipefail
KEYSTORE="${PORTPUTER_ANDROID_KEYSTORE:-$HOME/.android/portputer.keystore}"
if [[ -f "$KEYSTORE" ]]; then
    echo "$KEYSTORE already exists." >&2
    exit 1
fi
KEYTOOL="$(/usr/libexec/java_home 2>/dev/null)/bin/keytool"
[[ -x "$KEYTOOL" ]] || KEYTOOL=keytool
PASSWORD=$(openssl rand -hex 24)
mkdir -p "$(dirname "$KEYSTORE")"
"$KEYTOOL" -genkeypair -keystore "$KEYSTORE" -storetype PKCS12 -alias portputer \
    -keyalg RSA -keysize 4096 -validity 36500 -dname "CN=Portputer, O=aaron-gh" \
    -storepass "$PASSWORD" -keypass "$PASSWORD" >/dev/null
chmod 600 "$KEYSTORE"
security add-generic-password -U -a portputer -s "Portputer Android signing key" -w "$PASSWORD"
echo "Made $KEYSTORE; its password is in the login Keychain as \"Portputer Android signing key\"."
