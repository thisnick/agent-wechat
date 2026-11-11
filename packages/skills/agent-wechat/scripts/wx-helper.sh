#!/usr/bin/env bash
set -euo pipefail

# wx-helper.sh: agent-wechat CLI detection, execution, and health diagnostic script.
# Automatically detects available native wx binary in system or workspace, forwarding all arguments.

find_wx_binary() {
  # 1. Check explicit environment variable first
  if [ -n "${AGENT_WECHAT_NATIVE_BINARY:-}" ] && [ -x "$AGENT_WECHAT_NATIVE_BINARY" ]; then
    echo "$AGENT_WECHAT_NATIVE_BINARY"
    return 0
  fi

  # 2. Check workspace build targets
  local SCRIPT_DIR
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local REPO_ROOT
  REPO_ROOT="$(cd "$SCRIPT_DIR/../../../" && pwd)"

  if [ -x "$REPO_ROOT/packages/native-client-rust/target/release/wx" ]; then
    echo "$REPO_ROOT/packages/native-client-rust/target/release/wx"
    return 0
  fi

  if [ -x "$REPO_ROOT/packages/native-client-rust/target/debug/wx" ]; then
    echo "$REPO_ROOT/packages/native-client-rust/target/debug/wx"
    return 0
  fi

  # 3. Check globally installed wx in PATH (must be native 1.x)
  if command -v wx >/dev/null 2>&1; then
    local WX_PATH
    WX_PATH="$(command -v wx)"
    local VER
    VER="$("$WX_PATH" --version 2>/dev/null || echo "")"
    if [[ "$VER" =~ "1." ]] || [[ "$VER" =~ "wx" ]]; then
      echo "$WX_PATH"
      return 0
    fi
  fi

  # 4. Fall back to pnpm cli in local workspace
  if command -v pnpm >/dev/null 2>&1 && [ -f "$REPO_ROOT/package.json" ]; then
    echo "pnpm --silent --prefix $REPO_ROOT cli --"
    return 0
  fi

  return 1
}

run_doctor() {
  echo "=================================================="
  echo "🔍 agent-wechat Environment and Health Check"
  echo "=================================================="

  local HAS_ERRORS=0

  # 1. Detect native binary
  echo -n "[1/5] Checking native wx binary... "
  if WX_BIN="$(find_wx_binary 2>/dev/null)"; then
    echo "✅ found ($WX_BIN)"
    echo "      version: $($WX_BIN --version 2>/dev/null || echo 'unknown')"
  else
    echo "❌ failed: No usable native wx binary found!"
    echo "      👉 Resolution: Run 'pnpm native:build' or 'npm install -g @agent-wechat/cli@beta'"
    HAS_ERRORS=1
  fi

  # 2. Detect local daemon port
  echo -n "[2/5] Checking local daemon connectivity (127.0.0.1:6174)... "
  if command -v nc >/dev/null 2>&1 && nc -z -w 2 127.0.0.1 6174 2>/dev/null; then
    echo "✅ listening"
  else
    echo "⚠️ not listening (daemon may be stopped)"
    echo "      👉 Resolution: Run 'wx daemon start' or 'wx serve'"
  fi

  # 3. Detect conflicting official client processes
  echo -n "[3/5] Checking conflicting official WeChat processes... "
  if pgrep -i "xwechat|wechat" >/dev/null 2>&1; then
    echo "⚠️ warning: Official WeChat client is running!"
    echo "      👉 Resolution: Run 'killall -9 wechat xwechat' before taking over official profile"
  else
    echo "✅ no conflicting processes"
  fi

  # 4. Detect gateway server TCP 80 connectivity
  echo -n "[4/5] Checking WeChat gateway network connectivity (TCP 80)... "
  if command -v nc >/dev/null 2>&1; then
    if nc -z -w 3 long.weixin.qq.com 80 2>/dev/null; then
      echo "✅ reachable"
    else
      echo "❌ cannot connect to long.weixin.qq.com:80 directly!"
      echo "      👉 Resolution: Check firewall or unset HTTP proxy ('unset http_proxy')"
      HAS_ERRORS=1
    fi
  else
    echo "ℹ️ skipped (nc not installed)"
  fi

  # 5. Detect session status
  echo -n "[5/5] Checking session status... "
  if [ -n "${WX_BIN:-}" ]; then
    if STATUS_OUTPUT="$($WX_BIN --json status 2>/dev/null)"; then
      echo "✅ status query succeeded"
      echo "      summary: $STATUS_OUTPUT"
    else
      echo "⚠️ session not ready or not logged in"
      echo "      👉 Resolution: Run 'wx auth login --official' or 'wx auth login --strategy=qr'"
    fi
  else
    echo "⏭️ skipped (missing binary)"
  fi

  echo "=================================================="
  if [ $HAS_ERRORS -eq 0 ]; then
    echo "🎉 Core dependency checks passed!"
  else
    echo "⚠️ Some issues were detected. Please review recommendations above."
  fi
  echo "=================================================="
  return $HAS_ERRORS
}

if [ "${1:-}" = "doctor" ] || [ "${1:-}" = "--doctor" ]; then
  run_doctor
  exit $?
fi

if ! WX_EXEC="$(find_wx_binary)"; then
  echo "Error: No usable wx native binary found. Please run 'pnpm native:build' or install @agent-wechat/cli." >&2
  echo "💡 Tip: Run '$0 --doctor' to diagnose environment readiness." >&2
  exit 1
fi

exec $WX_EXEC "$@"
