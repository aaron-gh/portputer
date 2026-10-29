#!/bin/bash
# Builds the Rust core for iOS into ios/Frameworks/PortputerFFI.xcframework,
# generates the Swift bindings, the Xcode project (XcodeGen), and the app.
#   ios/build.sh                 release build for an iPhone
#   ios/build.sh --debug         debug build, quicker to compile
#   ios/build.sh --simulator     for the simulator (with --debug too)
#   ios/build.sh --rust-only     the Rust core and bindings only, for Xcode
#
# Signing is automatic, with the team in project.yml. PORTPUTER_VERSION and
# PORTPUTER_BUILD_NUMBER set the version and build number, as on the Mac.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$(pwd)
PROFILE=release
CARGO_FLAGS=(--release)
CONFIG=Release
SIMULATOR=0
RUST_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --debug) PROFILE=debug; CARGO_FLAGS=(); CONFIG=Debug ;;
        --simulator) SIMULATOR=1 ;;
        --rust-only) RUST_ONLY=1 ;;
        *) echo "Unknown option $arg" >&2; exit 1 ;;
    esac
done
CARGO="${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}"
# The built-in relay: relay/relay-url.txt, once the Worker is deployed.
if [[ -z "${PORTPUTER_RELAY_URL:-}" && -f relay/relay-url.txt ]]; then
    export PORTPUTER_RELAY_URL=$(tr -d '[:space:]' < relay/relay-url.txt)
fi
export IPHONEOS_DEPLOYMENT_TARGET=17.0

echo "Building the Rust core for iOS."
"$CARGO" build ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} -q -p portputer-ffi --lib --target aarch64-apple-ios
"$CARGO" build ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} -q -p portputer-ffi --lib --target aarch64-apple-ios-sim

echo "Generating the Swift bindings."
# From this computer's build of the library; the interface is the same.
"$CARGO" build ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} -q -p portputer-ffi --lib
GEN="$ROOT/target/uniffi-swift-ios"
rm -rf "$GEN"
"$CARGO" run ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} -q -p portputer-ffi --bin uniffi-bindgen -- \
    generate --library "target/$PROFILE/libportputer_ffi.a" --language swift --out-dir "$GEN"
mkdir -p ios/Sources/Portputer/Generated
cp "$GEN/portputer_ffi.swift" ios/Sources/Portputer/Generated/
HEADERS="$GEN/headers"
mkdir -p "$HEADERS"
cp "$GEN/portputer_ffiFFI.h" "$HEADERS/"
cat > "$HEADERS/module.modulemap" <<'MAP'
module portputer_ffiFFI {
    header "portputer_ffiFFI.h"
    export *
}
MAP

XCFW="$ROOT/ios/Frameworks/PortputerFFI.xcframework"
rm -rf "$XCFW"
xcodebuild -create-xcframework \
    -library "target/aarch64-apple-ios/$PROFILE/libportputer_ffi.a" -headers "$HEADERS" \
    -library "target/aarch64-apple-ios-sim/$PROFILE/libportputer_ffi.a" -headers "$HEADERS" \
    -output "$XCFW" >/dev/null
[[ "$RUST_ONLY" == 1 ]] && exit 0

echo "Generating the Xcode project."
VERSION="${PORTPUTER_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
BUILD_NUMBER="${PORTPUTER_BUILD_NUMBER:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}"
(cd ios && PORTPUTER_VERSION="$VERSION" PORTPUTER_BUILD_NUMBER="$BUILD_NUMBER" xcodegen generate --quiet)

echo "Building the app."
if [[ "$SIMULATOR" == 1 ]]; then
    DEST="generic/platform=iOS Simulator"
else
    DEST="generic/platform=iOS"
fi
xcodebuild -project ios/Portputer.xcodeproj -scheme Portputer -configuration "$CONFIG" \
    -destination "$DEST" -derivedDataPath ios/build -allowProvisioningUpdates -quiet build
APP=$(find ios/build/Build/Products -maxdepth 2 -name Portputer.app | head -1)
echo "Built $APP"
