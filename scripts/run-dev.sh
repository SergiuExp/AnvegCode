#!/usr/bin/env bash
# Build Anveg Code in debug mode (with the anveg-hook helper next to it, for
# live session status) and run it.
#
#   scripts/run-dev.sh                       # run with the Dev settings
#   ANVEG_HOME=/tmp/anveg scripts/run-dev.sh # run in a throwaway sandbox
#   RUST_LOG=debug scripts/run-dev.sh        # more logging
#
# Debug builds keep their settings in ~/Library/Application Support/Anveg
# Code Dev, apart from the installed app's. Arguments are passed to the app.
set -euo pipefail
cd "$(dirname "$0")/.."

case "${1:-}" in
  -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
esac

cargo build -p anveg_app -p anveg_hook
exec target/debug/anveg-code "$@"
