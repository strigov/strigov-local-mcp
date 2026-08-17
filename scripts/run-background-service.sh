#!/bin/sh
set -eu

KEYCHAIN_SERVICE='com.strigov.openai-tunnel.local-files.runtime-key'
KEYCHAIN_ACCOUNT='strigov-local-files'
DOCKER_BIN='/usr/local/bin/docker'
TUNNEL_CLIENT_BIN='/opt/homebrew/bin/tunnel-client'

export HOME='/Users/USER'
export PATH='/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin'

if [ ! -x "$DOCKER_BIN" ]; then
  printf 'Docker CLI not found: %s\n' "$DOCKER_BIN" >&2
  exit 1
fi

if [ ! -x "$TUNNEL_CLIENT_BIN" ]; then
  printf 'tunnel-client not found: %s\n' "$TUNNEL_CLIENT_BIN" >&2
  exit 1
fi

if ! "$DOCKER_BIN" info >/dev/null 2>&1; then
  /usr/bin/open -gja Docker
  attempt=0
  while [ "$attempt" -lt 60 ]; do
    if "$DOCKER_BIN" info >/dev/null 2>&1; then
      break
    fi
    attempt=$((attempt + 1))
    /bin/sleep 2
  done
fi

if ! "$DOCKER_BIN" info >/dev/null 2>&1; then
  printf 'Docker Desktop did not become ready within 120 seconds.\n' >&2
  exit 1
fi

CONTROL_PLANE_API_KEY=$(
  /usr/bin/security find-generic-password \
    -a "$KEYCHAIN_ACCOUNT" \
    -s "$KEYCHAIN_SERVICE" \
    -w
)

if [ -z "$CONTROL_PLANE_API_KEY" ]; then
  printf 'Runtime API key is empty in macOS Keychain.\n' >&2
  exit 1
fi

export CONTROL_PLANE_API_KEY

exec /usr/bin/caffeinate -i \
  "$TUNNEL_CLIENT_BIN" run --profile strigov-local-files-background
