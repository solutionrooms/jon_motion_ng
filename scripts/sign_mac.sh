#!/usr/bin/env bash
# Sign (and notarise) a macOS package built by scripts/package.sh, replacing the zip in place.
#   MACOS_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" scripts/sign_mac.sh dist/JonMotion-<version>-macos-<arch>.zip
# With APPLE_ID, APPLE_APP_PASSWORD (an app-specific password) and APPLE_TEAM_ID also set, the app
# is notarised and the ticket stapled, so macOS opens it without a warning.
set -euo pipefail
zip=${1:?usage: sign_mac.sh <package.zip>}
: "${MACOS_SIGN_IDENTITY:?set MACOS_SIGN_IDENTITY to a 'Developer ID Application' identity}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
ditto -x -k "$zip" "$work"
app="$work/Jon Motion.app"
[ -d "$app" ] || { echo "$zip does not contain Jon Motion.app" >&2; exit 1; }

codesign --force --options runtime --timestamp -s "$MACOS_SIGN_IDENTITY" "$app"
codesign --verify --strict --verbose=2 "$app"

if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  # Apple checks a zip of the app; the ticket is then stapled to the app itself, so it opens offline too.
  ditto -c -k --keepParent "$app" "$work/notarize.zip"
  xcrun notarytool submit "$work/notarize.zip" --apple-id "$APPLE_ID" --password "$APPLE_APP_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
  xcrun stapler staple "$app"
else
  echo "signed but NOT notarised (set APPLE_ID, APPLE_APP_PASSWORD and APPLE_TEAM_ID)" >&2
fi

rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "signed: $zip"
