#!/bin/bash
# Builds Portputer.app: the Rust core, the Swift bindings, the app and the
# agent it installs for the login window.
#   macos/build.sh            release build into macos/build/Portputer.app
#   macos/build.sh --debug    debug build, quicker to compile
#
# Updates come through Sparkle. These settings shape the build:
#   PORTPUTER_FEED_URL          the update feed (default: appcast.xml in
#                               Portputer's GitHub repository)
#   PORTPUTER_ED_PUBLIC_KEY     the public key updates are signed with (default:
#                               macos/Support/sparkle-public-key.txt; without
#                               one, Portputer won't install updates)
#   PORTPUTER_VERSION           the version people see (default: the
#                               workspace's, in Cargo.toml)
#   PORTPUTER_BUILD_NUMBER      the build number Sparkle compares (default: the
#                               number of commits, which only goes up)
#   PORTPUTER_ALLOW_LOCAL_HTTP  1 lets the feed be plain http on this computer,
#                               for testing updates
#
# PORTPUTER_SIGN_IDENTITY signs with a certificate; otherwise Portputer's own
# self-signed "Portputer Code Signing" (macos/make-certificate.sh makes it),
# then an Apple Development certificate, then ad hoc. macOS remembers the Accessibility and audio permissions by
# signature, so an ad-hoc build loses them every time it's rebuilt.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$(pwd)
PROFILE=release
CARGO_FLAGS=(--release)
if [[ "${1:-}" == "--debug" ]]; then
    PROFILE=debug
    CARGO_FLAGS=()
fi
CARGO="${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}"
# The built-in relay: relay/relay-url.txt, once the Worker is deployed.
if [[ -z "${PORTPUTER_RELAY_URL:-}" && -f relay/relay-url.txt ]]; then
    export PORTPUTER_RELAY_URL=$(tr -d '[:space:]' < relay/relay-url.txt)
fi

echo "Building the Rust core and the agent."
"$CARGO" build ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} -p portputer-ffi -p portputer-agent

echo "Generating the Swift bindings."
GEN="$ROOT/target/uniffi-swift"
rm -rf "$GEN"
"$CARGO" run ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} -q -p portputer-ffi --bin uniffi-bindgen -- \
    generate --library "target/$PROFILE/libportputer_ffi.a" --language swift --out-dir "$GEN"
mkdir -p macos/Sources/Portputer/Generated macos/Sources/portputer_ffiFFI/include
cp "$GEN/portputer_ffi.swift" macos/Sources/Portputer/Generated/
cp "$GEN/portputer_ffiFFI.h" macos/Sources/portputer_ffiFFI/include/
cat > macos/Sources/portputer_ffiFFI/include/module.modulemap <<'MAP'
module portputer_ffiFFI {
    header "portputer_ffiFFI.h"
    export *
}
MAP

echo "Building the app."
# Only the static library: with the dynamic one beside it, the linker would
# pick that.
LIBDIR="$ROOT/target/$PROFILE/static"
mkdir -p "$LIBDIR"
cp "target/$PROFILE/libportputer_ffi.a" "$LIBDIR/"
(cd macos && PORTPUTER_RUST_LIB_DIR="$LIBDIR" swift build -c "$PROFILE")
BIN=$(cd macos && PORTPUTER_RUST_LIB_DIR="$LIBDIR" swift build -c "$PROFILE" --show-bin-path)

echo "Putting Portputer.app together."
APP="$ROOT/macos/build/Portputer.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Resources"
cp "$BIN/Portputer" "$APP/Contents/MacOS/Portputer"
cp "target/$PROFILE/portputer-agent" "$APP/Contents/Helpers/portputer-agent"
cp macos/Support/Info.plist "$APP/Contents/Info.plist"
VERSION="${PORTPUTER_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
BUILD_NUMBER="${PORTPUTER_BUILD_NUMBER:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_NUMBER" "$APP/Contents/Info.plist"
FEED_URL="${PORTPUTER_FEED_URL:-https://raw.githubusercontent.com/aaron-gh/portputer/main/appcast.xml}"
PUBLIC_KEY="${PORTPUTER_ED_PUBLIC_KEY:-$(cat macos/Support/sparkle-public-key.txt 2>/dev/null || true)}"
/usr/libexec/PlistBuddy -c "Add :SUFeedURL string $FEED_URL" "$APP/Contents/Info.plist"
if [[ -n "$PUBLIC_KEY" ]]; then
    /usr/libexec/PlistBuddy -c "Add :SUPublicEDKey string $PUBLIC_KEY" "$APP/Contents/Info.plist"
fi
if [[ "${PORTPUTER_ALLOW_LOCAL_HTTP:-}" == 1 ]]; then
    /usr/libexec/PlistBuddy -c "Add :NSAppTransportSecurity dict" \
        -c "Add :NSAppTransportSecurity:NSAllowsLocalNetworking bool true" "$APP/Contents/Info.plist"
fi
mkdir -p "$APP/Contents/Frameworks"
SPARKLE=$(find macos/.build/artifacts -path '*Sparkle.xcframework/macos-*/Sparkle.framework' -maxdepth 6 | head -1)
cp -R "$SPARKLE" "$APP/Contents/Frameworks/"

SIGN_ID="${PORTPUTER_SIGN_IDENTITY:-}"
if [[ -z "$SIGN_ID" ]]; then
    DEV_ID=$(security find-identity -v -p codesigning | sed -n 's/.*"\(Apple Development: [^"]*\)".*/\1/p' | head -1)
    # Self-signed, so not "valid" to find-identity -v, but codesign uses it.
    if security find-identity -p codesigning | grep -q '"Portputer Code Signing"'; then
        SIGN_ID="Portputer Code Signing"
    elif [[ -n "$DEV_ID" ]]; then
        SIGN_ID="$DEV_ID"
    else
        SIGN_ID="-"
    fi
fi
echo "Building the Portputer Microphone driver."
macos/Driver/build.sh "$SIGN_ID"
cp -R macos/build/PortputerMicrophone.driver "$APP/Contents/Resources/"
# Sign from the inside out: Sparkle's helpers, its framework, the agent, then the app.
FW="$APP/Contents/Frameworks/Sparkle.framework"
for part in "$FW"/Versions/B/XPCServices/*.xpc "$FW/Versions/B/Autoupdate" "$FW/Versions/B/Updater.app" "$FW"; do
    if [[ -e "$part" ]]; then
        codesign --force --sign "$SIGN_ID" "$part" >/dev/null
    fi
done
codesign --force --sign "$SIGN_ID" --identifier io.github.aaron-gh.portputer.agent "$APP/Contents/Helpers/portputer-agent" >/dev/null
codesign --force --sign "$SIGN_ID" "$APP" >/dev/null
echo "Built $APP (signed: $SIGN_ID)"
