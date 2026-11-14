#!/usr/bin/env bash
# Build release packages into dist/.
#   scripts/package.sh            macOS app + Windows exe (Windows is cross-compiled from macOS)
#   scripts/package.sh mac        Jon Motion.app   -> JonMotion-<version>-macos-<arch>.zip
#   scripts/package.sh windows    jon_motion.exe   -> JonMotion-<version>-windows-x64.zip
# UNIVERSAL=1 makes the macOS app run on both Apple Silicon and Intel
#   (needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin).
# Cross-compiling Windows needs: rustup target add x86_64-pc-windows-gnu && brew install mingw-w64
# macOS signing (optional; without it the app is ad-hoc signed and Gatekeeper warns on first open):
#   MACOS_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)"   sign with the hardened runtime
#   APPLE_ID, APPLE_APP_PASSWORD, APPLE_TEAM_ID                     also notarise and staple
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

WHAT=${1:-all}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
mkdir -p dist

package_mac() {
  local bin arch
  if [ "${UNIVERSAL:-0}" = 1 ]; then
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    bin=$(mktemp -d)/jon_motion
    lipo -create -output "$bin" target/aarch64-apple-darwin/release/jon_motion target/x86_64-apple-darwin/release/jon_motion
    arch=universal
  else
    cargo build --release
    bin=target/release/jon_motion
    arch=$(uname -m)
  fi
  local app="dist/Jon Motion.app"
  rm -rf "$app" && mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$bin" "$app/Contents/MacOS/jon_motion"
  local iconset
  iconset=$(mktemp -d)/icon.iconset && mkdir -p "$iconset"
  for s in 16 32 128 256 512; do
    sips -z $s $s assets/icon_1024.png --out "$iconset/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) assets/icon_1024.png --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$iconset" -o "$app/Contents/Resources/icon.icns"
  cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Jon Motion</string>
  <key>CFBundleDisplayName</key><string>Jon Motion</string>
  <key>CFBundleIdentifier</key><string>com.jonmotion.app</string>
  <key>CFBundleExecutable</key><string>jon_motion</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
  local zip="dist/JonMotion-$VERSION-macos-$arch.zip"
  rm -f "$zip"
  if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    codesign --force --options runtime --timestamp -s "$MACOS_SIGN_IDENTITY" "$app"
    codesign --verify --strict --verbose=2 "$app"
    if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
      # Apple checks a zip of the app; the ticket is then stapled to the app itself, so it opens offline too.
      local upload
      upload=$(mktemp -d)/notarize.zip
      ditto -c -k --keepParent "$app" "$upload"
      xcrun notarytool submit "$upload" --apple-id "$APPLE_ID" --password "$APPLE_APP_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
      xcrun stapler staple "$app"
    else
      echo "signed but NOT notarised (set APPLE_ID, APPLE_APP_PASSWORD and APPLE_TEAM_ID)" >&2
    fi
  else
    codesign --force --deep -s - "$app"
  fi
  ditto -c -k --keepParent "$app" "$zip"
}

package_windows() {
  local zip="$PWD/dist/JonMotion-$VERSION-windows-x64.zip"
  cargo build --release --target x86_64-pc-windows-gnu
  rm -f "$zip"
  (cd target/x86_64-pc-windows-gnu/release && zip -q "$zip" jon_motion.exe)
}

case "$WHAT" in
  mac) package_mac ;;
  windows) package_windows ;;
  all) package_mac; package_windows ;;
  *) echo "usage: $0 [mac|windows|all]" >&2; exit 2 ;;
esac
ls -lh dist/*.zip
