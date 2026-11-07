#!/usr/bin/env bash
# Build target/release/bundle.noindex/Sennya.app (.noindex keeps it out of Spotlight; install with scripts/install-macos.sh)
#
#   scripts/bundle-macos.sh                      cargo build --release for this Mac (the default)
#   scripts/bundle-macos.sh --target TRIPLE ...  build for TRIPLE; two or more targets are joined
#                                                with lipo, e.g. a universal app:
#                                                --target aarch64-apple-darwin --target x86_64-apple-darwin
#   scripts/bundle-macos.sh --binary PATH        bundle an already-built sennya binary, no cargo
set -euo pipefail

TARGETS=()
BINARY=
while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGETS+=("${2:?--target needs a target triple}"); shift 2 ;;
    --target=*) TARGETS+=("${1#*=}"); shift ;;
    --binary) BINARY="${2:?--binary needs a path}"; shift 2 ;;
    --binary=*) BINARY="${1#*=}"; shift ;;
    -h | --help) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "bundle-macos.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
  esac
done
if [ -n "$BINARY" ]; then
  [ ${#TARGETS[@]} -eq 0 ] || { echo "bundle-macos.sh: use --binary or --target, not both" >&2; exit 2; }
  [ -f "$BINARY" ] || { echo "bundle-macos.sh: no binary at $BINARY" >&2; exit 1; }
  # Relative to where the script was called from, not the repo root.
  BINARY="$(cd "$(dirname "$BINARY")" && pwd)/$(basename "$BINARY")"
fi
cd "$(dirname "$0")/.."

if [ -n "$BINARY" ]; then
  BIN="$BINARY"
elif [ ${#TARGETS[@]} -eq 0 ]; then
  cargo build --release
  BIN=target/release/sennya
else
  CARGO_ARGS=(build --release)
  SLICES=()
  for t in "${TARGETS[@]}"; do
    CARGO_ARGS+=(--target "$t")
    SLICES+=("target/$t/release/sennya")
  done
  cargo "${CARGO_ARGS[@]}"
  if [ ${#SLICES[@]} -eq 1 ]; then
    BIN="${SLICES[0]}"
  else
    BIN=target/universal-apple-darwin/release/sennya
    mkdir -p "$(dirname "$BIN")"
    lipo -create -output "$BIN" "${SLICES[@]}"
  fi
fi

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP=target/release/bundle.noindex/Sennya.app
rm -rf "$APP"
mkdir -p "$(dirname "$APP")"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/sennya"

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
