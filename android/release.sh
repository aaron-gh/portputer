#!/bin/bash
# Makes an Android release of Portputer: builds it, signed with Portputer's
# Android key, as android/dist/Portputer-<version>.apk, and points the update
# feed, appcast-android.json at the top of the repository, at it.
#
#   android/release.sh [release-notes.txt]
#
# Then attach the APK to the GitHub release for this version, and commit and
# push appcast-android.json. Phones check it once a day.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$(pwd)
NOTES="${1:-}"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
export PORTPUTER_BUILD_NUMBER="${PORTPUTER_BUILD_NUMBER:-$(git rev-list --count HEAD)}"
source android/signing.sh
if [[ "$PORTPUTER_ANDROID_SIGNER" != portputer ]]; then
    echo "Portputer's Android signing key isn't here, so the release couldn't update installed copies. See android/make-signing-key.sh." >&2
    exit 1
fi
echo "Building Portputer for Android, build $PORTPUTER_BUILD_NUMBER."
(cd android && ./gradlew -q :app:assembleRelease)
APK="$ROOT/android/dist/Portputer-$VERSION.apk"
mkdir -p "$(dirname "$APK")"
cp android/app/build/outputs/apk/release/app-release.apk "$APK"
URL="https://github.com/aaron-gh/portputer/releases/download/v$VERSION/Portputer-$VERSION.apk"
python3 -I - "$ROOT/appcast-android.json" "$VERSION" "$PORTPUTER_BUILD_NUMBER" "$URL" "$APK" "$NOTES" <<'PY'
import hashlib, json, pathlib, sys
feed, version, build, url, apk, notes = sys.argv[1:]
digest = hashlib.sha256(pathlib.Path(apk).read_bytes()).hexdigest()
text = pathlib.Path(notes).read_text().strip() if notes else ""
pathlib.Path(feed).write_text(json.dumps({
    "version": version, "build": int(build), "url": url, "sha256": digest, "notes": text,
}, indent=2) + "\n")
PY
echo "Made $APK, version $VERSION, build $PORTPUTER_BUILD_NUMBER, and updated appcast-android.json."
echo "Attach it to the v$VERSION release, then commit and push appcast-android.json."
