#!/usr/bin/env bash
# Build "Anveg Code.app" (build plan, M9) and sign it for local use.
#
#   scripts/bundle-macos.sh              # this Mac's architecture
#   UNIVERSAL=1 scripts/bundle-macos.sh  # Apple silicon + Intel (lipo)
#   BUNDLE_ID=com.example.anveg scripts/bundle-macos.sh
#   SIGN_IDENTITY=- scripts/bundle-macos.sh   # ad hoc (default: Apple Development)
#
# Output: dist/Anveg Code.app (with Contents/MacOS/anveg-code and the
# anveg-hook helper next to it, where the app looks for it).
set -euo pipefail
cd "$(dirname "$0")/.."

[ "$(uname -s)" = Darwin ] || { echo "Run this on macOS." >&2; exit 1; }

NAME="Anveg Code"
# Placeholder identifier: confirm the real one before distributing.
BUNDLE_ID=${BUNDLE_ID:-dev.anveg.code}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP="dist/$NAME.app"

build() { # target-triple or empty
  if [ -n "${1:-}" ]; then
    cargo build --release --locked --target "$1" -p anveg_app -p anveg_hook
    echo "target/$1/release"
  else
    cargo build --release --locked -p anveg_app -p anveg_hook
    echo "target/release"
  fi
}

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

if [ "${UNIVERSAL:-0}" = 1 ]; then
  arm=$(build aarch64-apple-darwin | tail -1)
  intel=$(build x86_64-apple-darwin | tail -1)
  for bin in anveg-code anveg-hook; do
    lipo -create -output "$APP/Contents/MacOS/$bin" "$arm/$bin" "$intel/$bin"
  done
else
  out=$(build | tail -1)
  cp "$out/anveg-code" "$out/anveg-hook" "$APP/Contents/MacOS/"
fi

# The app icon ("Session Orbit"; source: assets/icon/AppIcon.svg).
cp assets/icon/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundleDisplayName</key><string>$NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleExecutable</key><string>anveg-code</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

# Signature for local use (not notarized): the helper first, then the
# bundle. A real identity (the first "Apple Development" one, or
# SIGN_IDENTITY) keeps the designated requirement the same across builds,
# so the Keychain's "Always Allow" survives updates; an ad-hoc signature
# (SIGN_IDENTITY=-, or no identity found) is tied to this build's hash.
if [ -z "${SIGN_IDENTITY:-}" ]; then
  SIGN_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -n 's/^ *[0-9]*) \([0-9A-F]\{40\}\) "Apple Development: .*"$/\1/p' | head -1)
  SIGN_IDENTITY=${SIGN_IDENTITY:--}
fi
[ "$SIGN_IDENTITY" = - ] && echo "Signing ad hoc: the Keychain will ask again after each build." >&2
codesign --force --timestamp=none --sign "$SIGN_IDENTITY" "$APP/Contents/MacOS/anveg-hook"
codesign --force --timestamp=none --sign "$SIGN_IDENTITY" "$APP"
codesign --verify --verbose=1 "$APP"

echo
echo "Built $APP ($(du -sh "$APP" | cut -f1))"
echo "Run it with: open \"$APP\"   (or drag it to /Applications)"
