#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT
export HOME="$temporary/home"
mkdir -p "$HOME" "$temporary/source"
new="$temporary/source/new"
old="$HOME/.local/opt/x/hex"
previous="$HOME/.local/opt/x/hex.previous"
link="$HOME/.local/bin/x"

printf 'first\n' > "$new"
bash "$root/scripts/activate-arch-x.sh" "$new"
test "$(cat "$old")" = first
test "$(readlink "$link")" = "$old"
test ! -e "$previous"

printf 'second\n' > "$new"
bash "$root/scripts/activate-arch-x.sh" "$new"
test "$(cat "$old")" = second
test "$(cat "$previous")" = first

# Failed activation must not poison either installed version.
: > "$new"
if bash "$root/scripts/activate-arch-x.sh" "$new" >/dev/null 2>&1; then
  echo "Empty binary incorrectly accepted" >&2
  exit 1
fi
test "$(cat "$old")" = second
test "$(cat "$previous")" = first

printf 'third\n' > "$new"
printf 'unrelated\n' > "$temporary/source/unrelated"
ln -sfn "$temporary/source/unrelated" "$link"
if bash "$root/scripts/activate-arch-x.sh" "$new" >/dev/null 2>&1; then
  echo "Unrelated x command incorrectly replaced" >&2
  exit 1
fi
test "$(cat "$old")" = second
test "$(cat "$previous")" = first
test "$(cat "$link")" = unrelated
ln -sfn "$old" "$link"

bash "$root/scripts/activate-arch-x.sh" "$new"
test "$(cat "$old")" = third
test "$(cat "$previous")" = second
test "$(find "$HOME/.local/opt/x" -name '.hex-*' | wc -l)" -eq 0
echo "Arch X activation smoke passed"
