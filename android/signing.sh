# Sourced by Portputer's Android build scripts. Finds Portputer's Android
# signing key, so releases are signed the same way on every computer and
# install over each other, and sets:
#   PORTPUTER_ANDROID_KEYSTORE      the key, by default ~/.android/portputer.keystore
#   PORTPUTER_ANDROID_KEY_PASSWORD  its password, from the Keychain on a Mac
#   PORTPUTER_ANDROID_SIGNER        "portputer" when the key can be used, or
#                                   "debug" when builds fall back to this
#                                   computer's debug key
export PORTPUTER_ANDROID_KEYSTORE="${PORTPUTER_ANDROID_KEYSTORE:-$HOME/.android/portputer.keystore}"
if [[ -z "${PORTPUTER_ANDROID_KEY_PASSWORD:-}" && -f "$PORTPUTER_ANDROID_KEYSTORE" ]] && command -v security >/dev/null; then
    PORTPUTER_ANDROID_KEY_PASSWORD=$(security find-generic-password -a portputer -s "Portputer Android signing key" -w 2>/dev/null || true)
fi
export PORTPUTER_ANDROID_KEY_PASSWORD="${PORTPUTER_ANDROID_KEY_PASSWORD:-}"
if [[ -f "$PORTPUTER_ANDROID_KEYSTORE" && -n "$PORTPUTER_ANDROID_KEY_PASSWORD" ]]; then
    export PORTPUTER_ANDROID_SIGNER=portputer
else
    export PORTPUTER_ANDROID_SIGNER=debug
    unset PORTPUTER_ANDROID_KEY_PASSWORD
fi
