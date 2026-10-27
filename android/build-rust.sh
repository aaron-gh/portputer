#!/bin/bash
# Builds the Rust core for Android (arm64 and x86_64) into the app's
# jniLibs, and generates its Kotlin bindings.
#   android/build-rust.sh            release build
#   android/build-rust.sh --debug    debug build
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
SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
NDK="${ANDROID_NDK_HOME:-$(ls -d "$SDK"/ndk/* | sort -V | tail -1)}"
HOST_TAG=$(ls "$NDK/toolchains/llvm/prebuilt" | head -1)
BIN="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin"
API=26
APP="$ROOT/android/app/src/main"
# The built-in relay: relay/relay-url.txt, once the Worker is deployed.
if [[ -z "${PORTPUTER_RELAY_URL:-}" && -f relay/relay-url.txt ]]; then
    export PORTPUTER_RELAY_URL=$(tr -d '[:space:]' < relay/relay-url.txt)
fi

build() {
    local triple=$1 abi=$2 clang_triple=$3
    local upper
    upper=$(echo "$triple" | tr 'a-z-' 'A-Z_')
    export "CARGO_TARGET_${upper}_LINKER=$BIN/${clang_triple}${API}-clang"
    export "CC_${triple//-/_}=$BIN/${clang_triple}${API}-clang"
    export "AR_${triple//-/_}=$BIN/llvm-ar"
    # 16 KB pages, which Android 15 devices can use.
    export "CARGO_TARGET_${upper}_RUSTFLAGS=-C link-arg=-Wl,-z,max-page-size=16384 -C strip=symbols"
    "$CARGO" build ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} -q -p portputer-ffi --lib --target "$triple"
    mkdir -p "$APP/jniLibs/$abi"
    cp "target/$triple/$PROFILE/libportputer_ffi.so" "$APP/jniLibs/$abi/"
}

build aarch64-linux-android arm64-v8a aarch64-linux-android
build x86_64-linux-android x86_64 x86_64-linux-android

# From this computer's build of the library: the Android ones are stripped of
# the metadata UniFFI reads, and the interface is the same.
"$CARGO" build ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} -q -p portputer-ffi --lib
"$CARGO" run ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} -q -p portputer-ffi --bin uniffi-bindgen -- \
    generate --library "target/$PROFILE/libportputer_ffi.a" \
    --language kotlin --no-format --out-dir "$APP/java"
