#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUTPUT="$ROOT/packages/shared/src/native.generated.ts"
TEMP=$(mktemp)
trap 'rm -f "$TEMP"' EXIT
# CI sets NATIVE_CONTRACTS_GENERATOR to the generate_contracts example it has
# already built with the whole workspace. `cargo run -p wechat-api` would
# recompile shared dependencies with that package's own feature set.
if [ -n "${NATIVE_CONTRACTS_GENERATOR:-}" ]; then
  "$NATIVE_CONTRACTS_GENERATOR" > "$TEMP"
else
  cargo run --quiet --manifest-path "$ROOT/packages/native-client-rust/Cargo.toml" -p wechat-api --example generate_contracts > "$TEMP"
fi
if [ "${1:-}" = --check ]; then
  cmp "$TEMP" "$OUTPUT" || { echo 'Native generated contracts are stale; run scripts/generate-types.sh.' >&2; exit 1; }
else
  install -m 644 "$TEMP" "$OUTPUT"
fi
