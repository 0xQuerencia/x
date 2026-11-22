#!/usr/bin/env bash
set -euo pipefail

if (( $# != 2 )); then
  echo "Usage: check-arch-helper.sh <helper-dir> <cli-link>" >&2
  exit 2
fi
helper="$1"
link="$2"
expected="$helper/node_modules/.bin/agent-desktop"

for path in "$helper" "$helper/package.json" "$helper/package-lock.json" "$helper/node_modules"; do
  if [[ -L "$path" ]]; then
    echo "Refusing a linked X desktop helper path: $path" >&2
    exit 1
  fi
done
if [[ -e "$helper" && ( ! -d "$helper" || ! -O "$helper" ) ]]; then
  echo "Refusing an unrelated or unowned desktop helper directory." >&2
  exit 1
fi
for file in "$helper/package.json" "$helper/package-lock.json"; do
  if [[ -e "$file" && ( ! -f "$file" || ! -O "$file" ) ]]; then
    echo "Refusing an unexpected desktop helper file: $file" >&2
    exit 1
  fi
done
if [[ -e "$helper/node_modules" && ( ! -d "$helper/node_modules" || ! -O "$helper/node_modules" ) ]]; then
  echo "Refusing an unexpected desktop helper dependency directory." >&2
  exit 1
fi
if [[ -e "$link" || -L "$link" ]]; then
  if [[ ! -L "$link" || "$(readlink "$link")" != "$expected" ]]; then
    echo "Refusing to overwrite an unrelated agent-desktop command." >&2
    exit 1
  fi
fi
if [[ -f "$helper/package.json" ]]; then
  node - "$helper/package.json" <<'NODE'
const fs = require('fs');
const path = process.argv[2];
let data;
try { data = JSON.parse(fs.readFileSync(path, 'utf8')); }
catch { process.stderr.write('Invalid desktop helper manifest.\n'); process.exit(1); }
if (!data || data.private !== true ||
    Object.keys(data).sort().join(',') !== 'dependencies,private' ||
    Object.keys(data.dependencies || {}).join(',') !== 'agent-desktop' ||
    data.dependencies['agent-desktop'] !== '1.1.0') {
  process.stderr.write('Refusing an unrelated or modified desktop helper manifest.\n');
  process.exit(1);
}
NODE
fi
