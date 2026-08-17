#!/bin/sh
set -eu

WORK_ROOT='/Users/USER/Documents/Claude/Projects/Work (local)'
COWORK_ROOT='/Users/USER/Documents/Claude/Projects/Cowork (local)'
IMAGE='strigov/local-files-mcp:1.0.0'

for ROOT_PATH in "$WORK_ROOT" "$COWORK_ROOT"; do
  if [ ! -d "$ROOT_PATH" ]; then
    printf 'Required MCP root is unavailable: %s\n' "$ROOT_PATH" >&2
    exit 1
  fi
done

exec docker run \
  --rm \
  --interactive \
  --network none \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --pids-limit 128 \
  --memory 512m \
  --tmpfs /tmp:rw,noexec,nosuid,size=32m \
  --mount "type=bind,src=$WORK_ROOT,dst=/data/work,readonly" \
  --mount "type=bind,src=$COWORK_ROOT,dst=/data/cowork,readonly" \
  "$IMAGE"
