#!/bin/sh
# Wraps a built binary in an ad-hoc signed Sense.app so launches go through
# LaunchServices like a dock click. usage: scripts/bundle.sh [profile]
set -eu
profile=${1:-release}
root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/target/$profile/Sense.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$root/target/$profile/sense" "$app/Contents/MacOS/sense"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>dev.sense.app</string>
  <key>CFBundleName</key><string>Sense</string>
  <key>CFBundleExecutable</key><string>sense</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
codesign --force --sign - "$app" >/dev/null
echo "$app"
