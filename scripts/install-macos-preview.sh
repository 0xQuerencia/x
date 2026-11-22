#!/usr/bin/env bash
set -euo pipefail

if (( $# != 1 )); then
  echo 'Usage: install-macos-preview.sh <x-macos-arm64>' >&2
  exit 2
fi

source_binary="$1"
if [[ -L "$source_binary" || ! -f "$source_binary" || ! -s "$source_binary" ]]; then
  echo 'A nonempty, regular X preview executable is required.' >&2
  exit 1
fi

if [[ "$(uname -s)" != Darwin ]]; then
  if [[ "${X_PREVIEW_TEST_PLATFORM:-}" != "1" ]]; then
    echo 'X macOS preview installation requires macOS.' >&2
    exit 1
  fi
fi

if [[ -z "${HOME:-}" || "$HOME" == '/' ]]; then
  echo 'Cannot determine a safe user home directory.' >&2
  exit 1
fi

folder="$HOME/.local/opt/x-macos-preview"
target="$folder/hex"
previous="$folder/hex.previous"
marker="$folder/.x-preview-owned"
link="$HOME/.local/bin/x-macos-preview"

if [[ -L "$folder" || -L "$target" || -L "$previous" || -L "$marker" ]]; then
  echo 'Refusing symlinked X preview installation files.' >&2
  exit 1
fi
if [[ -e "$folder" ]]; then
  if [[ ! -d "$folder" || ! -O "$folder" ]]; then
    echo 'Refusing an unowned or non-directory preview target.' >&2
    exit 1
  fi
  if [[ -e "$marker" ]]; then
    if [[ ! -f "$marker" || ! -O "$marker" || "$(cat "$marker")" != 'x-macos-preview-v1' ]]; then
      echo 'The X preview ownership marker is invalid.' >&2
      exit 1
    fi
  elif [[ -n "$(ls -A "$folder")" ]]; then
    echo 'Refusing an unrelated X preview directory.' >&2
    exit 1
  fi
fi
for file in "$target" "$previous"; do
  if [[ -e "$file" && ( ! -f "$file" || ! -O "$file" ) ]]; then
    echo 'Refusing an unrelated or unowned X preview executable.' >&2
    exit 1
  fi
done
if [[ -e "$link" || -L "$link" ]]; then
  if [[ ! -L "$link" || "$(readlink "$link")" != "$target" ]]; then
    echo 'Refusing to replace an unrelated x-macos-preview command.' >&2
    exit 1
  fi
fi

mkdir -p "$folder" "$HOME/.local/bin"
stage="$(mktemp "$folder/.new.XXXXXXXX")"
backup_stage=''
mark_stage=''
cleanup() {
  rm -f -- "$stage"
  if [[ -n "$backup_stage" ]]; then rm -f -- "$backup_stage"; fi
  if [[ -n "$mark_stage" ]]; then rm -f -- "$mark_stage"; fi
}
trap cleanup EXIT

install -m755 "$source_binary" "$stage"
if [[ -f "$target" ]]; then
  # Never overwrite the previous-version executable by truncating it.
  backup_stage="$(mktemp "$folder/.prev.XXXXXXXX")"
  install -m755 "$target" "$backup_stage"
  mv -f -- "$backup_stage" "$previous"
  backup_stage=''
fi
if [[ ! -e "$marker" ]]; then
  mark_stage="$(mktemp "$folder/.marker.XXXXXXXX")"
  printf '%s\n' 'x-macos-preview-v1' > "$mark_stage"
  chmod 600 "$mark_stage"
  mv -f -- "$mark_stage" "$marker"
  mark_stage=''
fi
mv -f -- "$stage" "$target"
ln -sfn "$target" "$link"
trap - EXIT
echo "Installed X unsigned CLI preview to $target. Official Hex was not modified."
echo 'Preview is not notarized and has not passed physical macOS acceptance.'
