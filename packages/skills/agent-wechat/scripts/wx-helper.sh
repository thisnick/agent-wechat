#!/usr/bin/env bash
set -euo pipefail

# wx-helper.sh: agent-wechat CLI 探测、执行与健康诊断脚本
# 自动探测系统或工作区中可用的原生 wx 二进制，并转发所有参数

find_wx_binary() {
  # 1. 优先检查环境变量显式指定的路径
  if [ -n "${AGENT_WECHAT_NATIVE_BINARY:-}" ] && [ -x "$AGENT_WECHAT_NATIVE_BINARY" ]; then
    echo "$AGENT_WECHAT_NATIVE_BINARY"
    return 0
  fi

  # 2. 检查工作区 target 目录下的构建产物
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

  # 3. 检查系统全局 PATH 中的 wx (确保是 1.x 原生版)
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

  # 4. 回退至通过 pnpm cli 运行本地构建
  if command -v pnpm >/dev/null 2>&1 && [ -f "$REPO_ROOT/package.json" ]; then
    echo "pnpm --silent --prefix $REPO_ROOT cli --"
    return 0
  fi

  return 1
}

run_doctor() {
  echo "=================================================="
  echo "🔍 agent-wechat 环境与服务健康诊断 (Doctor Check)"
  echo "=================================================="

  local HAS_ERRORS=0

  # 1. 检测二进制文件
  echo -n "[1/5] 检测原生 wx 二进制文件... "
  if WX_BIN="$(find_wx_binary 2>/dev/null)"; then
    echo "✅ 成功 ($WX_BIN)"
    echo "      版本信息: $($WX_BIN --version 2>/dev/null || echo '未知')"
  else
    echo "❌ 失败: 未找到可用的 1.x 原生 wx 二进制！"
    echo "      👉 解决方案: 执行 'pnpm native:build' 或 'npm install -g @agent-wechat/cli@beta'"
    HAS_ERRORS=1
  fi

  # 2. 检测守护进程运行与端口
  echo -n "[2/5] 检测本地守护进程连接性 (127.0.0.1:6174)... "
  if command -v nc >/dev/null 2>&1 && nc -z -w 2 127.0.0.1 6174 2>/dev/null; then
    echo "✅ 正常监听中"
  else
    echo "⚠️ 未连接 (守护进程可能未启动)"
    echo "      👉 解决方案: 执行 'wx daemon start' 或 'wx serve'"
  fi

  # 3. 检测冲突进程（官方客户端单写入者排他保护）
  echo -n "[3/5] 检测官方微信进程冲突... "
  if pgrep -i "xwechat|wechat" >/dev/null 2>&1; then
    echo "⚠️ 警告: 检测到官方微信正在运行！"
    echo "      👉 解决方案: 接管官方 Profile 前请先执行 'killall -9 wechat xwechat'"
  else
    echo "✅ 无冲突进程"
  fi

  # 4. 检测微信服务端网络连通性
  echo -n "[4/5] 检测微信服务器网络连通性 (TCP 80)... "
  if command -v nc >/dev/null 2>&1; then
    if nc -z -w 3 long.weixin.qq.com 80 2>/dev/null; then
      echo "✅ 连接正常"
    else
      echo "❌ 无法直连 long.weixin.qq.com:80！"
      echo "      👉 解决方案: 检查本地防火墙或禁用拦截原始 TCP 的 HTTP 代理 ('unset http_proxy')"
      HAS_ERRORS=1
    fi
  else
    echo "ℹ️ 跳过 (无 nc 工具)"
  fi

  # 5. 检测账号与会话状态
  echo -n "[5/5] 检测当前登录会话状态... "
  if [ -n "${WX_BIN:-}" ]; then
    if STATUS_OUTPUT="$($WX_BIN --json status 2>/dev/null)"; then
      echo "✅ 状态查询成功"
      echo "      状态摘要: $STATUS_OUTPUT"
    else
      echo "⚠️ 会话未就绪或未登录"
      echo "      👉 解决方案: 执行 'wx auth login --official' 或 'wx auth login --strategy=qr'"
    fi
  else
    echo "⏭️ 跳过 (缺少二进制)"
  fi

  echo "=================================================="
  if [ $HAS_ERRORS -eq 0 ]; then
    echo "🎉 核心依赖检测通过！"
  else
    echo "⚠️ 检测到部分异常，请参考上方提示进行修复。"
  fi
  echo "=================================================="
  return $HAS_ERRORS
}

# 检查是否传入了 doctor 诊断命令
if [ "${1:-}" = "doctor" ] || [ "${1:-}" = "--doctor" ]; then
  run_doctor
  exit $?
fi

# 正常调用
if ! WX_EXEC="$(find_wx_binary)"; then
  echo "错误: 未找到可用的 wx 原生二进制文件。请先运行 'pnpm native:build' 或安装 @agent-wechat/cli。" >&2
  echo "💡 提示: 可运行 '$0 --doctor' 进行环境自检与诊断。" >&2
  exit 1
fi

exec $WX_EXEC "$@"
