#!/bin/bash
# Builds PortputerMicrophone.driver, the Audio Server plug-in, into
# macos/build/, signed like the app (see macos/build.sh).
#   macos/Driver/build.sh [signing identity]
set -euo pipefail

cd "$(dirname "$0")"
OUT=../build/PortputerMicrophone.driver
rm -rf "$OUT"
mkdir -p "$OUT/Contents/MacOS"
cp Info.plist "$OUT/Contents/Info.plist"
clang -bundle -O2 -Wall -Wextra -Werror -arch arm64 -arch x86_64 -mmacosx-version-min=14.0 \
    -framework CoreAudio -framework CoreFoundation \
    -o "$OUT/Contents/MacOS/PortputerMicrophone" PortputerMicrophone.c
codesign --force --sign "${1:--}" "$OUT" >/dev/null
echo "Built macos/build/PortputerMicrophone.driver"
