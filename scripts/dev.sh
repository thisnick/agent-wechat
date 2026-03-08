#!/bin/bash
#
# Start the agent-wechat container in dev mode.
#
# Usage: pnpm dev
#
# Mounts tool scripts for live editing. The Rust binary is baked into
# the image — use `pnpm dev:deploy` to hot-swap it without rebuilding.
#

set -e

CONTAINER_NAME="agent-wechat"
DEFAULT_PORT=6174

# Determine architecture
ARCH=$(uname -m)
if [ "$ARCH" = "arm64" ] || [ "$ARCH" = "aarch64" ]; then
  IMAGE="agent-wechat:arm64"
else
  IMAGE="agent-wechat:amd64"
fi

# Get script directory and monorepo root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MONOREPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Paths to mount
DOCKER_TOOLS="$MONOREPO_ROOT/docker/tools"

# Auto-generate auth token if needed
TOKEN_DIR="$HOME/.config/agent-wechat"
TOKEN_PATH="$TOKEN_DIR/token"
if [ ! -f "$TOKEN_PATH" ]; then
  mkdir -p "$TOKEN_DIR"
  openssl rand -hex 32 > "$TOKEN_PATH"
  chmod 600 "$TOKEN_PATH"
  echo "Auth token generated: $TOKEN_PATH"
fi

# Stop any existing container
docker stop "$CONTAINER_NAME" 2>/dev/null || true
docker rm "$CONTAINER_NAME" 2>/dev/null || true

# Check if image exists
if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "Error: Image $IMAGE not found."
  echo "Run 'pnpm build:image:arm64' (or amd64) first to build the image."
  exit 1
fi

echo "Starting $CONTAINER_NAME in dev mode..."
echo "  Mounting: $DOCKER_TOOLS → /opt/tools"

# Always expose gdb port for on-demand attach
echo "  Debug: gdbserver attach available on port 1234"

docker run -d \
  --name "$CONTAINER_NAME" \
  --security-opt seccomp=unconfined \
  --cap-add=SYS_PTRACE \
  --cap-add=NET_ADMIN \
  ${PROXY:+-e "PROXY=$PROXY"} \
  -p "$DEFAULT_PORT:$DEFAULT_PORT" \
  -p 1234:1234 \
  -v "$CONTAINER_NAME-data:/data" \
  -v "$CONTAINER_NAME-wechat-home:/home/wechat" \
  -v "$DOCKER_TOOLS:/opt/tools" \
  -v "$TOKEN_PATH:/data/auth-token:ro" \
  "$IMAGE"

echo ""
echo "Dev container started!"
TOKEN=$(cat "$TOKEN_PATH")
echo "  API: http://localhost:$DEFAULT_PORT"
echo "  noVNC: http://localhost:$DEFAULT_PORT/vnc/?token=$TOKEN&autoconnect=true"
echo ""
echo "Waiting for server..."

for i in {1..30}; do
  if curl -s "http://localhost:$DEFAULT_PORT/health" >/dev/null 2>&1; then
    echo "Server is ready!"
    echo ""
    echo "Dev workflow:"
    echo "  - Edit Rust code, then run: pnpm dev:deploy"
    echo "  - Tool scripts are live-mounted (edit docker/tools/ directly)"
    echo "  - Type-check: cd packages/agent-server-rust && cargo watch -x check"
    exit 0
  fi
  sleep 1
  printf "."
done

echo ""
echo "Server did not become ready in time. Check logs with: docker logs $CONTAINER_NAME"
exit 1
