#!/usr/bin/env bash
# Build a macOS .dmg containing the zoio binary.
#
# Usage: ./scripts/build-dmg.sh [TAG]
#   TAG defaults to "v0.0.0-dev"

set -euo pipefail

TAG="${1:-v0.0.0-dev}"
VERSION="${TAG#v}"
BINARY="target/release/zoio"
BUILD_DIR="build"
APP_NAME="zoio"
DMG_NAME="${APP_NAME}-${TAG}-macos.dmg"

if [ ! -f "$BINARY" ]; then
    echo "ERROR: Release binary not found at $BINARY"
    echo "       Run 'cargo build --release' first."
    exit 1
fi

rm -rf "$BUILD_DIR/dmg-staging"
mkdir -p "$BUILD_DIR/dmg-staging"

# Copy binary into staging
cp "$BINARY" "$BUILD_DIR/dmg-staging/${APP_NAME}"
chmod +x "$BUILD_DIR/dmg-staging/${APP_NAME}"

# Add a symlink to /usr/local/bin for easy installation
# (drag the binary there, or just run it directly)
ln -s /usr/local/bin "$BUILD_DIR/dmg-staging/Install Here (usr-local-bin)"

# Build the DMG
rm -f "$BUILD_DIR/$DMG_NAME"
hdiutil create \
    -volname "$APP_NAME $VERSION" \
    -srcfolder "$BUILD_DIR/dmg-staging" \
    -ov \
    -format UDZO \
    "$BUILD_DIR/$DMG_NAME"

echo ""
echo "DMG created: $BUILD_DIR/$DMG_NAME"
SHA=$(shasum -a 256 "$BUILD_DIR/$DMG_NAME" | awk '{print $1}')
echo "SHA256: $SHA"
