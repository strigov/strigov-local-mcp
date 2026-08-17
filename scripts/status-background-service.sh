#!/bin/sh
set -eu

LABEL='com.strigov.openai-tunnel.local-files'
DOMAIN="gui/$(id -u)"
LOG_DIR='/Users/USER/Library/Logs/strigov-local-files-tunnel'

if /bin/launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; then
  printf 'LaunchAgent: loaded\n'
  /bin/launchctl print "$DOMAIN/$LABEL" | /usr/bin/grep -E 'state =|pid =|last exit code =' || true
else
  printf 'LaunchAgent: not loaded\n'
fi

if /usr/bin/curl -fsS 'http://127.0.0.1:8080/healthz' >/dev/null 2>&1; then
  printf 'Health: PASS\n'
else
  printf 'Health: FAIL\n'
fi

if /usr/bin/curl -fsS 'http://127.0.0.1:8080/readyz' >/dev/null 2>&1; then
  printf 'Readiness: PASS\n'
else
  printf 'Readiness: FAIL\n'
fi

printf '\nRecent stderr log:\n'
/usr/bin/tail -n 30 "$LOG_DIR/stderr.log" 2>/dev/null || true
