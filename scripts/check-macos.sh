#!/usr/bin/env bash
# Type-check the app for macOS from a Linux machine (no linking).
#
# GPUI's macOS build script only runs on a macOS host. For `cargo check`
# nothing is linked, so an empty stitched shader file in its OUT_DIR is
# enough for `include_str!`. Development aid only; real builds happen on a Mac.
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET=${TARGET:-aarch64-apple-darwin}
rustup target add "$TARGET" >/dev/null
for _ in 1 2; do
  if cargo check --target "$TARGET" --workspace --all-targets "$@" 2>/tmp/anveg-check-macos.log; then
    tail -1 /tmp/anveg-check-macos.log
    exit 0
  fi
  missing=$(grep -o "target/$TARGET/[^\`]*/stitched_shaders.metal" /tmp/anveg-check-macos.log | head -1 || true)
  if [ -z "$missing" ]; then
    cat /tmp/anveg-check-macos.log >&2
    exit 1
  fi
  mkdir -p "$(dirname "$missing")"
  : > "$missing"
done
cat /tmp/anveg-check-macos.log >&2
exit 1
