#!/usr/bin/env sh
# Regenerate the app icon's PNGs and the exe's multi-size .ico from the
# SVGs in assets/icon/.  Needs rsvg-convert (librsvg) and ImageMagick's
# `convert`.  Run from anywhere: tools/build-icon.sh
#
# 16, 24 and 32 px come from their own hand-tuned, pixel-aligned SVGs
# (16.svg, 24.svg, 32.svg), never from downscales; 48, 64 and 256 px from
# large.svg.  Outputs:
#   assets/icon/{16,24,32}.png              title-bar icon (title_bar.rs)
#   assets/icon.png, assets/icon.svg        256 px render, README icon
#   crates/rustdown-gui/windows/mdnotepad.ico  exe icon (16-256 px)
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
icon="$root/assets/icon"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

render() { # render <svg> <px> <png>
  rsvg-convert --width "$2" --height "$2" --format png --output "$3" "$1"
}

for size in 16 24 32; do
  render "$icon/$size.svg" "$size" "$icon/$size.png"
done
for size in 48 64 256; do
  render "$icon/large.svg" "$size" "$tmp/$size.png"
done

cp "$tmp/256.png" "$root/assets/icon.png"
cp "$icon/large.svg" "$root/assets/icon.svg"
convert "$icon/16.png" "$icon/24.png" "$icon/32.png" \
  "$tmp/48.png" "$tmp/64.png" "$tmp/256.png" \
  "$root/crates/rustdown-gui/windows/mdnotepad.ico"
echo "icon: assets/icon/{16,24,32}.png, assets/icon.png, windows/mdnotepad.ico"
