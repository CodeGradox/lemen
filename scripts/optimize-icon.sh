#!/bin/sh
# One-time source preparation; normal builds need only Apple's sips/iconutil.
set -eu
if [ "$#" -ne 2 ]; then
    printf 'Usage: %s original-image optimized.png\n' "$0" >&2
    exit 2
fi
command -v vips >/dev/null 2>&1 || {
    printf 'Install libvips first: brew install vips\n' >&2
    exit 1
}
source_image="$1"
destination="$2"
width="$(vipsheader -f width "$source_image")"
height="$(vipsheader -f height "$source_image")"
if [ "$width" -ne "$height" ] || [ "$width" -lt 1024 ]; then
    printf 'Use a square image at least 1024 by 1024 pixels; no cropping or upscaling is performed.\n' >&2
    exit 1
fi
mkdir -p "$(dirname "$destination")"
temporary="$(mktemp -d "$(dirname "$destination")/icon-opt.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT
trap 'exit 1' HUP INT TERM
# Apply orientation/profile before stripping metadata. Preserve full RGBA, not a
# quantized palette, so the gradient and transparent edges do not gain banding.
vips thumbnail "$source_image" "$temporary/resized.v" 1024 \
    --height 1024 --size down --output-profile srgb
vips pngsave "$temporary/resized.v" "$temporary/optimized.png" \
    --compression 9 --filter all --keep none
mv "$temporary/optimized.png" "$destination"
printf 'Optimized 1024 × 1024 icon: %s\n' "$destination"
