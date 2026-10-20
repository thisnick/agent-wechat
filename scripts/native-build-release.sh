#!/usr/bin/env bash
# Build local artifacts only; does not publish, upload, start a persistent service or touch an existing profile.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
OUT=${1:-"$ROOT/artifacts/native"}
VERSION=$(node -p "JSON.parse(require('fs').readFileSync('packages/cli/package.json')).version")
COMMIT=$(git rev-parse HEAD)
DIRTY=false
if [ -n "$(git status --porcelain)" ]; then DIRTY=true; fi
TARGET=x86_64-unknown-linux-gnu
if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != x86_64 ]; then
  echo 'Native release validation currently requires Linux x86_64 with glibc.' >&2
  exit 1
fi
node --input-type=module -e "import {targetFor} from './packages/cli/scripts/native-runtime.mjs'; targetFor();"
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
ASSET="$OUT/wx-v$VERSION-linux-x64-gnu"
# Refuse to overwrite an artifact set from another build.
if [ -e "$ASSET" ] || [ -e "$OUT/wx-v$VERSION-checksums.json" ]; then
  echo 'Release output already exists; choose a new empty output directory.' >&2
  exit 1
fi
# Build the release in Bookworm (glibc 2.36), not against ubuntu-latest's
# newer libc. Only source enters the builder; no account paths are mounted.
BUILDER_TAG="agent-wechat-native-release-build:local-${COMMIT:0:12}-$$"
docker build --platform linux/amd64 --target builder -f deploy/native/Dockerfile -t "$BUILDER_TAG" .
CONTAINER=$(docker create "$BUILDER_TAG")
trap 'docker rm "$CONTAINER" >/dev/null' EXIT
BINARY="$OUT/.wx-local-build-$$"
docker cp "$CONTAINER:/build/target/$TARGET/release/wx" "$BINARY"
install -m 755 "$BINARY" "$ASSET"
rm "$BINARY"
node scripts/native-release-manifest.mjs "$ASSET" "$VERSION" "$COMMIT" "$OUT/wx-v$VERSION-checksums.json" "$DIRTY"
python3 packages/native-client-rust/scripts/smoke.py "$ASSET"
echo "Native artifacts validated at $OUT (not published)"
