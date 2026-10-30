#!/usr/bin/env bash
# Build target/release/bundle.noindex/Sennya.app (.noindex keeps it out of Spotlight; install with scripts/install-macos.sh)
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP=target/release/bundle.noindex/Sennya.app
rm -rf "$APP"
mkdir -p "$(dirname "$APP")"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/sennya "$APP/Contents/MacOS/sennya"

# Regenerate with `python3 scripts/make_icon.py` (needs numpy).
ICON_PNG=assets/icon-1024.png
ICONSET=target/sennya.iconset
rm -rf "$ICONSET" && mkdir "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s "$ICON_PNG" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) "$ICON_PNG" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/sennya.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Sennya</string>
  <key>CFBundleDisplayName</key><string>Sennya</string>
  <key>CFBundleIdentifier</key><string>dev.sennya.terminal</string>
  <key>CFBundleExecutable</key><string>sennya</string>
  <key>CFBundleIconFile</key><string>sennya</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || true
echo "Built $APP"
