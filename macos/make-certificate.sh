#!/bin/bash
# Makes the self-signed "Portputer Code Signing" certificate in the login
# keychain, which build.sh signs with. macOS remembers permissions such as
# Accessibility by signature, so builds signed with the same certificate keep
# them. Ten years; codesign may use it without asking.
#   macos/make-certificate.sh
set -euo pipefail

NAME="Portputer Code Signing"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
if security find-identity -p codesigning "$KEYCHAIN" | grep -q "\"$NAME\""; then
    echo "$NAME is already in the login keychain."
    exit 0
fi
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cat > "$WORK/cert.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
CNF
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$WORK/cert.cnf" \
    -keyout "$WORK/key.pem" -out "$WORK/cert.pem" 2>/dev/null
PASS=$(openssl rand -hex 16)
openssl pkcs12 -export -inkey "$WORK/key.pem" -in "$WORK/cert.pem" -name "$NAME" \
    -out "$WORK/cert.p12" -passout "pass:$PASS"
security import "$WORK/cert.p12" -k "$KEYCHAIN" -P "$PASS" -T /usr/bin/codesign >/dev/null
echo "Made $NAME in the login keychain."
