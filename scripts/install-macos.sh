#!/usr/bin/env bash
# Build "Anveg Code.app" and install it in /Applications.
#
#   scripts/install-macos.sh            # build, install, keep it closed
#   scripts/install-macos.sh --open     # ... and launch it afterwards
#   DEST=~/Applications scripts/install-macos.sh
#   UNIVERSAL=1 scripts/install-macos.sh   # Apple silicon + Intel
#
# The build is scripts/bundle-macos.sh (same options: UNIVERSAL, BUNDLE_ID).
# If Anveg Code is running from the install location, it is asked to quit
# first (it may ask whether to stop running sessions); the install stops if
# it is still running after a minute. Your settings, projects and notes
# live in ~/Library/Application Support/Anveg Code and are not touched.
set -euo pipefail
cd "$(dirname "$0")/.."

OPEN=0
for arg in "$@"; do
  case "$arg" in
    --open) OPEN=1 ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Unknown option: $arg (try --help)" >&2; exit 1 ;;
  esac
done

[ "$(uname -s)" = Darwin ] || { echo "Run this on macOS." >&2; exit 1; }

NAME="Anveg Code"
BUNDLE_ID=${BUNDLE_ID:-dev.anveg.code}
DEST=${DEST:-/Applications}
SRC="dist/$NAME.app"
TARGET="$DEST/$NAME.app"

if [ ! -w "$DEST" ] && [ -d "$DEST" ]; then
  echo "$DEST is not writable by $(whoami)." >&2
  echo "Install for this user only with:  DEST=~/Applications $0" >&2
  exit 1
fi
mkdir -p "$DEST"

echo "==> Building $NAME"
BUNDLE_ID="$BUNDLE_ID" scripts/bundle-macos.sh

running() {
  pgrep -f "$TARGET/Contents/MacOS/anveg-code" >/dev/null 2>&1
}

if running; then
  echo "==> $NAME is running; asking it to quit"
  osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
  for _ in $(seq 1 60); do
    running || break
    sleep 1
  done
  if running; then
    echo "$NAME is still running (a confirmation may be waiting). Quit it and run this again." >&2
    exit 1
  fi
fi

echo "==> Installing to $TARGET"
# Copy next to the old one first, then swap, so a failed copy leaves the
# installed app as it was. ditto keeps the signature and attributes.
TMP="$DEST/.$NAME.app.installing"
rm -rf "$TMP"
ditto "$SRC" "$TMP"
rm -rf "$TARGET"
mv "$TMP" "$TARGET"
xattr -dr com.apple.quarantine "$TARGET" 2>/dev/null || true

# Let Launch Services (Spotlight, Dock, "Open With") know about it.
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
[ -x "$LSREGISTER" ] && "$LSREGISTER" -f "$TARGET" >/dev/null 2>&1 || true

codesign --verify "$TARGET"
echo
echo "Installed $TARGET ($(du -sh "$TARGET" | cut -f1))"

if [ "$OPEN" = 1 ]; then
  open "$TARGET"
else
  echo "Open it from Launchpad or Spotlight, or:  open \"$TARGET\""
fi
