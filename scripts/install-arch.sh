#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" != "Linux" ]]; then
  echo "X currently installs as a Linux desktop service." >&2
  exit 1
fi

# Detect tools, not distro package names: these may come from Nix, Flatpak,
# Homebrew-on-Linux or user-local installs as well as the system package manager.
required=(git cargo rustc cc pkg-config node npm systemctl wtype grim xdg-open curl)
optional=(slurp wl-copy opencode)
missing=()
for cmd in "${required[@]}"; do
  command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
done

if (("${#missing[@]}")); then
  distro="unknown"
  if [[ -r /etc/os-release ]]; then
    # shellcheck source=/dev/null
    source /etc/os-release
    distro="${ID:-unknown}"
  fi
  echo "X needs these commands before it can install: ${missing[*]}" >&2
  case "$distro" in
    arch|cachyos|endeavouros|manjaro)
      echo 'Arch: sudo pacman -S --needed base-devel git rust openblas pkgconf wtype grim slurp wl-clipboard xdg-utils curl nodejs npm python-gobject at-spi2-core' >&2 ;;
    debian|ubuntu|linuxmint|pop)
      echo 'Debian/Ubuntu: sudo apt install build-essential git cargo rustc pkg-config libopenblas-dev wtype grim slurp wl-clipboard xdg-utils curl nodejs npm python3-gi at-spi2-core' >&2 ;;
    fedora)
      echo 'Fedora: sudo dnf install @development-tools git cargo rust pkgconf-pkg-config openblas-devel wtype grim slurp wl-clipboard xdg-utils curl nodejs npm python3-gobject at-spi2-core' >&2 ;;
    *)
      echo 'Install your distro equivalents, then rerun this installer.' >&2 ;;
  esac
  exit 1
fi

for cmd in "${optional[@]}"; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Optional helper missing: $cmd (some features will be unavailable)" >&2
  fi
done
if ! python3 -c 'import gi' >/dev/null 2>&1; then
  echo 'Optional Python GI/AT-SPI bindings are missing: computer-use accessibility may be unavailable.' >&2
fi

if [[ -z "${WAYLAND_DISPLAY:-}" && -z "${DISPLAY:-}" ]]; then
  echo 'Start the installer from your active desktop session, not an SSH-only shell.' >&2
  exit 1
fi

cd "$root"
export CARGO_NET_GIT_FETCH_WITH_CLI=true
X_BUILD=1 HEX_LOCAL_BUILD=1 cargo build --release --locked --bin hex

install -Dm755 target/release/hex "$HOME/.local/opt/x/hex"
mkdir -p "$HOME/.local/bin"
ln -sfn "$HOME/.local/opt/x/hex" "$HOME/.local/bin/x"
export PATH="$HOME/.local/bin:$PATH"

# Keep a fixed upstream version; npm writes package-lock.json alongside the
# local install so reinstallations can reuse its locked dependency resolution.
helper="$HOME/.local/opt/x/agent-desktop"
mkdir -p "$helper"
if [[ ! -f "$helper/package.json" ]]; then
  printf '%s\n' '{"private":true,"dependencies":{"agent-desktop":"1.1.0"}}' > "$helper/package.json"
fi
if [[ -f "$helper/package-lock.json" ]]; then
  npm ci --prefix "$helper" --no-audit --no-fund
else
  npm install --prefix "$helper" --no-audit --no-fund
fi
if [[ -x "$helper/node_modules/.bin/agent-desktop" ]]; then
  ln -sfn "$helper/node_modules/.bin/agent-desktop" "$HOME/.local/bin/agent-desktop"
fi

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
systemctl --user enable x.service
# The CLI records this graphical session into session.env *before* systemd
# starts the unit, avoiding a service with no WAYLAND_DISPLAY/Hyprland instance.
"$HOME/.local/bin/x" start
"$HOME/.local/bin/x" labs on x-agent
echo "Installed X without touching the official Hex installation."
"$HOME/.local/bin/x" agent doctor
echo "X is ready: x agent status"
