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

# the voice helper (the microphone, and apple's speech engine): swift, so not
# part of cargo's build. it needs macOS 26's sdk; without it, tessel still
# runs, and only the cloud voice engines are offered.
voice_cfg=$([ "$profile" = release ] && echo release || echo debug)
if command -v swift >/dev/null 2>&1 && swift build -c "$voice_cfg" --package-path "$root/voice" >/dev/null 2>&1; then
    cp "$root/voice/.build/$voice_cfg/tessel-voice" "$app/Contents/MacOS/tessel-voice"
else
    echo "note: tessel-voice wasn't built (it needs swift and the macOS 26 sdk); voice input will offer cloud engines only" >&2
fi

# the workspace's version: the first `version = ` in the root Cargo.toml is [workspace.package]'s.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
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
    <key>NSMicrophoneUsageDescription</key><string>Tessel listens while you hold ⌥ (or press the mic) in the message box, and writes what you say there.</string>
    <key>NSSpeechRecognitionUsageDescription</key><string>Tessel turns what you say into text for the message box, on this Mac.</string>
</dict>
</plist>
PLIST
echo "$app"
