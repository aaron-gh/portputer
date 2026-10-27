#!/bin/bash
# Builds Portputer for Windows on the Mac, cross-compiled with MinGW-w64:
# Portputer.exe and portputer-agent.exe, in windows/dist/Portputer-<version>-windows-x64.zip.
#
# Needs MinGW-w64 (brew install mingw-w64) and Rust's Windows target, which
# this adds if it's missing. NVDA's controller client, nvdaControllerClient.dll,
# from NV Access, goes beside Portputer.exe for announcements through NVDA;
# put it in windows/ to have it included.
set -euo pipefail

cd "$(dirname "$0")/.."
CARGO="${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}"
RUSTUP="${RUSTUP:-$(command -v rustup || echo "$HOME/.cargo/bin/rustup")}"
TARGET=x86_64-pc-windows-gnu

# Homebrew's bin, for shells that don't have it on their path.
for dir in /opt/homebrew/bin /usr/local/bin; do
    [[ -d "$dir" && ":$PATH:" != *":$dir:"* ]] && PATH="$PATH:$dir"
done
if ! command -v x86_64-w64-mingw32-gcc >/dev/null; then
    echo "MinGW-w64 is needed to build for Windows. Install it with: brew install mingw-w64" >&2
    exit 1
fi
if ! "$RUSTUP" target list --installed | grep -qx "$TARGET"; then
    "$RUSTUP" target add "$TARGET"
fi

"$CARGO" build --release --target "$TARGET" -p portputer-windows -p portputer-agent

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
NAME="Portputer-$VERSION-windows-x64"
STAGE="windows/build/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE" windows/dist
cp "target/$TARGET/release/Portputer.exe" "target/$TARGET/release/portputer-agent.exe" "$STAGE/"
if [[ -f windows/nvdaControllerClient.dll ]]; then
    cp windows/nvdaControllerClient.dll "$STAGE/"
fi
(cd windows/build && rm -f "../dist/$NAME.zip" && zip -qr "../dist/$NAME.zip" "$NAME")
echo "Built windows/dist/$NAME.zip"
