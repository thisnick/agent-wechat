#!/usr/bin/env bash
# Shared script to launch WeChat with the correct environment.
# Called by both entrypoint.sh (initial launch) and agent-server (restart after crash).
#
# Required env vars: DISPLAY, DBUS_SESSION_BUS_ADDRESS
# Optional env vars: WECHAT_HOME (defaults to /home/wechat)

set -euo pipefail

WECHAT_HOME="${WECHAT_HOME:-/home/wechat}"
WECHAT_USER="${WECHAT_USER:-wechat}"

exec su -s /bin/bash -c "DISPLAY=$DISPLAY \
  DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-} \
  QT_ACCESSIBILITY=1 \
  QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1 \
  QT_AUTO_SCREEN_SCALE_FACTOR=0 \
  QT_ENABLE_HIGHDPI_SCALING=0 \
  QT_SCALE_FACTOR=1 \
  GTK_MODULES=gail:atk-bridge \
  HOME=$WECHAT_HOME \
  /usr/bin/wechat" "$WECHAT_USER"
