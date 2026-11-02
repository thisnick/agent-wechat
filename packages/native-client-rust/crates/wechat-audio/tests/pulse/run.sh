#!/usr/bin/env bash
set -euo pipefail
export XDG_CONFIG_HOME=/tmp/audio-config
export XDG_RUNTIME_DIR=/tmp/audio-runtime
mkdir -p "$XDG_CONFIG_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_CONFIG_HOME" "$XDG_RUNTIME_DIR"
export PULSE_SERVER="unix:$XDG_RUNTIME_DIR/pulse-socket"
pulseaudio --daemonize=no --use-pid-file=no --exit-idle-time=-1 --disable-shm \
  --log-target=stderr -n \
  --load="module-native-protocol-unix socket=$XDG_RUNTIME_DIR/pulse-socket auth-anonymous=1" \
  --load="module-null-sink sink_name=call_fixture format=float32le rate=48000 channels=1" \
  > /tmp/pulse.log 2>&1 &
fixture_pid=$!
trap 'kill "$fixture_pid" 2>/dev/null || true; wait "$fixture_pid" 2>/dev/null || true' EXIT
for _ in $(seq 1 100); do
  if [ -S "$XDG_RUNTIME_DIR/pulse-socket" ]; then break; fi
  kill -0 "$fixture_pid"
  sleep 0.02
done
/probe --isolated-fixture call_fixture.monitor call_fixture
