#!/usr/bin/env bash
# Exercise the opted-in KWin D-Bus adapter on a disposable virtual Plasma 6 desktop.
set -Eeuo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
for tool in cargo dbus-run-session gdbus kdotool kwin_wayland python3; do
  if ! command -v "$tool" >/dev/null; then
    echo "KDE voice smoke requires $tool." >&2
    exit 1
  fi
done
scratch="$(mktemp -d /tmp/xk.XXXXXX)"
trap 'rm -rf -- "$scratch"' EXIT
cd "$repo"
cargo test --locked -p x-agent --test kde_voice --no-run --message-format=json > "$scratch/build.json"
binary="$(python3 - "$scratch/build.json" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as messages:
    for line in messages:
        item = json.loads(line)
        if item.get("executable") and item.get("target", {}).get("name") == "kde_voice":
            print(item["executable"])
            break
    else:
        sys.exit("No KDE test executable found")
PY
)"
unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET HYPRLAND_INSTANCE_SIGNATURE SWAYSOCK I3SOCK AT_SPI_BUS_ADDRESS
unset XDG_SEAT XDG_SESSION_ID XDG_VTNR X_ENABLE_EXPERIMENTAL_KDE
export HOME="$scratch/home" XDG_RUNTIME_DIR="$scratch/run" XDG_CONFIG_HOME="$scratch/config"
export XDG_CACHE_HOME="$scratch/cache" XDG_DATA_HOME="$scratch/data" XDG_STATE_HOME="$scratch/state"
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=KDE KDE_SESSION_VERSION=6
mkdir -m 700 "$HOME" "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
cat > "$XDG_CONFIG_HOME/kwinrc" <<'CONF'
[Desktops]
Number=3
Rows=1
CONF
dbus-run-session -- bash -s -- "$scratch" "$binary" <<'SESSION'
set -Eeuo pipefail
kwin_wayland --virtual --socket wayland-kde --no-lockscreen --no-global-shortcuts > "$1/kwin.log" 2>&1 &
kwin=$!
trap 'kill "$kwin" 2>/dev/null || true; wait "$kwin" 2>/dev/null || true' EXIT
if ! gdbus wait --session --timeout 5 org.kde.KWin; then cat "$1/kwin.log" >&2; exit 1; fi
export WAYLAND_DISPLAY=wayland-kde X_TEST_ISOLATED_KDE=1
"$2" --ignored --exact kde_requires_session_opt_in
export X_ENABLE_EXPERIMENTAL_KDE=1
"$2" --ignored --exact kde_verifies_workspace_switch_after_typed_approval
if [[ -n "${X_TEST_VOICE_BENCH_RUNS:-}" ]]; then
  "$2" --ignored --exact offline_voice_path_latency --nocapture
fi
SESSION
