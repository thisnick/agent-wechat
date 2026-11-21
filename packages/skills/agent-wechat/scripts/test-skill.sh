#!/usr/bin/env bash
set -euo pipefail

skill_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
for file in SKILL.md references/cli-reference.md references/exit-codes.md references/troubleshooting.md examples/runbooks.md scripts/wx-helper.sh; do
  test -s "$skill_dir/$file"
done
grep -q '^name: agent-wechat$' "$skill_dir/SKILL.md"
grep -q '^description:' "$skill_dir/SKILL.md"
bash -n "$skill_dir/scripts/wx-helper.sh"

test_root="$(mktemp -d "${TMPDIR:-/tmp}/wechat-skill-test.XXXXXX")"
trap 'rm -rf -- "$test_root"' EXIT
mkdir -p "$test_root/bin with spaces" "$test_root/legacy"
native="$test_root/bin with spaces/native wx"
export WECHAT_SKILL_TEST_ARGS="$test_root/args"
cat >"$native" <<'WX'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --help) printf '%s\n' 'Native wx commands: accounts messages daemon'; exit 0 ;;
  --version) printf '%s\n' 'wx 2.3.2'; exit 0 ;;
esac
printf '%s\0' "$@" >>"$WECHAT_SKILL_TEST_ARGS"
printf '%s\n' '{"fixture":"private-test-result","state":"needs_review","id":"job_fixture"}'
exit "${WECHAT_SKILL_TEST_EXIT:-0}"
WX
chmod +x "$native"

# An explicit executable with spaces must preserve each argument.
export AGENT_WECHAT_NATIVE_BINARY="$native"
"$skill_dir/scripts/wx-helper.sh" --json --account fixture messages send wxid_fixture --file "/tmp/report with spaces.pdf" --idempotency-key fixture-intent >"$test_root/output"
printf '%s\0' --json --account fixture messages send wxid_fixture --file "/tmp/report with spaces.pdf" --idempotency-key fixture-intent >"$test_root/expected"
cmp "$test_root/expected" "$WECHAT_SKILL_TEST_ARGS"

# Preserve a nonzero outcome and its JSON; never retry the command.
: >"$WECHAT_SKILL_TEST_ARGS"
code=0
WECHAT_SKILL_TEST_EXIT=8 "$skill_dir/scripts/wx-helper.sh" --json jobs get job_fixture >"$test_root/output" || code=$?
test "$code" -eq 8
printf '%s\0' --json jobs get job_fixture >"$test_root/expected"
cmp "$test_root/expected" "$WECHAT_SKILL_TEST_ARGS"
grep -q '"state":"needs_review"' "$test_root/output"

# A diagnostic asks only the configured CLI for status and suppresses private JSON.
: >"$WECHAT_SKILL_TEST_ARGS"
"$skill_dir/scripts/wx-helper.sh" --doctor >"$test_root/doctor"
printf '%s\0' --json status >"$test_root/expected"
cmp "$test_root/expected" "$WECHAT_SKILL_TEST_ARGS"
if grep -q private-test-result "$test_root/doctor"; then exit 1; fi

# An invalid explicit override must fail instead of selecting another account's CLI.
: >"$WECHAT_SKILL_TEST_ARGS"
code=0
AGENT_WECHAT_NATIVE_BINARY="$test_root/missing" "$skill_dir/scripts/wx-helper.sh" --help >"$test_root/output" 2>&1 || code=$?
test "$code" -eq 2
test ! -s "$WECHAT_SKILL_TEST_ARGS"

# A legacy global command must not hide a native checkout build.
cat >"$test_root/legacy/wx" <<'WX'
#!/usr/bin/env bash
printf '%s\n' 'Legacy commands: up down screenshot'
WX
chmod +x "$test_root/legacy/wx"
checkout="$test_root/source checkout"
mkdir -p "$checkout/packages/skills/agent-wechat/scripts" "$checkout/packages/native-client-rust/target/release"
cp "$skill_dir/scripts/wx-helper.sh" "$checkout/packages/skills/agent-wechat/scripts/wx-helper.sh"
cp "$native" "$checkout/packages/native-client-rust/target/release/wx"
: >"$WECHAT_SKILL_TEST_ARGS"
(
  unset AGENT_WECHAT_NATIVE_BINARY
  export PATH="$test_root/legacy:/usr/bin:/bin"
  "$checkout/packages/skills/agent-wechat/scripts/wx-helper.sh" --json accounts list >"$test_root/output"
)
printf '%s\0' --json accounts list >"$test_root/expected"
cmp "$test_root/expected" "$WECHAT_SKILL_TEST_ARGS"
printf '%s\n' 'Skill package checks passed.'
