#!/usr/bin/env bash
# builds BuildCast.app and a compressed dmg from the release binaries
# usage: make_bundle.sh [version]   (run on macOS after `cargo build --workspace --release`)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VER="${1:-$(grep -m1 '^version = ' "$ROOT/Cargo.toml" | cut -d'"' -f2)}"
APP="$ROOT/target/release/bundle/BuildCast.app"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/buildcast" "$APP/Contents/MacOS/"
cp "$ROOT/target/release/buildcast-cli" "$APP/Contents/MacOS/"

ICONSET="$(mktemp -d)/buildcast.iconset"
mkdir -p "$ICONSET"
for s in 16 32 64 128 256 512; do
    sips -z "$s" "$s" "$ROOT/packages/gui/assets/buildcast-1024.png" \
        --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    sips -z "$d" "$d" "$ROOT/packages/gui/assets/buildcast-1024.png" \
        --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/buildcast.icns"

sed "s/@VERSION@/$VER/" "$ROOT/installer/macos/Info.plist" > "$APP/Contents/Info.plist"

hdiutil create -volname BuildCast -srcfolder "$APP" -ov -format UDZO \
    "$ROOT/target/release/buildcast-$VER-macos.dmg"
echo "built: target/release/buildcast-$VER-macos.dmg"
