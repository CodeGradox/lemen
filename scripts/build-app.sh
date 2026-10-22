#!/bin/sh
# Builds build/lemur.app. Signed ad hoc unless LEMEN_SIGNING_IDENTITY names
# a Developer ID, which also gets the hardened runtime notarization needs.
set -eu
cd "$(dirname "$0")/.."

signing_identity="${LEMEN_SIGNING_IDENTITY:--}"
build_directory="${BUILD_DIR:-$PWD/build}"
cargo build --release --locked
# The version comes from Cargo.toml, so there is one place to bump it.
metadata="$(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
m = json.load(sys.stdin)
print(m["packages"][0]["version"])
print(m["target_directory"])
')"
version="$(printf '%s\n' "$metadata" | sed -n 1p)"
target_directory="$(printf '%s\n' "$metadata" | sed -n 2p)"
app="$build_directory/lemur.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target_directory/release/lemur" "$app/Contents/MacOS/lemur"
cp Resources/Info.plist "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy \
    -c "Add :CFBundleShortVersionString string $version" \
    -c "Add :CFBundleVersion string $version" \
    "$app/Contents/Info.plist"
rm -f "$app/Contents/Resources/lemur.icns"
if [ -f Resources/lemur.png ]; then
    sh scripts/build-icon.sh Resources/lemur.png "$app/Contents/Resources/lemur.icns"
    /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string lemur" "$app/Contents/Info.plist"
fi
set -- --force --sign "$signing_identity"
case "$signing_identity" in
    "Developer ID Application:"*)
        set -- "$@" --options runtime --timestamp
        ;;
esac
if [ -n "${LEMEN_SIGNING_KEYCHAIN:-}" ]; then
    set -- "$@" --keychain "$LEMEN_SIGNING_KEYCHAIN"
fi
printf 'Signing with %s\n' "$signing_identity"
codesign "$@" "$app"
codesign --verify --strict "$app"
printf '\nBuilt %s (version %s)\nLaunch with: open "%s"\n' "$app" "$version" "$app"
