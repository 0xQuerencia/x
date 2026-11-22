#!/usr/bin/env bash
set -euo pipefail

x_dir="$HOME/.local/opt/x"
x_binary="$x_dir/hex"
x_backup="$x_dir/hex.previous"
x_link="$HOME/.local/bin/x"

if [[ -L "$x_dir" || ! -d "$x_dir" ||
      -L "$x_binary" || ! -f "$x_binary" ||
      -L "$x_backup" || ! -f "$x_backup" ]]; then
  echo "An installed X binary and a regular previous-version backup are required." >&2
  exit 1
fi
if [[ ! -L "$x_link" || "$(readlink "$x_link")" != "$x_binary" ]]; then
  echo "Refusing rollback: the x command is not owned by this X installation." >&2
  exit 1
fi

stage="$(mktemp "$x_dir/.hex-rollback.XXXXXXXX")"
trap 'rm -f "$stage"' EXIT
install -m755 "$x_backup" "$stage"
mv -f -- "$stage" "$x_binary"
trap - EXIT

if systemctl --user is-active --quiet x.service; then
  if ! systemctl --user restart x.service; then
    echo "Previous X binary restored, but the X service did not restart." >&2
    exit 1
  fi
fi
echo "Previous X executable restored. Your settings and workflows were not changed."
