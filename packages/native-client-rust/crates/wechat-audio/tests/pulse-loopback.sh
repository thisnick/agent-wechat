#!/usr/bin/env bash
# Run a generated-audio fixture in a dedicated container, with no physical devices.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../../../../.." && pwd)
BINARY=${1:?Supply the built isolated_loopback example path}
BINARY=$(realpath "$BINARY")
if [ ! -f "$BINARY" ] || [ ! -x "$BINARY" ]; then
  echo 'Expected an executable isolated_loopback example.' >&2
  exit 1
fi
image="agent-wechat-audio-fixture:$(date +%s)-$$"
container="agent-wechat-audio-fixture-$(date +%s)-$$"
# --rm removes only this new container, never existing services or volumes.
docker build --tag "$image" "$ROOT/packages/native-client-rust/crates/wechat-audio/tests/pulse"
docker run --rm --name "$container" --network none --read-only \
  --cap-drop ALL --security-opt no-new-privileges --pids-limit 64 --memory 256m --cpus 1 \
  --tmpfs /tmp:rw,nosuid,nodev,size=64m,mode=1777 \
  --mount "type=bind,src=$BINARY,dst=/probe,readonly" "$image"
