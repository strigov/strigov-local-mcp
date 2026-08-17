#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  printf 'Usage: %s tunnel_ID\n' "$0" >&2
  exit 2
fi

if ! command -v tunnel-client >/dev/null 2>&1; then
  printf 'tunnel-client is not installed. Install it from OpenAI Platform tunnel settings.\n' >&2
  exit 1
fi

if [ -z "${CONTROL_PLANE_API_KEY:-}" ]; then
  printf 'CONTROL_PLANE_API_KEY is not set in this shell.\n' >&2
  exit 1
fi

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
MCP_COMMAND="$SCRIPT_DIR/run-container.sh"

tunnel-client init \
  --sample sample_mcp_stdio_local \
  --profile strigov-local-files \
  --tunnel-id "$1" \
  --mcp-command "$MCP_COMMAND"

exec tunnel-client doctor --profile strigov-local-files --explain
