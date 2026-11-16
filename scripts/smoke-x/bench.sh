#!/usr/bin/env bash
# Measure real X CLI planning/IPC round trips in the current graphical session.
# Does not measure microphone latency, Decider network calls, or action execution.
set -euo pipefail
bin="${1:-hex}"
runs="${2:-50}"
if ! [[ "$runs" =~ ^[0-9]+$ ]] || ((runs < 10 || runs > 200)); then
  echo 'Usage: bench.sh [hex-binary] [10..200 runs, default 50]' >&2
  exit 2
fi
if ! command -v "$bin" >/dev/null 2>&1; then
  echo "Missing X/Hex executable: $bin" >&2
  exit 1
fi
if ! "$bin" agent status >/dev/null 2>&1; then
  echo 'X agent is unavailable. Start the service and enable X Labs first.' >&2
  exit 1
fi
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
for ((i=1; i<=runs; i++)); do
  start="$(date +%s%N)"
  if ! "$bin" agent say --mode actions 'list windows' >/dev/null 2>&1; then
    echo "X refused sample $i; verify that the agent is idle" >&2
    exit 1
  fi
  finish="$(date +%s%N)"
  printf '%s\n' "$(((finish-start)/1000000))" >> "$tmp"
  # Cancel any pending approval; this sample deliberately never acts on a window.
  "$bin" agent stop >/dev/null 2>&1 || true
  sleep 0.06
done
sort -n "$tmp" | awk -v n="$runs" '
{ values[NR] = $1 }
END {
  middle = int((n + 1) / 2)
  upper = int(n * 0.95 + 0.9999)
  if (upper > n) upper = n
  printf "X plan-only roundtrip: n=%d p50=%dms p95=%dms\n", n, values[middle], values[upper]
  print "Includes CLI startup and IPC. Excludes microphone, network Decider, and action execution."
}'
