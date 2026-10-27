#!/bin/bash
# Makes a Mac release of Portputer: builds the app, packs it in a disk image
# with an Applications shortcut, signs the disk image with Portputer's update
# key, and adds it to the update feed, appcast.xml at the top of the
# repository.
#
#   macos/release.sh [release-notes.html]
#
# Then attach the disk image to the GitHub release for this version, and
# commit and push appcast.xml; installed copies find the update there.
#
# Settings:
#   PORTPUTER_ED_KEY_FILE      the private update key, base64, if it isn't in
#                              the login Keychain as the "portputer" account
#   PORTPUTER_SIGN_IDENTITY    as for build.sh; a Developer ID certificate is
#                              also used for the disk image, for notarisation
#   PORTPUTER_NOTARY_PROFILE   a notarytool keychain profile; with it, the
#                              disk image is notarised and stapled
#   PORTPUTER_DOWNLOAD_URL     where the disk image will be published
#                              (default: the GitHub release for this version)
#   PORTPUTER_APPCAST          the feed to add the release to (default: appcast.xml)
#   PORTPUTER_VERSION, PORTPUTER_BUILD_NUMBER, PORTPUTER_FEED_URL: as for build.sh
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$(pwd)
NOTES="${1:-}"

macos/build.sh
APP="$ROOT/macos/build/Portputer.app"
VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$APP/Contents/Info.plist")
BUILD_NUMBER=$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$APP/Contents/Info.plist")
if ! /usr/libexec/PlistBuddy -c "Print :SUPublicEDKey" "$APP/Contents/Info.plist" >/dev/null 2>&1; then
    echo "The app has no public update key, so it couldn't check updates. Put it in macos/Support/sparkle-public-key.txt." >&2
    exit 1
fi

echo "Making the disk image."
DMG="$ROOT/macos/dist/Portputer-$VERSION.dmg"
macos/dmg.sh "$APP" "$DMG" "Portputer $VERSION"
if [[ "${PORTPUTER_SIGN_IDENTITY:-}" == "Developer ID Application"* ]]; then
    codesign --force --sign "$PORTPUTER_SIGN_IDENTITY" --timestamp "$DMG"
fi
if [[ -n "${PORTPUTER_NOTARY_PROFILE:-}" ]]; then
    echo "Notarising. This takes a few minutes."
    xcrun notarytool submit "$DMG" --keychain-profile "$PORTPUTER_NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG"
fi

echo "Signing the update."
SIGN_UPDATE=$(find macos/.build/artifacts -path '*/Sparkle/bin/sign_update' | head -1)
# Portputer's key is the "portputer" account in the login Keychain, made by
# generate_keys --account portputer.
KEY_ARGS=(--account portputer)
if [[ -n "${PORTPUTER_ED_KEY_FILE:-}" ]]; then
    KEY_ARGS=(--ed-key-file "$PORTPUTER_ED_KEY_FILE")
fi
# Prints: sparkle:edSignature="…" length="…"
SIGNATURE=$("$SIGN_UPDATE" "${KEY_ARGS[@]}" "$DMG")

URL="${PORTPUTER_DOWNLOAD_URL:-https://github.com/aaron-gh/portputer/releases/download/v$VERSION}/Portputer-$VERSION.dmg"
APPCAST="${PORTPUTER_APPCAST:-$ROOT/appcast.xml}"
python3 -I - "$APPCAST" "$VERSION" "$BUILD_NUMBER" "$URL" "$SIGNATURE" "$NOTES" <<'PY'
import email.utils, html, pathlib, sys
appcast, version, build, url, signature, notes = sys.argv[1:]
path = pathlib.Path(appcast)
description = ""
if notes:
    description = f"\n            <description><![CDATA[{pathlib.Path(notes).read_text()}]]></description>"
item = f"""        <item>
            <title>Version {html.escape(version)}</title>
            <pubDate>{email.utils.formatdate()}</pubDate>
            <sparkle:version>{html.escape(build)}</sparkle:version>
            <sparkle:shortVersionString>{html.escape(version)}</sparkle:shortVersionString>
            <sparkle:minimumSystemVersion>14.2</sparkle:minimumSystemVersion>{description}
            <enclosure url="{html.escape(url)}" {signature} type="application/octet-stream" />
        </item>
"""
text = path.read_text()
if f"<sparkle:version>{build}</sparkle:version>" in text:
    sys.exit(f"The feed already has build {build}. Commit first, or set PORTPUTER_BUILD_NUMBER.")
# Newest first: straight after the channel's own details.
marker = "        <language>en</language>\n"
if marker not in text:
    sys.exit("The feed isn't laid out as this script expects.")
path.write_text(text.replace(marker, marker + item, 1))
PY
echo "Made $DMG, version $VERSION, build $BUILD_NUMBER."
echo "Attach it to the v$VERSION release, then commit and push $(basename "$APPCAST")."
