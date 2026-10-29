#!/bin/sh
# Builds the AppKit fixture for crates/nugget-a11y/tests/macos_ax.rs with swiftc (Xcode or the
# Command Line Tools) into an app bundle, so LaunchServices can launch and activate it.
#
# Usage: build.sh [out-dir]   Prints the path of the built NuggetA11yFixture.app.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-"$here/out"}
app="$out/NuggetA11yFixture.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
swiftc -O -framework AppKit -o "$app/Contents/MacOS/NuggetA11yFixture" "$here/A11yFixture.swift" >&2
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>NuggetA11yFixture</string>
  <key>CFBundleIdentifier</key><string>dev.nugget.a11y-fixture</string>
  <key>CFBundleName</key><string>NuggetA11yFixture</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
# Ad-hoc signature: arm64 binaries must be signed to run.
codesign --force --sign - "$app" >&2
echo "$app"
