#!/usr/bin/env bash
set -euo pipefail

# Compile the Rust server inside Docker and deploy into a running container.
# Builds in debug mode by default (for debugging). Use --release for optimized builds.
# Usage:
#   ./scripts/dev-deploy.sh                 # debug build (default)
#   ./scripts/dev-deploy.sh --release       # release build
#   ./scripts/dev-deploy.sh --container abc # specify container name/id

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RUST_DIR="$ROOT_DIR/packages/agent-server-rust"
# Use Ubuntu 22.04-based builder to match runtime glibc (2.35)
BUILDER_IMAGE="agent-wechat-builder:latest"
CACHE_VOLUME="agent-wechat-cargo-cache"

CONTAINER=""
BUILD_MODE="debug"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --container)
      CONTAINER="${2:-}"
      shift 2
      ;;
    --release)
      BUILD_MODE="release"
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      echo "Usage: $0 [--container name] [--release]" >&2
      exit 1
      ;;
  esac
done

# Auto-detect container
if [ -z "$CONTAINER" ]; then
  CONTAINER=$(docker ps --filter "name=agent-wechat" --format '{{.Names}}' | head -1)
  if [ -z "$CONTAINER" ]; then
    echo "No running agent-wechat container found. Specify with --container" >&2
    exit 1
  fi
fi

# Detect container platform
CONTAINER_ARCH=$(docker inspect --format '{{.Architecture}}' "$CONTAINER" 2>/dev/null || echo "")
case "$CONTAINER_ARCH" in
  amd64)  PLATFORM="linux/amd64" ;;
  arm64)  PLATFORM="linux/arm64" ;;
  *)
    case "$(uname -m)" in
      x86_64)        PLATFORM="linux/amd64" ;;
      aarch64|arm64) PLATFORM="linux/arm64" ;;
      *) echo "Unknown architecture." >&2; exit 1 ;;
    esac
    ;;
esac

CARGO_ARGS="--release"
BINARY_DIR="release"
if [ "$BUILD_MODE" = "debug" ]; then
  CARGO_ARGS=""
  BINARY_DIR="debug"
fi

# Build the builder image (cached after first run)
echo "==> Ensuring builder image exists (Ubuntu 22.04 + Rust)"
docker build -q -t "$BUILDER_IMAGE" -f "$ROOT_DIR/docker/Dockerfile.builder" "$ROOT_DIR/docker"

echo "==> Building in Docker ($PLATFORM, mode=$BUILD_MODE)"
docker run --rm \
  --platform "$PLATFORM" \
  -v "$RUST_DIR:/build:ro" \
  -v "$CACHE_VOLUME:/build/target" \
  -v "${CACHE_VOLUME}-registry:/usr/local/cargo/registry" \
  -w /build \
  "$BUILDER_IMAGE" \
  cargo build $CARGO_ARGS

echo "==> Deploying to container: $CONTAINER"
# Extract binary from cache volume via a temporary container
TMP_CT=$(docker create -v "$CACHE_VOLUME:/target:ro" "$BUILDER_IMAGE")
docker cp "$TMP_CT:/target/$BINARY_DIR/agent-server" - | docker cp - "$CONTAINER:/opt/agent-server/"

# For debug builds, also extract binary locally for symbol resolution
if [ "$BUILD_MODE" = "debug" ]; then
  LOCAL_BIN="$RUST_DIR/target/debug-remote"
  mkdir -p "$LOCAL_BIN"
  docker cp "$TMP_CT:/target/$BINARY_DIR/agent-server" "$LOCAL_BIN/agent-server"
  echo "==> Debug binary extracted to $LOCAL_BIN/agent-server"
fi

docker rm "$TMP_CT" > /dev/null

# Kill server process — entrypoint restart loop brings it back with new binary
docker exec "$CONTAINER" pkill -f '/opt/agent-server/agent-server' 2>/dev/null || true
echo "==> Server restarting with new binary"
