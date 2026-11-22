#!/usr/bin/env bash
# Package only the X fork's unsigned experimental executable.
set -euo pipefail
if (( $# != 2 )) || [[ "$(uname -s)" != "Darwin" ]]; then
  echo 'Usage on macOS: package-macos-preview.sh <x-binary> <output.zip>' >&2
  exit 2
fi
source_binary="$1"
output="$2"
if [[ ! -f "$source_binary" || ! -x "$source_binary" || -L "$source_binary" ]]; then
  echo 'Refusing an absent, linked or non-executable X preview binary' >&2
  exit 1
fi
if [[ -e "$output" || -L "$output" || "$output" != *.zip ]]; then
  echo 'Refusing to replace an existing/non-zip preview destination' >&2
  exit 1
fi
if ! command -v ditto >/dev/null || ! command -v plutil >/dev/null; then
  echo 'This bundle requires native macOS packaging utilities' >&2
  exit 1
fi
tmp="$(mktemp -d)"
trap 'rm -rf -- "$tmp"' EXIT
app="$tmp/X Preview.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$source_binary" "$app/Contents/MacOS/x"
chmod 755 "$app/Contents/MacOS/x"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleExecutable</key><string>x</string>
    <key>CFBundleIdentifier</key><string>art.querencia.x.preview</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>X Preview</string>
    <key>CFBundleDisplayName</key><string>X Preview</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>X only accesses the microphone when you enable dictation or voice commands.</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>X asks permission before controlling other applications.</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"
[[ "$(plutil -extract CFBundleIdentifier raw -o - "$app/Contents/Info.plist")" == 'art.querencia.x.preview' ]]
[[ "$(plutil -extract CFBundleExecutable raw -o - "$app/Contents/Info.plist")" == 'x' ]]
ditto -c -k --keepParent "$app" "$output"
test -s "$output"
