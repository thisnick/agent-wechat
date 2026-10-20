#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUTPUT="$ROOT/packages/shared/src/native.generated.ts"
TEMP=$(mktemp)
trap 'rm -f "$TEMP"' EXIT
cargo run --quiet --manifest-path "$ROOT/packages/native-client-rust/Cargo.toml" -p wechat-api --example generate_contracts > "$TEMP"
if [ "${1:-}" = --check ]; then
  cmp "$TEMP" "$OUTPUT" || { echo 'Native generated contracts are stale; run scripts/native-generate-types.sh.' >&2; exit 1; }
else
  install -m 644 "$TEMP" "$OUTPUT"
fi
