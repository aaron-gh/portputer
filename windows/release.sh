#!/bin/bash
# Makes a Windows release of Portputer: builds the installer, signs it with
# Portputer's update key (the Mac's, in the login Keychain), and adds it to
# the Windows update feed, appcast-windows.xml at the top of the repository.
#
#   windows/release.sh [release-notes.html]
#
# Then attach the installer to the GitHub release for this version, and
# commit and push appcast-windows.xml; WinSparkle finds the update there.
#
# Settings:
#   PORTPUTER_ED_KEY_FILE   the private update key, base64, if it isn't in
#                           the login Keychain as the "portputer" account
#   PORTPUTER_DOWNLOAD_URL  where the installer will be published (default:
#                           the GitHub release for this version)
#   PORTPUTER_APPCAST       the feed to add the release to (default: appcast-windows.xml)
#   PORTPUTER_BUILD_NUMBER  as for build.sh
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$(pwd)
NOTES="${1:-}"

export PORTPUTER_BUILD_NUMBER="${PORTPUTER_BUILD_NUMBER:-$(git rev-list --count HEAD)}"
windows/build.sh
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
SETUP="$ROOT/windows/dist/Portputer-$VERSION-windows-x64-setup.exe"

echo "Signing the update."
SIGN_UPDATE=$(find macos/.build/artifacts -path '*/Sparkle/bin/sign_update' 2>/dev/null | head -1)
if [[ -z "$SIGN_UPDATE" ]]; then
    echo "Sparkle's sign_update isn't here yet. Build the Mac app once with macos/build.sh, which fetches it." >&2
    exit 1
fi
KEY_ARGS=(--account portputer)
if [[ -n "${PORTPUTER_ED_KEY_FILE:-}" ]]; then
    KEY_ARGS=(--ed-key-file "$PORTPUTER_ED_KEY_FILE")
fi
# Prints: sparkle:edSignature="…" length="…"
SIGNATURE=$("$SIGN_UPDATE" "${KEY_ARGS[@]}" "$SETUP")

URL="${PORTPUTER_DOWNLOAD_URL:-https://github.com/aaron-gh/portputer/releases/download/v$VERSION}/$(basename "$SETUP")"
APPCAST="${PORTPUTER_APPCAST:-$ROOT/appcast-windows.xml}"
python3 -I - "$APPCAST" "$VERSION" "$PORTPUTER_BUILD_NUMBER" "$URL" "$SIGNATURE" "$NOTES" <<'PY'
import email.utils, html, pathlib, sys
appcast, version, build, url, signature, notes = sys.argv[1:]
path = pathlib.Path(appcast)
text = path.read_text()
if f"<sparkle:version>{build}</sparkle:version>" in text:
    sys.exit(f"The feed already has build {build}. Commit first, or set PORTPUTER_BUILD_NUMBER.")
description = ""
if notes:
    description = f"\n            <description><![CDATA[{pathlib.Path(notes).read_text()}]]></description>"
# The installer runs quietly (/S) and starts Portputer again when it's done.
item = f"""        <item>
            <title>Version {html.escape(version)}</title>
            <pubDate>{email.utils.formatdate()}</pubDate>
            <sparkle:version>{html.escape(build)}</sparkle:version>
            <sparkle:shortVersionString>{html.escape(version)}</sparkle:shortVersionString>{description}
            <enclosure url="{html.escape(url)}" {signature} type="application/octet-stream" sparkle:os="windows-x64" sparkle:installerArguments="/S" />
        </item>
"""
# Newest first: straight after the channel's own details.
marker = "        <language>en</language>\n"
if marker not in text:
    sys.exit("The feed isn't laid out as this script expects.")
path.write_text(text.replace(marker, marker + item, 1))
PY
echo "Made $SETUP, version $VERSION, build $PORTPUTER_BUILD_NUMBER."
echo "Attach it to the v$VERSION release, then commit and push $(basename "$APPCAST")."
