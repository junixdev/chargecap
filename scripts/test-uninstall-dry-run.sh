#!/usr/bin/env bash
# test-uninstall-dry-run.sh — check what scripts/uninstall.sh removes.
#
# Regression: a plain uninstall removed the app state directory, so the
# limit that "Limit enabled" restores was lost. Only --purge may remove it.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_SUPPORT="$HOME/Library/Application Support/chargecap"
FAILED=0

check() {
  local label=$1 expect=$2 needle=$3 output=$4
  if [ "$expect" = "present" ]; then
    if printf '%s' "$output" | grep -qF -- "$needle"; then
      echo "ok   $label"
    else
      echo "FAIL $label: expected to find $needle"
      FAILED=1
    fi
  elif printf '%s' "$output" | grep -qF -- "$needle"; then
    echo "FAIL $label: did not expect $needle"
    FAILED=1
  else
    echo "ok   $label"
  fi
}

plain="$("$HERE/uninstall.sh" --dry-run)"
purge="$("$HERE/uninstall.sh" --purge --dry-run)"

check "a plain uninstall keeps the app state" absent  "rm -rf $APP_SUPPORT" "$plain"
check "a plain uninstall keeps the daemon config" absent "sudo rm -rf /Library/Application Support/chargecap" "$plain"
check "--purge removes the app state" present "rm -rf $APP_SUPPORT" "$purge"
check "--purge removes the daemon config" present "sudo rm -rf /Library/Application Support/chargecap" "$purge"
check "a plain uninstall still removes the app" present "rm -rf /Applications/chargecap.app" "$plain"

exit "$FAILED"
