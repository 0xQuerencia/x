#!/usr/bin/env bash
# Real compositor integration test in a disposable headless Sway session.
set -Eeuo pipefail
if ! command -v sway >/dev/null || ! command -v swaymsg >/dev/null || ! command -v foot >/dev/null; then
  echo "Headless Sway smoke requires sway and foot." >&2
  exit 1
fi
runtime="$(mktemp -d)"
chmod 700 "$runtime"
sway_pid=""
cleanup() {
  if [[ -n "$sway_pid" ]]; then
    kill "$sway_pid" 2>/dev/null || true
    wait "$sway_pid" 2>/dev/null || true
  fi
  rm -rf "$runtime"
}
trap cleanup EXIT
export XDG_RUNTIME_DIR="$runtime"
export WLR_BACKENDS=headless
export WLR_RENDERER=pixman
export WLR_LIBINPUT_NO_DEVICES=1
export WLR_HEADLESS_OUTPUTS=1
export XDG_SESSION_TYPE=wayland
cat > "$runtime/sway.conf" <<'CONF'
output * resolution 1280x800
focus_follows_mouse no
default_border pixel 1
CONF
# No device access, no X11 display, no dependency on a physical GPU.
sway -c "$runtime/sway.conf" --unsupported-gpu >"$runtime/sway.log" 2>&1 &
sway_pid=$!
ready=0
for i in $(seq 1 80); do
  if ! kill -0 "$sway_pid" 2>/dev/null; then
    echo "Sway exited before IPC became ready:" >&2
    cat "$runtime/sway.log" >&2
    exit 1
  fi
  socket="$(find "$runtime" -maxdepth 2 -type s -name 'sway-ipc.*.sock' -print -quit)"
  if [[ -n "$socket" ]] && SWAYSOCK="$socket" swaymsg -t get_version >/dev/null 2>&1; then
    export SWAYSOCK="$socket"
    display="$(find "$runtime" -maxdepth 1 -type s -name 'wayland-*' -printf '%f\n' | head -n 1)"
    if [[ -n "$display" ]]; then
      export WAYLAND_DISPLAY="$display"
      ready=1
      break
    fi
  fi
  sleep 0.15
done
if [[ "$ready" != 1 ]]; then
  echo "Headless Sway did not expose a usable IPC+Wayland session:" >&2
  cat "$runtime/sway.log" >&2
  exit 1
fi
echo "Headless Sway ready; checking isolated window operations."
cargo test --locked -p x-agent --lib wm::tests::live_sway -- --ignored --test-threads=1 --nocapture
