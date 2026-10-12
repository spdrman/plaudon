#!/usr/bin/env bash
# O2.6 — rasterise the application icon from its committed source.
#
# `apps/plaudon-egui/packaging/icon.svg` is the only hand-edited icon asset in
# the repository. Two derived artifacts are committed next to it because the
# platforms that need them have no SVG rasteriser:
#
#   icon-1024.png        the master raster every platform scales from
#   windows/plaudon.ico  the Windows executable, installer and shortcut icon
#
# The macOS `.icns` is *not* committed: `scripts/package-macos-egui.sh` builds
# it from the master raster with `sips` and `iconutil`, which exist on every
# Mac that can build the bundle.
#
#   scripts/make-app-icon.sh    regenerate the derived artifacts
#
# Byte-for-byte output depends on the rasteriser's version, so there is no
# `--check` mode here and no CI job diffing the images: what the committed
# artifacts *promise* (a 1024×1024 master, an `.ico` carrying the ten sizes
# Explorer asks for) is asserted instead by `apps/plaudon-egui/tests/
# packaging.rs`, which parses them. Requires one SVG rasteriser
# (`rsvg-convert`, `magick`/`convert` or `inkscape`) and, for the `.ico`,
# `magick`/`convert`. Reason token: `missing_tool`.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"

PKG="$REPO_ROOT/apps/plaudon-egui/packaging"
SRC="$PKG/icon.svg"
MASTER="$PKG/icon-1024.png"
ICO="$PKG/windows/plaudon.ico"
# The sizes Windows Explorer actually asks for, smallest first.
ICO_SIZES=(16 20 24 32 40 48 64 96 128 256)

if [ $# -gt 0 ]; then
  echo "unknown argument: $1" >&2
  exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

rasterise() { # out-png size
  local out="$1" size="$2"
  if command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert --width "$size" --height "$size" --output "$out" "$SRC"
  elif command -v magick >/dev/null 2>&1; then
    magick -background none -density 384 "$SRC" -resize "${size}x${size}" "$out"
  elif command -v convert >/dev/null 2>&1; then
    convert -background none -density 384 "$SRC" -resize "${size}x${size}" "$out"
  elif command -v inkscape >/dev/null 2>&1; then
    inkscape "$SRC" --export-type=png --export-filename="$out" \
      --export-width="$size" --export-height="$size" >/dev/null
  else
    echo "reason=missing_tool: no SVG rasteriser (rsvg-convert, magick, convert or inkscape)" >&2
    exit 1
  fi
}

echo "==> rasterising $SRC"
rasterise "$WORK/icon-1024.png" 1024

ICO_INPUTS=()
for size in "${ICO_SIZES[@]}"; do
  rasterise "$WORK/icon-$size.png" "$size"
  ICO_INPUTS+=("$WORK/icon-$size.png")
done

ICO_TOOL=""
for candidate in magick convert; do
  if command -v "$candidate" >/dev/null 2>&1; then ICO_TOOL="$candidate"; break; fi
done
if [ -z "$ICO_TOOL" ]; then
  echo "reason=missing_tool: magick or convert is required to assemble the .ico" >&2
  exit 1
fi
"$ICO_TOOL" "${ICO_INPUTS[@]}" "$WORK/plaudon.ico"

mkdir -p "$(dirname "$ICO")"
cp "$WORK/icon-1024.png" "$MASTER"
cp "$WORK/plaudon.ico" "$ICO"
echo "wrote $MASTER"
echo "wrote $ICO"
