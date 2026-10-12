#!/bin/sh
set -eu
PROJECT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$PROJECT_DIR"
sh scripts/build-native.sh
VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
IMAGE="dist/Strigov-Local-MCP-$VERSION-macos-$(uname -m).dmg"
hdiutil create -volname "Strigov Local MCP $VERSION" -srcfolder dist/installer-image -format UDZO -ov "$IMAGE"
hdiutil verify "$IMAGE"
shasum -a 256 "$IMAGE" > "$IMAGE.sha256"
printf 'Installer: %s/%s\n' "$PROJECT_DIR" "$IMAGE"
