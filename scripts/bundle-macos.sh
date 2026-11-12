#!/bin/sh
# make Tessel.app around a build, so the Dock and the app switcher show
# tessel's name and icon instead of a bare binary's.
#
#   scripts/bundle-macos.sh [debug|release]   ->  target/<profile>/Tessel.app
#
# the binaries are hard links, not copies: no extra disk, and the app still
# finds tesseld and tesselctl beside itself. run it again after a build, since
# a build replaces the files the links point at.
set -eu
profile=${1:-debug}
root=$(cd "$(dirname "$0")/.." && pwd)
bin="$root/target/$profile"
app="$bin/Tessel.app"

for b in tessel tesseld tesselctl; do
    [ -x "$bin/$b" ] || { echo "no $bin/$b: build first (cargo build -p tessel -p tesseld -p tesselctl)" >&2; exit 1; }
done

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
for b in tessel tesseld tesselctl; do
    ln "$bin/$b" "$app/Contents/MacOS/$b" 2>/dev/null || cp "$bin/$b" "$app/Contents/MacOS/$b"
done
cp "$root/assets/icon/tessel.icns" "$app/Contents/Resources/tessel.icns"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/tessel/Cargo.toml" | head -1)
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Tessel</string>
    <key>CFBundleDisplayName</key><string>Tessel</string>
    <key>CFBundleIdentifier</key><string>app.tessel.desktop</string>
    <key>CFBundleExecutable</key><string>tessel</string>
    <key>CFBundleIconFile</key><string>tessel</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${version:-0.1.0}</string>
    <key>CFBundleVersion</key><string>${version:-0.1.0}</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
echo "$app"
