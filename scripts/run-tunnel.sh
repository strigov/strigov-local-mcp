#!/bin/sh
set -eu

if [ -z "${CONTROL_PLANE_API_KEY:-}" ]; then
  printf 'CONTROL_PLANE_API_KEY is not set in this shell.\n' >&2
  exit 1
fi

exec tunnel-client run --profile strigov-local-files
