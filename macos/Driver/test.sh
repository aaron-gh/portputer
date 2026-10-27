#!/bin/bash
# Loads the built driver the way coreaudiod does and checks its devices,
# properties and loopback, without installing it.
#   macos/Driver/test.sh
set -euo pipefail
cd "$(dirname "$0")"
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT
clang -Wall -framework CoreAudio -framework CoreFoundation -o "$OUT/test" test.c
"$OUT/test" ../build/PortputerMicrophone.driver
