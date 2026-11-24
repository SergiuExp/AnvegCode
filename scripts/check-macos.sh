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
# The tree-sitter grammars (Phase 4) compile C. Without an Apple toolchain,
# use `zig cc` for the macOS target when zig is installed.
if ! command -v xcrun >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
  wrap=$(mktemp -d)
  zig_target=${TARGET%%-*}-macos
  # cc-rs passes its own --target/-arch (Apple spelling); zig wants its own.
  cat > "$wrap/cc" <<EOF
#!/bin/bash
args=()
skip=0
for a in "\$@"; do
  if [ \$skip = 1 ]; then skip=0; continue; fi
  case "\$a" in
    --target=*|-mmacosx-version-min=*) ;;
    -arch|-target) skip=1 ;;
    *) args+=("\$a") ;;
  esac
done
exec zig cc -target $zig_target "\${args[@]}"
EOF
  printf '#!/bin/sh\nexec zig ar "$@"\n' > "$wrap/ar"
  chmod +x "$wrap/cc" "$wrap/ar"
  t=$(echo "$TARGET" | tr '-' '_')
  export "CC_$t=$wrap/cc" "CXX_$t=$wrap/cc" "AR_$t=$wrap/ar"
fi
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
