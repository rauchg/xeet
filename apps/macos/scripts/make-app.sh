#!/bin/bash
# Builds "Xeet Capture.app": the menu-bar app, the Apple Vision OCR helper,
# and the xeet CLI side by side in Contents/MacOS (so `xeet capture --ocr auto`
# finds the helper). Ad-hoc signed for local use.
#
# UNVERIFIED: never run (written without macOS). Requires macOS 14+, Xcode or
# the Command Line Tools (Swift 5.9+), and a Rust toolchain.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="${1:-$here/build}"
app="$out/Xeet Capture.app"

(cd "$repo" && cargo build --release --bin xeet)
(cd "$here" && swift build -c release)
bin="$(cd "$here" && swift build -c release --show-bin-path)"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin/XeetCapture" "$app/Contents/MacOS/XeetCapture"
cp "$bin/xeet-vision-ocr" "$app/Contents/MacOS/xeet-vision-ocr"
cp "$repo/target/release/xeet" "$app/Contents/MacOS/xeet"
version="$(grep -m1 '^version' "$repo/Cargo.toml" | cut -d'"' -f2)"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>dev.xeet.capture</string>
  <key>CFBundleName</key><string>Xeet Capture</string>
  <key>CFBundleExecutable</key><string>XeetCapture</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
codesign --force --deep --sign - "$app"
echo "$app"
