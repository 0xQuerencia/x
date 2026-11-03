#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
missing=()
for pkg in base-devel git rust openblas wtype nodejs npm; do
  pacman -Qq "$pkg" >/dev/null 2>&1 || missing+=("$pkg")
done
if ((${#missing[@]})); then
  echo "Missing packages: ${missing[*]}" >&2
  echo "Install them with: sudo pacman -S --needed ${missing[*]}" >&2
  exit 1
fi

cd "$root"
export CARGO_NET_GIT_FETCH_WITH_CLI=true
X_BUILD=1 HEX_LOCAL_BUILD=1 cargo build --release --bin hex

install -Dm755 target/release/hex "$HOME/.local/opt/x/hex"
mkdir -p "$HOME/.local/bin"
ln -sf "$HOME/.local/opt/x/hex" "$HOME/.local/bin/x"
# Screen access reads apps through AT-SPI with a pinned agent-desktop (Apache-2.0).
mkdir -p "$HOME/.local/opt/x/agent-desktop"
npm install --prefix "$HOME/.local/opt/x/agent-desktop" agent-desktop@1.1.0 --no-audit --no-fund
mkdir -p "$HOME/.config/systemd/user"
cat > "$HOME/.config/systemd/user/x.service" <<'UNIT'
# X-Managed=true
[Unit]
Description=X voice and agent
PartOf=graphical-session.target
After=graphical-session.target pipewire.service wireplumber.service
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
Type=notify
NotifyAccess=main
ExecStart=%h/.local/opt/x/hex service
EnvironmentFile=-%h/.local/share/x/session.env
Restart=on-failure
RestartSec=3
TimeoutStopSec=30
UMask=0077

[Install]
WantedBy=graphical-session.target
UNIT
systemctl --user daemon-reload
echo "Installed as x. Official Hex is untouched; X keeps its data in ~/.local/share/x."
echo "Run: systemctl --user enable --now x.service && x labs on x-agent && x agent status"
