#!/bin/sh
# A real packaging smoke test: ad-hoc fixture only, no credentials or Apple service.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
directory=${BUILD_DIR:-"$ROOT/build"}
mkdir -p "$directory"
temporary=$(mktemp -d "$directory/dmg-test.XXXXXX")
trap 'rm -rf "$temporary"' EXIT
trap 'exit 1' HUP INT TERM
app="$temporary/lemur.app"
mkdir -p "$app/Contents/MacOS"
cp "$ROOT/Resources/Info.plist" "$app/Contents/Info.plist"
xcrun swiftc -o "$app/Contents/MacOS/lemur" - <<'SWIFT'
print("lemur packaging test")
SWIFT
codesign --force --sign - --timestamp=none "$app"
before=$(shasum -a 256 "$app/Contents/MacOS/lemur")
(umask 077; sh "$ROOT/scripts/build-dmg.sh" "$app" "$temporary/lemur-macos-arm64.dmg")
test "$before" = "$(shasum -a 256 "$app/Contents/MacOS/lemur")"
printf 'DMG smoke test passed; source app unchanged.\n'
