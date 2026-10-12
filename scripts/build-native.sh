#!/bin/sh
set -eu
PROJECT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$PROJECT_DIR"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) RUNTIME_PLATFORM=darwin-arm64 ;;
  Darwin-x86_64) RUNTIME_PLATFORM=darwin-amd64 ;;
  *) printf 'Build this bundle on macOS.\n' >&2; exit 1 ;;
esac
python3 scripts/fetch-tunnel-runtime.py --platform "$RUNTIME_PLATFORM"
export MACOSX_DEPLOYMENT_TARGET=13.0
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build --remap-path-prefix=$PROJECT_DIR=/src/local-files"
cargo build --release --locked --bins
python3 scripts/package-macos.py
