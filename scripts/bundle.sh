#!/usr/bin/env bash
# bundle.sh — build chargecap and chargecapd in release mode and assemble
# them into an ad-hoc-signed target/bundle/chargecap.app.
#
# The bundle carries both binaries: `chargecap` is what a user launches, and
# `chargecapd` rides along so scripts/install.sh can find it and hand it to
# its own `install` subcommand (crates/daemon/src/install.rs), which copies
# itself out to /usr/local/libexec/chargecapd and never depends on the app
# bundle staying in place after that.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

APP="$ROOT/target/bundle/chargecap.app"
CONTENTS="$APP/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"

echo "Building chargecap and chargecapd (release)..."
cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml"

VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/crates/app/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "error: cannot read version from crates/app/Cargo.toml" >&2; exit 1; }

echo "Assembling $APP (version $VERSION)..."
rm -rf "$APP"
mkdir -p "$MACOS" "$RESOURCES"

install -m 755 "$ROOT/target/release/chargecap" "$MACOS/chargecap"
install -m 755 "$ROOT/target/release/chargecapd" "$MACOS/chargecapd"

"$HERE/make-icon.sh" "$RESOURCES/AppIcon.icns"

cat >"$CONTENTS/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>com.rymera.chargecap.app</string>
	<key>CFBundleName</key>
	<string>chargecap</string>
	<key>CFBundleExecutable</key>
	<string>chargecap</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon.icns</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSUIElement</key>
	<true/>
	<key>LSMinimumSystemVersion</key>
	<string>14.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
EOF

plutil -lint "$CONTENTS/Info.plist" >/dev/null

echo "Ad-hoc signing..."
codesign --force --sign - --deep "$APP"

echo "$APP"
