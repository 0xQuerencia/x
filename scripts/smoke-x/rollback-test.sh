#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT
export HOME="$temp/home"
mkdir -p "$HOME/.local/opt/x" "$HOME/.local/bin" "$HOME/stubs"
printf 'new\n' > "$HOME/.local/opt/x/hex"
printf 'previous\n' > "$HOME/.local/opt/x/hex.previous"
ln -s "$HOME/.local/opt/x/hex" "$HOME/.local/bin/x"
printf '#!/usr/bin/env bash\nexit 3\n' > "$HOME/stubs/systemctl"
chmod +x "$HOME/stubs/systemctl"
export PATH="$HOME/stubs:$PATH"

bash "$root/scripts/rollback-arch-x.sh" >/dev/null
test "$(cat "$HOME/.local/opt/x/hex")" = previous

printf 'unrelated\n' > "$HOME/.local/bin/unrelated"
ln -sfn "$HOME/.local/bin/unrelated" "$HOME/.local/bin/x"
if bash "$root/scripts/rollback-arch-x.sh" >/dev/null 2>&1; then
  echo 'Rollback incorrectly overwrote an unrelated x command' >&2
  exit 1
fi
test "$(cat "$HOME/.local/opt/x/hex")" = previous
echo "Arch X rollback smoke passed"
