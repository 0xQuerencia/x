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
if ! command -v python3 >/dev/null 2>&1; then
  echo 'python3 is required to check the exact task returned by X.' >&2
  exit 1
fi
tmp="$(mktemp)"
snapshot="$(mktemp)"
active_id=""
cleanup() {
  if [[ -n "$active_id" ]]; then
    "$bin" agent stop --task "$active_id" >/dev/null 2>&1 || true
  fi
  rm -f "$tmp" "$snapshot"
}
trap cleanup EXIT
if ! "$bin" agent status --json > "$snapshot" || ! python3 - "$snapshot" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
state = data.get("snapshot", {})
sys.exit(0 if data.get("status") == "agent" and state.get("phase") in
         {"idle", "completed", "failed", "cancelled"} and
         state.get("trusted_actions") is False else 1)
PY
then
  echo 'Start X with Labs enabled, leave the foreground idle, and use the default approval policy.' >&2
  exit 1
fi
for ((i=1; i<=runs; i++)); do
  start="$(date +%s%N)"
  if ! "$bin" agent say --mode actions --json 'list windows' > "$snapshot"; then
    echo "X refused sample $i; verify that the agent is idle" >&2
    exit 1
  fi
  finish="$(date +%s%N)"
  if ! active_id="$(python3 - "$snapshot" <<'PY'
import json, sys
reply = json.load(open(sys.argv[1], encoding="utf-8"))
state = reply.get("snapshot", {})
task = state.get("task") or {}
if (reply.get("status") != "agent" or state.get("phase") != "awaiting_approval"
        or task.get("text") != "list windows" or type(task.get("id")) is not int):
    sys.exit(1)
print(task["id"])
PY
)"; then
    echo 'X did not return the expected unexecuted preview; no task was stopped.' >&2
    exit 1
  fi
  printf '%s\n' "$(((finish-start)/1000000))" >> "$tmp"
  # Discard only this sample's exact preview. Never cancel unrelated work.
  if ! "$bin" agent stop --task "$active_id" >/dev/null 2>&1; then
    echo 'The sample task changed before cleanup; no replacement task was stopped.' >&2
    exit 1
  fi
  active_id=""
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
