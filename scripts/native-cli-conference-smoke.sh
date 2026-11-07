#!/usr/bin/env bash
# Built CLI/API with generated sound; no host audio, profiles or runtime networking.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
CLI=$(realpath "${1:?Supply the built wx executable}")
FIXTURE=$(realpath "${2:?Supply the built wx-calls-fixture executable}")
test -x "$CLI" && test -x "$FIXTURE"
image="agent-wechat-cli-conference-fixture:$(date +%s)-$$"
container="agent-wechat-cli-conference-fixture-$(date +%s)-$$"
trap 'docker image rm "$image" >/dev/null 2>&1 || true' EXIT
docker build --tag "$image" "$ROOT/packages/native-client-rust/crates/wechat-audio/tests/pulse"
docker run --rm --name "$container" --network none --read-only \
  --cap-drop ALL --security-opt no-new-privileges --pids-limit 128 --memory 768m --cpus 2 \
  --tmpfs /tmp:rw,nosuid,nodev,size=128m,mode=1777 \
  --mount "type=bind,src=$CLI,dst=/testbin/wx,readonly" \
  --mount "type=bind,src=$FIXTURE,dst=/testbin/wx-calls-fixture,readonly" \
  --mount "type=bind,src=$ROOT/scripts/native-cli-conference-smoke.py,dst=/test/conference.py,readonly" \
  --entrypoint python3 "$image" /test/conference.py --wx /testbin/wx --fixture /testbin/wx-calls-fixture
