#!/usr/bin/env bash
set -euo pipefail

# test-skill.sh: Verify agent-wechat skill installation integrity and environment

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$SKILL_DIR/../../.." && pwd)"

echo "=================================================="
echo "🧪 agent-wechat Skill Integrity and Environment Test"
echo "=================================================="

FAILED=0

check_step() {
  local desc="$1"
  shift
  echo -n "👉 $desc ... "
  if "$@"; then
    echo "✅ passed"
  else
    echo "❌ failed"
    FAILED=$((FAILED + 1))
  fi
}

# 1. Verify SKILL.md and frontmatter
check_step "Check SKILL.md exists" test -f "$SKILL_DIR/SKILL.md"
check_step "Check name field in SKILL.md" grep -q "^name: agent-wechat" "$SKILL_DIR/SKILL.md"
check_step "Check description field in SKILL.md" grep -q "^description:" "$SKILL_DIR/SKILL.md"

# 2. Verify reference docs
check_step "Check references/cli-reference.md" test -s "$SKILL_DIR/references/cli-reference.md"
check_step "Check references/exit-codes.md" test -s "$SKILL_DIR/references/exit-codes.md"
check_step "Check references/troubleshooting.md" test -s "$SKILL_DIR/references/troubleshooting.md"
check_step "Check examples/runbooks.md" test -s "$SKILL_DIR/examples/runbooks.md"

# 3. Verify helper script permissions and execution
check_step "Check scripts/wx-helper.sh is executable" test -x "$SKILL_DIR/scripts/wx-helper.sh"
check_step "Test scripts/wx-helper.sh --json version" "$SKILL_DIR/scripts/wx-helper.sh" --json version >/dev/null

# 4. Verify workspace and global installation targets
check_step "Check workspace installation (.agents/skills/agent-wechat)" test -e "$REPO_ROOT/.agents/skills/agent-wechat/SKILL.md"
check_step "Check global installation (~/.gemini/config/skills/agent-wechat)" test -e "$HOME/.gemini/config/skills/agent-wechat/SKILL.md"

echo "=================================================="
if [ $FAILED -eq 0 ]; then
  echo "🎉 All checks passed. Skill installation and environment are fully ready."
else
  echo "⚠️ $FAILED check(s) failed. Please review the output above."
fi
echo "=================================================="
exit $FAILED
