#!/usr/bin/env bash
# Build a .msi installer using cargo-wix.
#
# Usage: ./scripts/build-msi.sh
#
# Prerequisites:
#   - Windows with WiX Toolset v3 installed
#   - cargo install cargo-wix

set -euo pipefail

if ! command -v cargo-wix &> /dev/null; then
    echo "Installing cargo-wix..."
    cargo install cargo-wix
fi

# Initialize WiX config if not present
if [ ! -d "wix" ]; then
    cargo wix init --no-build
fi

cargo wix --no-build --nocapture

echo ""
echo "MSI installer created in target/wix/"
ls -lh target/wix/*.msi
