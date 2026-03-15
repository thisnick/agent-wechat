#!/usr/bin/env bash
# Sync the version from packages/agent-server-rust/package.json to Cargo.toml.
# Called by the version-packages script after changesets bumps versions.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUST_DIR="$SCRIPT_DIR/../packages/agent-server-rust"

VERSION=$(node -p "require('$RUST_DIR/package.json').version")

# Update the version in [package] section of Cargo.toml
sed -i "s/^version = \".*\"/version = \"$VERSION\"/" "$RUST_DIR/Cargo.toml"

echo "Synced Cargo.toml version to $VERSION"

# Update Cargo.lock to reflect the new version
if command -v cargo &> /dev/null; then
  (cd "$RUST_DIR" && cargo update --workspace)
  echo "Updated Cargo.lock"
else
  echo "Warning: cargo not found, Cargo.lock not updated" >&2
fi
