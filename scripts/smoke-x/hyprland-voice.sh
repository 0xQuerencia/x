#!/usr/bin/env bash
# Run X's finished-voice acceptance test on Hyprland nested in a virtual KWin desktop.
set -Eeuo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
for tool in cargo dbus-daemon dbus-run-session gdbus kwin_wayland Hyprland hyprctl python3; do
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
# D-Bus activation inherits this environment, so isolate it before starting the bus.
unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET HYPRLAND_INSTANCE_SIGNATURE SWAYSOCK I3SOCK AT_SPI_BUS_ADDRESS
unset XDG_SEAT XDG_SESSION_ID XDG_VTNR
export HOME="$scratch/home" XDG_RUNTIME_DIR="$scratch/r" XDG_CONFIG_HOME="$scratch/config"
export XDG_CACHE_HOME="$scratch/cache" XDG_DATA_HOME="$scratch/data" XDG_STATE_HOME="$scratch/state"
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=Hyprland
mkdir -m 700 "$HOME" "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
dbus-run-session -- bash -s -- "$scratch" "$binary" <<'INNER'
set -Eeuo pipefail
scratch="$1"
binary="$2"
kwin=""
hypr=""
access_bus=""
registry=""
cleanup() {
  if [[ -n "$registry" ]]; then kill "$registry" 2>/dev/null || true; wait "$registry" 2>/dev/null || true; fi
  if [[ -n "$access_bus" ]]; then kill "$access_bus" 2>/dev/null || true; wait "$access_bus" 2>/dev/null || true; fi
  if [[ -n "$hypr" ]]; then kill "$hypr" 2>/dev/null || true; wait "$hypr" 2>/dev/null || true; fi
  if [[ -n "$kwin" ]]; then kill "$kwin" 2>/dev/null || true; wait "$kwin" 2>/dev/null || true; fi
}
trap cleanup EXIT
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
for display in "$XDG_RUNTIME_DIR"/wayland-[0-9]*; do
  if [[ -S "$display" ]]; then export WAYLAND_DISPLAY="${display##*/}"; break; fi
done
if [[ -z "${WAYLAND_DISPLAY:-}" ]]; then cat "$scratch/hypr.log" >&2; exit 1; fi
export GDK_BACKEND=wayland GTK_USE_PORTAL=0
# System D-Bus activation can use a socket under /run/user even when the Wayland
# session is private. Both the GTK fixture and X must use this owned bus.
export AT_SPI_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/accessibility"
dbus-daemon --session --nofork --address="$AT_SPI_BUS_ADDRESS" > "$scratch/accessibility.log" 2>&1 &
access_bus="$!"
for i in {1..80}; do
  [[ ! -S "$XDG_RUNTIME_DIR/accessibility" ]] || break
  kill -0 "$access_bus" 2>/dev/null || break
  sleep 0.1
done
if [[ ! -S "$XDG_RUNTIME_DIR/accessibility" ]]; then cat "$scratch/accessibility.log" >&2; exit 1; fi
for candidate in /usr/lib/at-spi2-registryd /usr/libexec/at-spi2-registryd /usr/lib/at-spi2-core/at-spi2-registryd; do
  if [[ -x "$candidate" ]]; then
    "$candidate" --use-gnome-session=0 > "$scratch/registry.log" 2>&1 &
    registry="$!"
    break
  fi
done
if [[ -z "$registry" ]]; then echo "AT-SPI registry is required" >&2; exit 1; fi
gdbus wait --address "$AT_SPI_BUS_ADDRESS" --timeout 5 org.a11y.atspi.Registry
mkdir -m 700 "$scratch/bin"
cat > "$scratch/bin/mousepad" <<'PYTHON'
#!/usr/bin/env python3
from pathlib import Path
import gi

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk

app = Gtk.Application(application_id="org.xfce.mousepad")

def activate(app):
    home = Path.home()
    def record(text):
        (home / "editor-text").write_text(text, encoding="utf-8")

    if (home / "protected-editor").exists():
        view = Gtk.Entry()
        view.set_visibility(False)
        view.connect("changed", lambda entry: record(entry.get_text()))
    else:
        view = Gtk.TextView()
        buffer = view.get_buffer()
        buffer.connect("changed", lambda buffer: record(
            buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)))
    window = Gtk.ApplicationWindow(application=app, title="X disposable editor")
    window.set_default_size(480, 160)
    window.add(view)
    window.show_all()
    view.grab_focus()

    def close_if_requested():
        request = home / "close-editor"
        if not request.exists():
            return True
        request.unlink()
        window.close()
        return False

    GLib.timeout_add(100, close_if_requested)

app.connect("activate", activate)
app.run()
PYTHON
chmod 700 "$scratch/bin/mousepad"
export PATH="$scratch/bin:$PATH"
filter=""
if [[ -n "${X_TEST_VOICE_BENCH_RUNS:-}" ]]; then filter="offline_voice_path_latency"; fi
if ! X_TEST_ISOLATED_HYPRLAND=1 "$binary" --ignored --test-threads=1 --nocapture "$filter"; then
  hyprctl -j clients >&2

  exit 1
fi
INNER
