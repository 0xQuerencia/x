#!/usr/bin/env bash
set -euo pipefail

if (( $# != 1 )); then
  echo "Usage: activate-arch-x.sh <new-x-executable>" >&2
  exit 2
fi
source_binary="$1"
if [[ -L "$source_binary" || ! -f "$source_binary" || ! -s "$source_binary" ]]; then
  echo "The new X executable must be a nonempty regular file." >&2
  exit 1
fi

x_dir="$HOME/.local/opt/x"
x_binary="$x_dir/hex"
x_backup="$x_dir/hex.previous"
x_link="$HOME/.local/bin/x"
if [[ -L "$x_dir" ]]; then
  echo "Refusing a symlinked X install directory." >&2
  exit 1
fi
for file in "$x_binary" "$x_backup"; do
  if [[ -L "$file" || ( -e "$file" && ! -f "$file" ) ]]; then
    echo "Refusing an unexpected X install file: $file" >&2
    exit 1
  fi
done
if [[ -e "$x_link" || -L "$x_link" ]]; then
  if [[ ! -L "$x_link" || "$(readlink "$x_link")" != "$x_binary" ]]; then
    echo "Refusing to replace an unrelated x command." >&2
    exit 1
  fi
fi

mkdir -p "$x_dir" "$HOME/.local/bin"
stage="$(mktemp "$x_dir/.hex-new.XXXXXXXX")"
backup_stage=""
cleanup() {
  rm -f -- "$stage"
  if [[ -n "$backup_stage" ]]; then
    rm -f -- "$backup_stage"
  fi
}
trap cleanup EXIT
install -m755 -- "$source_binary" "$stage"

# Never truncate the previous-version backup in place. A crash or full disk
# during the copy must leave the last good rollback executable untouched.
if [[ -f "$x_binary" ]]; then
  backup_stage="$(mktemp "$x_dir/.hex-previous.XXXXXXXX")"
  install -m755 -- "$x_binary" "$backup_stage"
  mv -f -- "$backup_stage" "$x_backup"
  backup_stage=""
fi

# Both files are on the destination filesystem; only completed copies are
# moved into the active names.
mv -f -- "$stage" "$x_binary"
ln -sfn -- "$x_binary" "$x_link"
