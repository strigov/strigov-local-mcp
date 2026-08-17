#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(dirname -- "$SCRIPT_DIR")

exec docker build --pull --tag strigov/local-files-mcp:1.0.0 "$PROJECT_DIR"
