#!/usr/bin/env bash
set -euo pipefail
export XDG_CONFIG_HOME=/tmp/audio-config
export XDG_RUNTIME_DIR=/tmp/audio-runtime
mkdir -p "$XDG_CONFIG_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_CONFIG_HOME" "$XDG_RUNTIME_DIR"
export PULSE_SERVER="unix:$XDG_RUNTIME_DIR/pulse-socket"
if [ -x /wx ]; then
  # Audio-enabled executables must still serve without any sound server.
  PULSE_SERVER=unix:/does-not-exist /wx serve --listen 127.0.0.1:0 --data-dir /tmp/empty-daemon \
    >/tmp/daemon-out.log 2>/tmp/daemon.log &
  daemon_pid=$!
  trap 'kill "$daemon_pid" 2>/dev/null || true; wait "$daemon_pid" 2>/dev/null || true' EXIT
  for _ in $(seq 1 100); do
    if grep -q '"state":"ready"' /tmp/daemon.log; then break; fi
    kill -0 "$daemon_pid"
    sleep 0.05
  done
  grep -q '"state":"ready"' /tmp/daemon.log
  kill "$daemon_pid"
  wait "$daemon_pid"
  trap - EXIT
  /wx calls dial --help | grep -q -- '--input-device'
  /wx calls answer --help | grep -q -- '--output-device'
fi
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
if [ -x /wx ]; then
  /wx --config /tmp/no-client-settings.toml --token-stdin --json calls devices </dev/null >/tmp/devices.json
  grep -q 'call_fixture.monitor' /tmp/devices.json
  grep -q '"id": "call_fixture"' /tmp/devices.json
  test ! -e /tmp/no-client-settings.toml
  echo 'CLI device discovery and audio-enabled headless startup passed.'
fi
/probe --isolated-fixture call_fixture.monitor call_fixture
