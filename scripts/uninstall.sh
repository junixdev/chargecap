#!/usr/bin/env bash
# uninstall.sh — quit chargecap, re-enable charging, remove the daemon and
# the app, and clean up the LaunchAgent and app-support directory.
#
#   scripts/uninstall.sh            # keep the saved charge limit and the app state
#   scripts/uninstall.sh --purge    # also remove the saved charge limit
#   scripts/uninstall.sh --dry-run  # print every command, run nothing
#
# Needs `sudo` once, to uninstall chargecapd (SMC writes are root-only).
set -euo pipefail

PURGE=0
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --purge) PURGE=1 ;;
    --dry-run) DRY_RUN=1 ;;
    *) echo "error: unknown option $arg" >&2; exit 1 ;;
  esac
done

APP="/Applications/chargecap.app"
# Where crates/daemon/src/install.rs copies itself; not on PATH by default.
DAEMON_BIN="/usr/local/libexec/chargecapd"
AGENT_LABEL="io.github.junixdev.chargecap.app"
AGENT_PLIST="$HOME/Library/LaunchAgents/$AGENT_LABEL.plist"
APP_SUPPORT="$HOME/Library/Application Support/chargecap"
SYSTEM_APP_SUPPORT="/Library/Application Support/chargecap"

run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    printf '+ %s\n' "$*"
  else
    "$@"
  fi
}

echo "Quitting chargecap, if running..."
run pkill -x chargecap || true

if [ -x "$DAEMON_BIN" ] || [ "$DRY_RUN" -eq 1 ]; then
  echo "Uninstalling the daemon (needs sudo; re-enables charging)..."
  run sudo "$DAEMON_BIN" uninstall || true
else
  echo "$DAEMON_BIN not installed; skipping."
fi

echo "Removing $APP..."
run rm -rf "$APP"

echo "Removing the launch-at-login agent..."
run launchctl bootout "gui/$(id -u)/$AGENT_LABEL" || true
run rm -f "$AGENT_PLIST"

if [ "$PURGE" -eq 1 ]; then
  echo "Purging saved config..."
  run rm -rf "$APP_SUPPORT"
  run sudo rm -rf "$SYSTEM_APP_SUPPORT"
else
  # WARNING: keep both saved files. $APP_SUPPORT/app.json holds the limit
  # that "Limit enabled" restores, so removing it here made a reinstall
  # forget the user's choice, which --purge is meant to do.
  echo "Keeping $SYSTEM_APP_SUPPORT/config.json and $APP_SUPPORT/app.json (pass --purge to remove them)."
fi

echo "Done."
