#!/usr/bin/env bash
set -euo pipefail

# Watch Rust source, compile inside Docker on change, and hot-swap the
# binary in the running container (restarts only the server process).
#
# Usage:
#   pnpm dev:watch                     # auto-detect everything
#   pnpm dev:watch --container foo     # specify container

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RUST_DIR="$ROOT_DIR/packages/agent-server-rust"
BUILDER_IMAGE="rust:1.93-bookworm"
CACHE_VOLUME="agent-wechat-cargo-cache"

CONTAINER=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --container)
      CONTAINER="${2:-}"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      echo "Usage: $0 [--container name]" >&2
      exit 1
      ;;
  esac
done

# Auto-detect container
if [ -z "$CONTAINER" ]; then
  CONTAINER=$(docker ps --filter "name=agent-wechat" --format '{{.Names}}' | head -1)
  if [ -z "$CONTAINER" ]; then
    echo "No running agent-wechat container found. Start one with: pnpm dev" >&2
    exit 1
  fi
fi

# Detect container platform for docker run --platform
CONTAINER_ARCH=$(docker inspect --format '{{.Architecture}}' "$CONTAINER" 2>/dev/null || echo "")
case "$CONTAINER_ARCH" in
  amd64)  PLATFORM="linux/amd64" ;;
  arm64)  PLATFORM="linux/arm64" ;;
  *)
    case "$(uname -m)" in
      x86_64)        PLATFORM="linux/amd64" ;;
      aarch64|arm64) PLATFORM="linux/arm64" ;;
      *)
        echo "Unknown architecture." >&2
        exit 1
        ;;
    esac
    ;;
esac

echo "Watching $RUST_DIR"
echo "  Builder:   $BUILDER_IMAGE ($PLATFORM)"
echo "  Container: $CONTAINER"
echo "  Cache:     docker volume '$CACHE_VOLUME'"
echo ""

# Build inside a Docker container matching the target platform.
# Cargo registry + target dir cached in a named volume for fast incremental builds.
build_and_deploy() {
  echo "==> Building (docker, debug)..."
  docker run --rm \
    --platform "$PLATFORM" \
    -v "$RUST_DIR:/build:ro" \
    -v "$CACHE_VOLUME:/build/target" \
    -v "${CACHE_VOLUME}-registry:/usr/local/cargo/registry" \
    -w /build \
    "$BUILDER_IMAGE" \
    cargo build

  echo "==> Deploying to $CONTAINER"
  # Extract binary from cache volume via a temporary container
  local tmp_ct
  tmp_ct=$(docker create -v "$CACHE_VOLUME:/target:ro" "$BUILDER_IMAGE")
  docker cp "$tmp_ct:/target/debug/agent-server" - | docker cp - "$CONTAINER:/opt/agent-server/"

  # Extract binary locally for symbol resolution (debugger)
  local local_bin="$RUST_DIR/target/debug-remote"
  mkdir -p "$local_bin"
  docker cp "$tmp_ct:/target/debug/agent-server" "$local_bin/agent-server"

  docker rm "$tmp_ct" > /dev/null

  # Kill server process — entrypoint restart loop brings it back with new binary
  docker exec "$CONTAINER" pkill -f '/opt/agent-server/agent-server' 2>/dev/null || true
  echo "==> Server restarting with new binary"
}

# Write a temp deploy script that cargo watch will call
DEPLOY_SCRIPT=$(mktemp)
trap "rm -f $DEPLOY_SCRIPT" EXIT

cat > "$DEPLOY_SCRIPT" <<SCRIPT
#!/usr/bin/env bash
set -euo pipefail
$(declare -f build_and_deploy)
RUST_DIR="$RUST_DIR"
PLATFORM="$PLATFORM"
BUILDER_IMAGE="$BUILDER_IMAGE"
CACHE_VOLUME="$CACHE_VOLUME"
CONTAINER="$CONTAINER"
build_and_deploy
SCRIPT
chmod +x "$DEPLOY_SCRIPT"

cd "$RUST_DIR"
cargo watch -w src -w migrations -s "$DEPLOY_SCRIPT"
