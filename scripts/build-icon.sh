#!/bin/sh
# Convert the original square logo into a macOS icon without altering its artwork.
set -eu
if [ "$#" -ne 2 ]; then
    printf 'Usage: %s source.png destination.icns\n' "$0" >&2
    exit 2
fi
source_image="$1"
destination="$2"
if [ ! -f "$source_image" ]; then
    printf 'Icon source not found: %s\n' "$source_image" >&2
    exit 1
fi
width="$(sips -g pixelWidth "$source_image" | awk '/pixelWidth:/ { print $2 }')"
height="$(sips -g pixelHeight "$source_image" | awk '/pixelHeight:/ { print $2 }')"
case "$width:$height" in
    *[!0-9:]*|:|:*|*:)
        printf 'Could not read the icon dimensions.\n' >&2
        exit 1
        ;;
esac
if [ "$width" -ne "$height" ] || [ "$width" -lt 1024 ]; then
    printf 'Use a square PNG at least 1024 by 1024 pixels; artwork is never cropped or stretched.\n' >&2
    exit 1
fi
mkdir -p "$(dirname "$destination")"
temporary="$(mktemp -d "$(dirname "$destination")/icon.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT
trap 'exit 1' HUP INT TERM
iconset="$temporary/lemur.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -s format png -z "$size" "$size" "$source_image" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double="$((size * 2))"
    sips -s format png -z "$double" "$double" "$source_image" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil --convert icns --output "$destination" "$iconset"
