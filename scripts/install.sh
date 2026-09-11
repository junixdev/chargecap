#!/usr/bin/env bash
# install.sh — build the .app bundle, put it in /Applications, install the
# root daemon, and launch the menu-bar app.
#
#   scripts/install.sh              # bundle + install + launch
#   scripts/install.sh --no-build   # reuse an existing target/bundle build
#   scripts/install.sh --dry-run    # print every command, run nothing
#
# Needs `sudo` once, to install chargecapd as a LaunchDaemon (SMC writes are
# root-only).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

BUILD=1
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --no-build) BUILD=0 ;;
    --dry-run) DRY_RUN=1 ;;
    *) echo "error: unknown option $arg" >&2; exit 1 ;;
  esac
done

BUNDLE="$ROOT/target/bundle/chargecap.app"
DEST="/Applications/chargecap.app"

run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    printf '+ %s\n' "$*"
  else
    "$@"
  fi
}

# WARNING: the script quits a running chargecap before it installs. If a
# later step fails, put the menu-bar app back, so a failed install never
# leaves the Mac with neither the app nor the daemon.
on_error() {
  echo "error: install failed." >&2
  if [ "$DRY_RUN" -eq 0 ] && [ -d "$DEST" ]; then
    echo "Relaunching $DEST so you keep the menu; it will report the daemon as not running." >&2
    open "$DEST" || true
  fi
  echo "The daemon log is at /Library/Logs/chargecap/daemon.log." >&2
}
trap on_error ERR

if [ "$BUILD" -eq 1 ]; then
  if [ "$DRY_RUN" -eq 1 ]; then
    printf '+ %s\n' "$HERE/bundle.sh"
  else
    "$HERE/bundle.sh"
  fi
elif [ ! -d "$BUNDLE" ]; then
  echo "error: $BUNDLE does not exist; run without --no-build first" >&2
  exit 1
fi

echo "Quitting a running chargecap, if any..."
run pkill -x chargecap || true

echo "Installing to $DEST..."
run rm -rf "$DEST"
run ditto "$BUNDLE" "$DEST"

echo "Installing the daemon (needs sudo)..."
run sudo "$DEST/Contents/MacOS/chargecapd" install

echo "Launching chargecap..."
run open "$DEST"

echo "Done."
