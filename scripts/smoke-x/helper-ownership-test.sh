#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
temp="$(mktemp -d)"
trap 'rm -rf -- "$temp"' EXIT
mkdir -p "$temp/bin"
helper="$temp/managed-helper"
link="$temp/bin/agent-desktop"
check() { bash "$root/scripts/check-arch-helper.sh" "$helper" "$link"; }
refuse() { if check >/dev/null 2>&1; then echo "Unexpectedly accepted helper state" >&2; exit 1; fi; }
check
mkdir -p "$helper"
printf '%s\n' '{"private":true,"dependencies":{"agent-desktop":"1.1.0"}}' > "$helper/package.json"
check
ln -s "$helper/node_modules/.bin/agent-desktop" "$link"
check
rm "$link"
printf 'unrelated\n' > "$link"
refuse
rm "$link"
ln -s "$temp/unrelated-cli" "$link"
refuse
rm "$link"
mv "$helper/package.json" "$temp/original.json"
ln -s "$temp/original.json" "$helper/package.json"
refuse
rm "$helper/package.json"
printf '%s\n' '{"private":true,"scripts":{"postinstall":"echo dangerous"},"dependencies":{"agent-desktop":"1.1.0"}}' > "$helper/package.json"
refuse
mv "$temp/original.json" "$helper/package.json"
check
mv "$helper" "$temp/owned-original"
ln -s "$temp/owned-original" "$helper"
refuse
echo "Arch X desktop helper ownership smoke passed"
