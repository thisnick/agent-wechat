#!/usr/bin/env bash
set -euo pipefail

# ============================================
# Environment setup
# ============================================
export DISPLAY=${DISPLAY:-:99}
export QT_ACCESSIBILITY=${QT_ACCESSIBILITY:-1}
export QT_LINUX_ACCESSIBILITY_ALWAYS_ON=${QT_LINUX_ACCESSIBILITY_ALWAYS_ON:-1}
export GTK_MODULES=${GTK_MODULES:-gail:atk-bridge}
export WECHAT_HOME=${WECHAT_HOME:-/home/wechat}

# ============================================
# X11 setup
# ============================================
if [ "$(id -u)" -eq 0 ]; then
  mkdir -p /tmp/.X11-unix
  chown root:root /tmp/.X11-unix
  chmod 1777 /tmp/.X11-unix
fi

if [ -f /tmp/.X99-lock ]; then
  rm -f /tmp/.X99-lock
fi

# ============================================
# Transparent proxy via redsocks (optional)
# ============================================
if [ -n "${PROXY:-}" ]; then
  REDSOCKS_TYPE="http-connect"
  PROXY_VAL="$PROXY"

  # Strip optional scheme prefix
  if echo "$PROXY_VAL" | grep -q '^socks5://'; then
    REDSOCKS_TYPE="socks5"
    PROXY_VAL="${PROXY_VAL#socks5://}"
  elif echo "$PROXY_VAL" | grep -q '^http://'; then
    PROXY_VAL="${PROXY_VAL#http://}"
  fi

  # Parse user:pass@host:port or host:port
  PROXY_USER=""
  PROXY_PASS=""
  if echo "$PROXY_VAL" | grep -q '@'; then
    PROXY_USERINFO="${PROXY_VAL%%@*}"
    PROXY_HOSTPORT="${PROXY_VAL##*@}"
    PROXY_USER="${PROXY_USERINFO%%:*}"
    PROXY_PASS="${PROXY_USERINFO#*:}"
  else
    PROXY_HOSTPORT="$PROXY_VAL"
  fi

  # Split host:port
  PROXY_HOST=$(echo "$PROXY_HOSTPORT" | rev | cut -d: -f2- | rev)
  PROXY_PORT=$(echo "$PROXY_HOSTPORT" | rev | cut -d: -f1 | rev)

  echo "Configuring transparent proxy: $PROXY_HOST:$PROXY_PORT ($REDSOCKS_TYPE)"

  # Generate redsocks config
  cat > /tmp/redsocks.conf <<REDSOCKS_EOF
base {
    log_debug = off;
    log_info = on;
    daemon = on;
    redirector = iptables;
}

redsocks {
    local_ip = 127.0.0.1;
    local_port = 12345;
    ip = $PROXY_HOST;
    port = $PROXY_PORT;
    type = $REDSOCKS_TYPE;
$([ -n "$PROXY_USER" ] && echo "    login = \"$PROXY_USER\";")
$([ -n "$PROXY_PASS" ] && echo "    password = \"$PROXY_PASS\";")
}
REDSOCKS_EOF

  # Create dedicated user for redsocks (iptables uid exclusion prevents redirect loop)
  id -u redsocks >/dev/null 2>&1 || useradd -r -s /usr/sbin/nologin redsocks
  chown redsocks /tmp/redsocks.conf

  # Start redsocks as dedicated user
  su -s /bin/sh -c "redsocks -c /tmp/redsocks.conf" redsocks

  # iptables: redirect all outgoing TCP through redsocks
  # Skip redsocks' own traffic to prevent redirect loop
  # Skip local/private ranges to preserve internal services
  iptables -t nat -N REDSOCKS
  iptables -t nat -A REDSOCKS -m owner --uid-owner redsocks -j RETURN
  iptables -t nat -A REDSOCKS -d 0.0.0.0/8 -j RETURN
  iptables -t nat -A REDSOCKS -d 10.0.0.0/8 -j RETURN
  iptables -t nat -A REDSOCKS -d 127.0.0.0/8 -j RETURN
  iptables -t nat -A REDSOCKS -d 169.254.0.0/16 -j RETURN
  iptables -t nat -A REDSOCKS -d 172.16.0.0/12 -j RETURN
  iptables -t nat -A REDSOCKS -d 192.168.0.0/16 -j RETURN
  iptables -t nat -A REDSOCKS -d 224.0.0.0/4 -j RETURN
  iptables -t nat -A REDSOCKS -d 240.0.0.0/4 -j RETURN
  iptables -t nat -A REDSOCKS -p tcp -j REDIRECT --to-ports 12345
  iptables -t nat -A OUTPUT -p tcp -j REDSOCKS

  echo "Transparent proxy configured."
fi

# ============================================
# Start Xvfb
# ============================================
Xvfb "$DISPLAY" -screen 0 1280x800x24 &
sleep 1

