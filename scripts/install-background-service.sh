#!/bin/sh
set -eu

LABEL='com.strigov.openai-tunnel.local-files'
KEYCHAIN_SERVICE='com.strigov.openai-tunnel.local-files.runtime-key'
KEYCHAIN_ACCOUNT='strigov-local-files'
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(dirname -- "$SCRIPT_DIR")
PLIST_SOURCE="$PROJECT_DIR/launchd/$LABEL.plist"
PLIST_TARGET="/Users/USER/Library/LaunchAgents/$LABEL.plist"
LOG_DIR='/Users/USER/Library/Logs/strigov-local-files-tunnel'
RUNTIME_DIR='/Users/USER/.local/share/strigov-local-files-mcp'
SOURCE_PROFILE='/Users/USER/.config/tunnel-client/strigov-local-files.yaml'
BACKGROUND_PROFILE='strigov-local-files-background'
DOMAIN="gui/$(id -u)"

if [ ! -f "$PLIST_SOURCE" ]; then
  printf 'LaunchAgent template not found: %s\n' "$PLIST_SOURCE" >&2
  exit 1
fi

if [ ! -f "$SOURCE_PROFILE" ]; then
  printf 'Source tunnel profile not found: %s\n' "$SOURCE_PROFILE" >&2
  exit 1
fi

if [ -n "${CONTROL_PLANE_API_KEY:-}" ]; then
  /usr/bin/security add-generic-password \
    -a "$KEYCHAIN_ACCOUNT" \
    -s "$KEYCHAIN_SERVICE" \
    -l 'OpenAI MCP tunnel runtime key' \
    -w "$CONTROL_PLANE_API_KEY" \
    -U >/dev/null
  unset CONTROL_PLANE_API_KEY
elif ! /usr/bin/security find-generic-password -a "$KEYCHAIN_ACCOUNT" -s "$KEYCHAIN_SERVICE" >/dev/null 2>&1; then
  printf 'Paste the OpenAI tunnel runtime API key (input is hidden): ' >&2
  trap '/bin/stty echo 2>/dev/null || true' EXIT HUP INT TERM
  /bin/stty -echo
  IFS= read -r CONTROL_PLANE_API_KEY
  /bin/stty echo
  trap - EXIT HUP INT TERM
  printf '\n' >&2

  if [ -z "$CONTROL_PLANE_API_KEY" ]; then
    printf 'Runtime API key cannot be empty.\n' >&2
    exit 1
  fi

  /usr/bin/security add-generic-password \
    -a "$KEYCHAIN_ACCOUNT" \
    -s "$KEYCHAIN_SERVICE" \
    -l 'OpenAI MCP tunnel runtime key' \
    -w "$CONTROL_PLANE_API_KEY" \
    -U >/dev/null
  unset CONTROL_PLANE_API_KEY
fi

TUNNEL_ID=$(
  /usr/bin/awk '/^[[:space:]]*tunnel_id:/ { gsub(/"/, "", $2); print $2; exit }' "$SOURCE_PROFILE"
)

if [ -z "$TUNNEL_ID" ]; then
  printf 'Could not read tunnel_id from %s\n' "$SOURCE_PROFILE" >&2
  exit 1
fi

/bin/mkdir -p '/Users/USER/Library/LaunchAgents' "$LOG_DIR" "$RUNTIME_DIR"
/usr/bin/install -m 700 "$SCRIPT_DIR/run-background-service.sh" "$RUNTIME_DIR/run-background-service.sh"
/usr/bin/install -m 700 "$SCRIPT_DIR/run-container.sh" "$RUNTIME_DIR/run-container.sh"

/opt/homebrew/bin/tunnel-client init \
  --sample sample_mcp_stdio_local \
  --profile "$BACKGROUND_PROFILE" \
  --tunnel-id "$TUNNEL_ID" \
  --mcp-command "$RUNTIME_DIR/run-container.sh" >/dev/null

/usr/bin/install -m 600 "$PLIST_SOURCE" "$PLIST_TARGET"

/bin/launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true

if /usr/sbin/lsof -nP -iTCP:8080 -sTCP:LISTEN >/dev/null 2>&1; then
  printf 'Port 8080 is already in use. Stop the foreground tunnel with Ctrl+C, then run this installer again.\n' >&2
  exit 1
fi

/bin/launchctl bootstrap "$DOMAIN" "$PLIST_TARGET"
/bin/launchctl enable "$DOMAIN/$LABEL"
/bin/launchctl kickstart -k "$DOMAIN/$LABEL"

attempt=0
while [ "$attempt" -lt 30 ]; do
  if /usr/bin/curl -fsS 'http://127.0.0.1:8080/readyz' >/dev/null 2>&1; then
    printf 'Background MCP tunnel is ready.\n'
    printf 'Status: %s/status-background-service.sh\n' "$SCRIPT_DIR"
    printf 'Logs: %s\n' "$LOG_DIR"
    exit 0
  fi
  attempt=$((attempt + 1))
  /bin/sleep 2
done

printf 'LaunchAgent was installed, but readiness was not confirmed within 60 seconds.\n' >&2
printf 'Run %s/status-background-service.sh for diagnostics.\n' "$SCRIPT_DIR" >&2
exit 1
