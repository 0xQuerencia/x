#!/usr/bin/env bash
# Run the X11 EWMH integration test in an isolated throwaway Xvfb display.
set -Eeuo pipefail
for program in Xvfb xvfb-run openbox xterm wmctrl xprop; do
  if ! command -v "$program" >/dev/null; then
    echo "Required isolated X11 smoke helper missing: $program" >&2
    exit 1
  fi
done
xvfb-run -a -e /dev/stderr -s "-screen 0 1280x800x24" bash -ec '
  openbox --sm-disable > /tmp/x-smoke-openbox.log 2>&1 &
  wm_pid=$!
  trap "kill $wm_pid 2>/dev/null || true" EXIT
  ready=0
  for i in $(seq 1 50); do
    if wmctrl -m >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 0.1
  done
  if [[ "$ready" != 1 ]]; then
    echo "Isolated X11 WM unavailable" >&2
    cat /tmp/x-smoke-openbox.log >&2
    exit 1
  fi
  # Test both desktop switching and moving while keeping all windows isolated.
  wmctrl -n 4
  cargo test --locked -p x-agent --lib wm::x11::tests::live_x11_ewmh -- --ignored --test-threads=1 --nocapture
'
