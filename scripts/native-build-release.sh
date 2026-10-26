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
if [ "$(uname -s)" != Linux ]; then
  echo 'Native release validation requires a Linux worker.' >&2; exit 1
fi
case "$(uname -m)" in
  x86_64) ARCH=x64; PLATFORM=linux/amd64 ;;
  aarch64|arm64) ARCH=arm64; PLATFORM=linux/arm64 ;;
  *) echo 'Native release validation requires an x64 or ARM64 worker.' >&2; exit 1 ;;
esac
node --input-type=module -e "import {targetFor} from './packages/cli/scripts/native-runtime.mjs'; targetFor();"
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
ASSET="$OUT/wx-v$VERSION-linux-$ARCH-gnu"
# Refuse to overwrite an artifact set from another build.
if [ -e "$ASSET" ] || [ -e "$OUT/wx-v$VERSION-checksums.json" ]; then
  echo 'Release output already exists; choose a new empty output directory.' >&2
  exit 1
fi
# Build the release in Bookworm (glibc 2.36), not against ubuntu-latest's
# newer libc. Only source enters the builder; no account paths are mounted.
BUILDER_TAG=${NATIVE_RELEASE_BUILDER_IMAGE:-"agent-wechat-native-release-build:local-${COMMIT:0:12}-$$"}
if [ -n "${NATIVE_RELEASE_BUILDER_IMAGE:-}" ]; then
  # CI supplies its cached builder. Refuse artifacts from another commit
  # or architecture before attaching the current source identity to them.
  REVISION=$(docker image inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' "$BUILDER_TAG")
  IMAGE_PLATFORM=$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$BUILDER_TAG")
  if [ "$REVISION" != "$COMMIT" ] || [ "$IMAGE_PLATFORM" != "$PLATFORM" ]; then
    echo 'Release builder must match the current commit and native platform.' >&2
    exit 1
  fi
else
  docker build --platform "$PLATFORM" --target builder -f deploy/native/Dockerfile -t "$BUILDER_TAG" .
fi
CONTAINER=$(docker create "$BUILDER_TAG")
trap 'docker rm "$CONTAINER" >/dev/null' EXIT
BINARY="$OUT/.wx-local-build-$$"
docker cp "$CONTAINER:/build/target/release/wx" "$BINARY"
install -m 755 "$BINARY" "$ASSET"
rm "$BINARY"
node scripts/native-release-manifest.mjs "$ASSET" "$VERSION" "$COMMIT" "$OUT/wx-v$VERSION-checksums.json" "$DIRTY"
install -m 644 packages/cli/THIRD_PARTY_NOTICES.txt "$OUT/wx-v$VERSION-THIRD-PARTY-NOTICES.txt"
# Keep compiler provenance beside CI artifacts so desktop can reuse this binary.
# Local release builds do not need the additional handoff metadata.
if [ "${NATIVE_CI_ARTIFACT:-false}" = true ]; then
  COMPILER_FILE=$(mktemp)
  docker cp "$CONTAINER:/build/rustc-version" "$COMPILER_FILE"
  node scripts/native-ci-artifact.mjs record "$OUT" --compiler-file "$COMPILER_FILE"
  rm "$COMPILER_FILE"
fi
python3 packages/native-client-rust/scripts/smoke.py "$ASSET"
echo "Native artifacts validated at $OUT (not published)"