# ============================================
# Start D-Bus session as wechat user
# This ensures AT-SPI socket is accessible to wechat
# ============================================
DBUS_OUTPUT=$(su -s /bin/bash -c "dbus-launch --sh-syntax" wechat)
eval "$DBUS_OUTPUT"
export DBUS_SESSION_BUS_ADDRESS

echo "D-Bus session (wechat user): $DBUS_SESSION_BUS_ADDRESS"

# ============================================
# Start fluxbox window manager
# ============================================
if command -v fluxbox >/dev/null 2>&1; then
  su -s /bin/bash -c "DISPLAY=$DISPLAY DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS HOME=$WECHAT_HOME fluxbox &" wechat
fi

# ============================================
# Start notification daemon (prevents crash on incoming messages)
# ============================================
if command -v dunst >/dev/null 2>&1; then
  su -s /bin/bash -c "DISPLAY=$DISPLAY DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS HOME=$WECHAT_HOME dunst &" wechat
fi

# ============================================
# Start accessibility daemon as wechat user
# ============================================
if [ -x /usr/libexec/at-spi-bus-launcher ]; then
  su -s /bin/bash -c "DISPLAY=$DISPLAY DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS HOME=$WECHAT_HOME /usr/libexec/at-spi-bus-launcher &" wechat
  sleep 1  # Give AT-SPI time to register
fi

# ============================================
# Start VNC (internal only, accessed via noVNC)
# ============================================
if [ "${ENABLE_VNC:-1}" = "1" ]; then
  # -shared: allow multiple connections (needed for vncdotool)
  # -xkb: better keyboard handling
  # -listen 127.0.0.1: internal only (noVNC/websockify connects locally)
  x11vnc -display "$DISPLAY" -forever -nopw -shared -xkb -rfbport 5900 -listen 127.0.0.1 &
fi

# ============================================
# Start noVNC (browser-based VNC via websockify)
# ============================================
if [ "${ENABLE_VNC:-1}" = "1" ] && [ -d /opt/novnc ]; then
  NOVNC_PORT="${NOVNC_PORT:-6080}"
  websockify --web /opt/novnc "$NOVNC_PORT" localhost:5900 &
  echo "noVNC: http://localhost:$NOVNC_PORT/vnc.html?autoconnect=true"
fi

# ============================================
# Start PulseAudio (for audio support)
# ============================================
if command -v pulseaudio >/dev/null 2>&1; then
  su -s /bin/bash -c "pulseaudio --start --exit-idle-time=-1" wechat || true
fi

# ============================================
# Start WeChat with auto-restart (background supervisor)
# Disable Qt HiDPI scaling so AT-SPI coordinates match actual screen pixels
# ============================================
(
  RESTART_DELAY=3
  MAX_RAPID_RESTARTS=5
  RAPID_WINDOW=60
  restart_count=0
  window_start=$(date +%s)

  while true; do
    su -s /bin/bash -c "DISPLAY=$DISPLAY \
      DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS \
      QT_ACCESSIBILITY=1 \
      QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1 \
      QT_AUTO_SCREEN_SCALE_FACTOR=0 \
      QT_ENABLE_HIGHDPI_SCALING=0 \
      QT_SCALE_FACTOR=1 \
      GTK_MODULES=gail:atk-bridge \
      HOME=$WECHAT_HOME \
      /usr/bin/wechat" wechat

    echo "WeChat exited ($?), restarting in ${RESTART_DELAY}s..."

    NOW=$(date +%s)
    if [ $((NOW - window_start)) -gt $RAPID_WINDOW ]; then
      restart_count=0
      window_start=$NOW
    fi
    restart_count=$((restart_count + 1))

    if [ $restart_count -ge $MAX_RAPID_RESTARTS ]; then
      echo "WeChat crash loop detected, backing off to 30s..."
      sleep 30
      restart_count=0
      window_start=$(date +%s)
    else
      sleep $RESTART_DELAY
    fi
  done
) &

# ============================================
# Initialize data directory
# ============================================
DB_PATH="${AGENT_DB_PATH:-/data/agent.db}"
if [ ! -f "$DB_PATH" ]; then
  echo "Initializing database at $DB_PATH..."
  mkdir -p "$(dirname "$DB_PATH")"
  chown wechat:wechat "$(dirname "$DB_PATH")"
fi

# ============================================
# Start agent-server (Rust binary, foreground)
# ============================================
echo "Starting agent-server on port ${AGENT_PORT:-6174}..."

# Run in a restart loop so `pkill agent-server` restarts it
# (used by dev-deploy/dev-watch to hot-swap the binary)
while true; do
  /opt/agent-server/agent-server &
  SERVER_PID=$!
  wait $SERVER_PID
  EXIT_CODE=$?
  # Exit cleanly on SIGTERM (container shutdown)
  if [ $EXIT_CODE -eq 143 ]; then
    exit 0
  fi
  echo "agent-server exited ($EXIT_CODE), restarting in 1s..."
  sleep 1
done
