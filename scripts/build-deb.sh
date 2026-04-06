#!/usr/bin/env bash
# Build a .deb package using cargo-deb.
#
# Usage: ./scripts/build-deb.sh
#
# Prerequisites: cargo install cargo-deb

set -euo pipefail

if ! command -v cargo-deb &> /dev/null; then
    echo "Installing cargo-deb..."
    cargo install cargo-deb
fi

cargo deb --no-build

echo ""
echo "DEB package created in target/debian/"
ls -lh target/debian/*.deb
