#!/bin/sh
# A real packaging smoke test: ad-hoc fixture only, no credentials or Apple service.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
directory=${BUILD_DIR:-"$ROOT/build"}
mkdir -p "$directory"
temporary=$(mktemp -d "$directory/dmg-test.XXXXXX")
trap 'rm -rf "$temporary"' EXIT
trap 'exit 1' HUP INT TERM
app="$temporary/lemen.app"
mkdir -p "$app/Contents/MacOS"
cp "$ROOT/Resources/Info.plist" "$app/Contents/Info.plist"
xcrun swiftc -o "$app/Contents/MacOS/lemen" - <<'SWIFT'
print("lemen packaging test")
SWIFT
codesign --force --sign - --timestamp=none "$app"
before=$(shasum -a 256 "$app/Contents/MacOS/lemen")
(umask 077; sh "$ROOT/scripts/build-dmg.sh" "$app" "$temporary/lemen-macos-arm64.dmg")
test "$before" = "$(shasum -a 256 "$app/Contents/MacOS/lemen")"
printf 'DMG smoke test passed; source app unchanged.\n'
