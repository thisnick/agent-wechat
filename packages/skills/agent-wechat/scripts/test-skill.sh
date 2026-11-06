#!/usr/bin/env bash
set -euo pipefail

# test-skill.sh: 验证 agent-wechat skill 安装完整性与可用性

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$SKILL_DIR/../../.." && pwd)"

echo "=================================================="
echo "🧪 agent-wechat Skill 安装与自检测试"
echo "=================================================="

FAILED=0

check_step() {
  local desc="$1"
  shift
  echo -n "👉 $desc ... "
  if "$@"; then
    echo "✅ 通过"
  else
    echo "❌ 失败"
    FAILED=$((FAILED + 1))
  fi
}

# 1. 验证 SKILL.md 存在且包含规范 Frontmatter
check_step "检查 SKILL.md 及其 Frontmatter" test -f "$SKILL_DIR/SKILL.md"
check_step "检查 SKILL.md 中的 name 字段" grep -q "^name: agent-wechat" "$SKILL_DIR/SKILL.md"
check_step "检查 SKILL.md 中的 description 字段" grep -q "^description:" "$SKILL_DIR/SKILL.md"

# 2. 验证各引用文档完整性
check_step "检查 references/cli-reference.md" test -s "$SKILL_DIR/references/cli-reference.md"
check_step "检查 references/exit-codes.md" test -s "$SKILL_DIR/references/exit-codes.md"
check_step "检查 references/troubleshooting.md" test -s "$SKILL_DIR/references/troubleshooting.md"
check_step "检查 examples/runbooks.md" test -s "$SKILL_DIR/examples/runbooks.md"

# 3. 验证辅助脚本可执行性
check_step "检查 scripts/wx-helper.sh 可执行权限" test -x "$SKILL_DIR/scripts/wx-helper.sh"
check_step "测试 scripts/wx-helper.sh --json version" "$SKILL_DIR/scripts/wx-helper.sh" --json version >/dev/null

# 4. 验证工作区安装位置与软链接
check_step "检查工作区安装 (.agents/skills/agent-wechat)" test -e "$REPO_ROOT/.agents/skills/agent-wechat/SKILL.md"

# 5. 验证全局安装位置与软链接
check_step "检查全局安装 (~/.gemini/config/skills/agent-wechat)" test -e "$HOME/.gemini/config/skills/agent-wechat/SKILL.md"

echo "=================================================="
if [ $FAILED -eq 0 ]; then
  echo "🎉 全部 10 项测试通过！Skill 安装与运行环境完全就绪。"
else
  echo "⚠️ 共有 $FAILED 项测试失败，请核实以上报错。"
fi
echo "=================================================="
exit $FAILED
