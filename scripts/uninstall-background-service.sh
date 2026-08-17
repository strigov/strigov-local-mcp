#!/bin/sh
set -eu

LABEL='com.strigov.openai-tunnel.local-files'
PLIST_TARGET="/Users/USER/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"

/bin/launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
if [ -f "$PLIST_TARGET" ]; then
  /bin/rm "$PLIST_TARGET"
fi

printf 'Background service removed. The runtime key remains in macOS Keychain.\n'
