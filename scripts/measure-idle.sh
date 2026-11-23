#!/usr/bin/env bash
# Report Anveg Code's CPU and memory while idle (build plan, 1.2 and 10).
# Usage: scripts/measure-idle.sh [seconds]   (default 60)
# Start the app first, open the windows and sessions to measure, leave it
# alone, then run this. Memory excludes the `claude` child processes.
set -euo pipefail
secs=${1:-60}
pid=$(pgrep -x anveg-code | head -1 || true)
[ -n "$pid" ] || { echo "anveg-code is not running" >&2; exit 1; }
cpu_ticks() { # user+system CPU time of the process, in seconds
  if [ -r "/proc/$pid/stat" ]; then
    awk -v hz="$(getconf CLK_TCK)" '{print ($14+$15)/hz}' "/proc/$pid/stat"
  else
    ps -o time= -p "$pid" | awk -F'[:.]' '{ if (NF==4) print $1*3600+$2*60+$3+$4/100; else print $1*60+$2+$3/100 }'
  fi
}
start=$(cpu_ticks)
sleep "$secs"
end=$(cpu_ticks)
rss_kb=$(ps -o rss= -p "$pid" | tr -d ' ')
awk -v s="$start" -v e="$end" -v t="$secs" -v r="$rss_kb" 'BEGIN {
  printf "idle CPU over %ds: %.2f%%\n", t, (e-s)/t*100
  printf "app memory (RSS): %.0f MB\n", r/1024
}'
