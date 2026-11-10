#!/usr/bin/env bash
# Wraps the built `oyl` binary into Oyl.app so macOS treats it as an app
# (Dock icon, name, activation, bundle id).
#
#   scripts/bundle.sh            # debug build   -> <target>/debug/Oyl.app
#   scripts/bundle.sh --release  # release build -> <target>/release/Oyl.app
#   scripts/bundle.sh --open     # ... and launch it
set -euo pipefail
cd "$(dirname "$0")/.."

profile=debug
open_after=0
for arg in "$@"; do
  case "$arg" in
    --release) profile=release ;;
    --open) open_after=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

if [ "$profile" = release ]; then
  cargo build --release -p oyl-app
else
  cargo build -p oyl-app
fi

target_dir=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
app="$target_dir/$profile/Oyl.app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp assets/icon/Oyl.icns "$app/Contents/Resources/Oyl.icns"
cp "$target_dir/$profile/oyl" "$app/Contents/MacOS/oyl"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Oyl</string>
  <key>CFBundleIconFile</key><string>Oyl</string>
  <key>CFBundleDisplayName</key><string>Oyl</string>
  <key>CFBundleIdentifier</key><string>dev.oyl.Oyl</string>
  <key>CFBundleExecutable</key><string>oyl</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
</dict>
</plist>
PLIST
codesign --force --sign - "$app" >/dev/null 2>&1 || true
echo "$app"
if [ "$open_after" = 1 ]; then
  open "$app"
fi
