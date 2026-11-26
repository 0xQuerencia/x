#!/usr/bin/env bash
# Run X's finished-voice acceptance test on Hyprland nested in a virtual KWin desktop.
set -Eeuo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
for tool in cargo dbus-run-session kwin_wayland Hyprland hyprctl python3; do
  if ! command -v "$tool" >/dev/null; then
    echo "Hyprland voice smoke requires $tool." >&2
    exit 1
  fi
done
# Keep the runtime path short enough for Hyprland's long instance socket names.
scratch="$(mktemp -d /tmp/xh.XXXXXX)"
trap 'rm -rf -- "$scratch"' EXIT
# Compile outside the temporary home. The compositor and tests never inherit the host home.
cd "$repo"
cargo test --locked -p x-agent --test hyprland_voice --no-run --message-format=json > "$scratch/build.json"
binary="$(python3 - "$scratch/build.json" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as messages:
    for line in messages:
        item = json.loads(line)
        if item.get("executable") and item.get("target", {}).get("name") == "hyprland_voice":
            print(item["executable"])
            break
    else:
        sys.exit("No Hyprland test executable found")
PY
)"
dbus-run-session -- bash -s -- "$scratch" "$binary" <<'INNER'
set -Eeuo pipefail
scratch="$1"
binary="$2"
kwin=""
hypr=""
cleanup() {
  if [[ -n "$hypr" ]]; then kill "$hypr" 2>/dev/null || true; wait "$hypr" 2>/dev/null || true; fi
  if [[ -n "$kwin" ]]; then kill "$kwin" 2>/dev/null || true; wait "$kwin" 2>/dev/null || true; fi
}
trap cleanup EXIT
export HOME="$scratch/home" XDG_RUNTIME_DIR="$scratch/r" XDG_CONFIG_HOME="$scratch/config"
export XDG_CACHE_HOME="$scratch/cache" XDG_DATA_HOME="$scratch/data" XDG_STATE_HOME="$scratch/state"
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=Hyprland
mkdir -m 700 "$HOME" "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET HYPRLAND_INSTANCE_SIGNATURE SWAYSOCK I3SOCK
unset XDG_SEAT XDG_SESSION_ID XDG_VTNR
kwin_wayland --virtual --socket wayland-parent --width 1024 --height 768 \
  --no-lockscreen --no-global-shortcuts > "$scratch/kwin.log" 2>&1 &
kwin="$!"
for i in {1..80}; do
  [[ ! -S "$XDG_RUNTIME_DIR/wayland-parent" ]] || break
  kill -0 "$kwin" 2>/dev/null || break
  sleep 0.1
done
if [[ ! -S "$XDG_RUNTIME_DIR/wayland-parent" ]]; then cat "$scratch/kwin.log" >&2; exit 1; fi
cat > "$scratch/hypr.lua" <<'LUA'
hl.config({ xwayland = { enabled = false } })
hl.monitor({ output = "", mode = "preferred", position = "auto", scale = 1 })
LUA
WAYLAND_DISPLAY=wayland-parent Hyprland --config "$scratch/hypr.lua" > "$scratch/hypr.log" 2>&1 &
hypr="$!"
ready=0
shopt -s nullglob
for i in {1..80}; do
  kill -0 "$hypr" 2>/dev/null || break
  for socket in "$XDG_RUNTIME_DIR"/hypr/*/.socket.sock; do
    [[ -S "$socket" ]] || continue
    instance="${socket%/*}"
    export HYPRLAND_INSTANCE_SIGNATURE="${instance##*/}"
    if hyprctl -j activeworkspace | python3 -c 'import json, sys; sys.exit(json.load(sys.stdin).get("id") != 1)' 2>/dev/null; then
      ready=1
      break 2
    fi
  done
  sleep 0.1
done
if [[ "$ready" != 1 ]]; then cat "$scratch/hypr.log" >&2; exit 1; fi
X_TEST_ISOLATED_HYPRLAND=1 "$binary" --ignored --test-threads=1 --nocapture
INNER
