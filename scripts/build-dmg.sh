#!/bin/sh
# Package an already signed app with dmgbuild; never load credentials or sign code.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TOOLS=${DMG_TOOLS_DIR:-${BUILD_DIR:-"$ROOT/build"}/dmg-tools}
if [ "$#" -ne 2 ]; then
    echo "Usage: sh scripts/build-dmg.sh /path/to/lemur.app /path/to/output.dmg" >&2
    exit 1
fi
if [ ! -x "$TOOLS/bin/dmgbuild" ]; then
    echo "Missing DMG tools. Run sh scripts/prepare-dmg-tools.sh first." >&2
    exit 1
fi
app=$(CDPATH= cd -- "$1" && pwd)
output="$(CDPATH= cd -- "$(dirname -- "$2")" && pwd)/$(basename -- "$2")"
test "$(basename -- "$app")" = lemur.app
case "$output" in
    "$app"/*) echo "Output must not be inside the app." >&2; exit 1 ;;
    *.dmg) ;;
    *) echo "Output must end in .dmg." >&2; exit 1 ;;
esac
if [ -e "$output" ] || [ -L "$output" ]; then
    echo "Output already exists; refusing to replace it." >&2
    exit 1
fi
codesign --verify --deep --strict "$app"
umask 022
"$TOOLS/bin/dmgbuild" -s "$ROOT/scripts/dmg-settings.py" -D "app=$app" lemur "$output"
hdiutil verify "$output"

# Check the actual image rather than trusting the packaging tool's exit status.
mount=$(mktemp -d "${TMPDIR:-/tmp}/lemur-dmg-check.XXXXXX")
cleanup() {
    if ! hdiutil detach "$mount" >/dev/null 2>&1; then
        if ! rmdir "$mount" 2>/dev/null; then
            printf 'Could not detach disk image at %s; eject it manually.\n' "$mount" >&2
            exit 1
        fi
    else
        rmdir "$mount"
    fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
hdiutil attach -readonly -nobrowse -noautoopen -mountpoint "$mount" "$output" >/dev/null
test "$(readlink "$mount/Applications")" = /Applications
test -f "$mount/.DS_Store"
codesign --verify --deep --strict "$mount/lemur.app"
printf 'Created and verified %s\n' "$output"
