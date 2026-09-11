#!/usr/bin/env bash
# make-dmg.sh — wrap target/bundle/chargecap.app in a compressed disk image
# with an Applications shortcut next to it, so installing is one drag.
#
#   scripts/make-dmg.sh                 # target/bundle/chargecap-<version>.dmg
#   scripts/make-dmg.sh out/custom.dmg  # write to a chosen path
#
# Run scripts/bundle.sh first; this script does not build.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

APP="$ROOT/target/bundle/chargecap.app"
[ -d "$APP" ] || { echo "error: $APP does not exist; run scripts/bundle.sh first" >&2; exit 1; }

VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/crates/app/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "error: cannot read version from crates/app/Cargo.toml" >&2; exit 1; }

OUT="${1:-$ROOT/target/bundle/chargecap-$VERSION.dmg}"
case "$OUT" in /*) ;; *) OUT="$PWD/$OUT" ;; esac

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/chargecap-dmg.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

echo "Staging $APP..."
ditto "$APP" "$STAGE/chargecap.app"
ln -s /Applications "$STAGE/Applications"

echo "Creating $OUT..."
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"
hdiutil create -quiet -volname "chargecap $VERSION" -srcfolder "$STAGE" \
  -fs HFS+ -format UDZO -ov "$OUT"

echo "$OUT"
