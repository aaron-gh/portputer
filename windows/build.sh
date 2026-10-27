#!/bin/bash
# Builds Portputer for Windows on the Mac, cross-compiled with MinGW-w64,
# with NVDA's controller client and WinSparkle, as an installer and a zip.
#   windows/build.sh    makes windows/dist/Portputer-<version>-windows-x64-setup.exe
#                       and windows/dist/Portputer-<version>-windows-x64.zip
#
#   PORTPUTER_BUILD_NUMBER  the number updates are compared by (default: the
#                           number of commits, which only goes up, as on the Mac)
#
# Needs MinGW-w64 and NSIS (brew install mingw-w64 makensis) and Rust's
# Windows target, which this adds if it's missing.
set -euo pipefail

cd "$(dirname "$0")/.."
CARGO="${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}"
RUSTUP="${RUSTUP:-$(command -v rustup || echo "$HOME/.cargo/bin/rustup")}"
TARGET=x86_64-pc-windows-gnu
# The built-in relay: relay/relay-url.txt, once the Worker is deployed.
if [[ -z "${PORTPUTER_RELAY_URL:-}" && -f relay/relay-url.txt ]]; then
    export PORTPUTER_RELAY_URL=$(tr -d '[:space:]' < relay/relay-url.txt)
fi

# Homebrew's bin, for shells that don't have it on their path.
for dir in /opt/homebrew/bin /usr/local/bin; do
    [[ -d "$dir" && ":$PATH:" != *":$dir:"* ]] && PATH="$PATH:$dir"
done
if ! command -v x86_64-w64-mingw32-gcc >/dev/null; then
    echo "MinGW-w64 is needed to build for Windows. Install it with: brew install mingw-w64" >&2
    exit 1
fi
if ! command -v makensis >/dev/null; then
    echo "NSIS is needed to make the installer. Install it with: brew install makensis" >&2
    exit 1
fi
if ! "$RUSTUP" target list --installed | grep -qx "$TARGET"; then
    "$RUSTUP" target add "$TARGET"
fi

export PORTPUTER_BUILD_NUMBER="${PORTPUTER_BUILD_NUMBER:-$(git rev-list --count HEAD 2>/dev/null || echo 0)}"
echo "Building the Windows app, build $PORTPUTER_BUILD_NUMBER."
"$CARGO" build --release --target "$TARGET" -p portputer-windows -p portputer-agent

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
NAME="Portputer-$VERSION-windows-x64"
STAGE="windows/build/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE" windows/dist windows/build/cache
cp "target/$TARGET/release/Portputer.exe" "target/$TARGET/release/portputer-agent.exe" "$STAGE/"

# NV Access's NVDA controller client, which Portputer speaks through when
# NVDA is running. Pinned, and checked against its known checksum.
NVDA_VERSION=2026.2
NVDA_SHA256=510736f021aefa33378076fa342a4524b586bc1c6b096db9479af42b28aac649
NVDA_ZIP="windows/build/cache/nvda_${NVDA_VERSION}_controllerClient.zip"
if [[ ! -f "$NVDA_ZIP" ]]; then
    echo "Downloading NVDA's controller client $NVDA_VERSION."
    curl -fsSL -o "$NVDA_ZIP.part" \
        "https://download.nvaccess.org/releases/$NVDA_VERSION/nvda_${NVDA_VERSION}_controllerClient.zip"
    mv "$NVDA_ZIP.part" "$NVDA_ZIP"
fi
if [[ "$(shasum -a 256 "$NVDA_ZIP" | cut -d' ' -f1)" != "$NVDA_SHA256" ]]; then
    echo "NVDA's controller client doesn't match its checksum. Delete $NVDA_ZIP and try again." >&2
    exit 1
fi
unzip -qjo "$NVDA_ZIP" x64/nvdaControllerClient.dll -d "$STAGE"
unzip -qpo "$NVDA_ZIP" license.txt | sed 's/$/\r/' > "$STAGE/nvdaControllerClient-LICENSE.txt"

# WinSparkle, which updates Portputer. Pinned and checked in the same way.
WINSPARKLE_VERSION=0.9.4
WINSPARKLE_SHA256=6037df37fc263bd1650a1c4949681a9d40ffe991d01f35892a406cb5d103c976
WINSPARKLE_ZIP="windows/build/cache/WinSparkle-$WINSPARKLE_VERSION.zip"
if [[ ! -f "$WINSPARKLE_ZIP" ]]; then
    echo "Downloading WinSparkle $WINSPARKLE_VERSION."
    curl -fsSL -o "$WINSPARKLE_ZIP.part" \
        "https://github.com/vslavik/winsparkle/releases/download/v$WINSPARKLE_VERSION/WinSparkle-$WINSPARKLE_VERSION.zip"
    mv "$WINSPARKLE_ZIP.part" "$WINSPARKLE_ZIP"
fi
if [[ "$(shasum -a 256 "$WINSPARKLE_ZIP" | cut -d' ' -f1)" != "$WINSPARKLE_SHA256" ]]; then
    echo "WinSparkle doesn't match its checksum. Delete $WINSPARKLE_ZIP and try again." >&2
    exit 1
fi
unzip -qjo "$WINSPARKLE_ZIP" "WinSparkle-$WINSPARKLE_VERSION/x64/Release/WinSparkle.dll" -d "$STAGE"
unzip -qpo "$WINSPARKLE_ZIP" "WinSparkle-$WINSPARKLE_VERSION/COPYING" | sed 's/$/\r/' > "$STAGE/WinSparkle-LICENSE.txt"

# Windows line endings, so Notepad shows it properly.
sed 's/$/\r/' LICENSE > "$STAGE/LICENSE.txt"

ZIP="$PWD/windows/dist/$NAME.zip"
rm -f "$ZIP"
(cd windows/build && zip -qr "$ZIP" "$NAME")
echo "Built $ZIP"

SETUP="$PWD/windows/dist/$NAME-setup.exe"
# makensis needs a UTF-8 locale; without one it fails with bad_alloc.
LC_ALL=en_US.UTF-8 makensis -V2 -DVERSION="$VERSION" -DVERSION_NUMBERS="$(echo "$VERSION" | sed 's/[^0-9.].*//')" \
    -DBUILD="$PORTPUTER_BUILD_NUMBER" -DSTAGE="$PWD/$STAGE" -DOUTFILE="$SETUP" windows/installer.nsi
echo "Built $SETUP"
