#!/usr/bin/env bash
set -euo pipefail

# Generate TypeScript types from Rust definitions using ts-rs
# Usage: ./scripts/generate-types.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RUST_PKG="$ROOT_DIR/packages/agent-server-rust"
GENERATED_DIR="$ROOT_DIR/packages/shared/src/types/generated"

echo "Generating TypeScript types from Rust..."

# Clean previous generated files
rm -rf "$GENERATED_DIR"
mkdir -p "$GENERATED_DIR"

# Run ts-rs export tests with output directed to shared package
cd "$RUST_PKG"
TS_RS_EXPORT_DIR="$GENERATED_DIR" cargo test --quiet 2>/dev/null

# Fix imports: ts-rs generates `from "./Foo"` but NodeNext requires `from "./Foo.js"`
for f in "$GENERATED_DIR"/*.ts; do
  if grep -q 'from "./' "$f"; then
    perl -pi -e 's|from "(\./[^"]+)"|from "$1.js"|g' "$f"
  fi
done

# Create barrel file that re-exports all generated types
BARREL="$GENERATED_DIR/index.ts"
echo "// Auto-generated barrel file — do not edit manually" > "$BARREL"
echo "// Generated from packages/agent-server-rust/src/ia/types.rs via ts-rs" >> "$BARREL"
echo "" >> "$BARREL"

for f in "$GENERATED_DIR"/*.ts; do
  base="$(basename "$f" .ts)"
  if [ "$base" = "index" ]; then
    continue
  fi
  echo "export type { $base } from \"./$base.js\";" >> "$BARREL"
done

echo ""
echo "Generated types in $GENERATED_DIR:"
ls -1 "$GENERATED_DIR"
echo ""
echo "Done."
