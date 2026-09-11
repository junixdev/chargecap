#!/usr/bin/env bash
# make-icon.sh — build Resources/AppIcon.icns from a placeholder battery
# glyph. Draws a 1024x1024 SVG, rasterizes it with `qlmanage` (Quick Look,
# built into macOS — no ImageMagick dependency), then packs the required
# icon sizes into an .iconset and runs `iconutil` to produce the .icns.
#
#   scripts/make-icon.sh <output.icns>
set -euo pipefail

OUT="${1:?usage: make-icon.sh <output.icns>}"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/chargecap-icon.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

SVG="$WORK/icon.svg"
cat >"$SVG" <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024">
  <rect width="1024" height="1024" rx="224" fill="#1c1c1e"/>
  <rect x="292" y="372" width="380" height="280" rx="44" fill="#30d158"/>
  <rect x="672" y="452" width="56" height="120" rx="14" fill="#30d158"/>
  <rect x="332" y="412" width="220" height="200" rx="20" fill="#1c1c1e"/>
  <rect x="332" y="412" width="140" height="200" rx="20" fill="#30d158"/>
</svg>
EOF

qlmanage -t -s 1024 -o "$WORK" "$SVG" >/dev/null
BASE_PNG="$WORK/icon.svg.png"
[ -f "$BASE_PNG" ] || { echo "error: qlmanage did not produce a thumbnail" >&2; exit 1; }

ICONSET="$WORK/AppIcon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$BASE_PNG" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$BASE_PNG" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done

mkdir -p "$(dirname "$OUT")"
iconutil -c icns "$ICONSET" -o "$OUT"
echo "wrote $OUT"
