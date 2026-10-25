#!/usr/bin/env bash
set -euo pipefail
if [ -f "$RUNNER_TEMP/agent-wechat-original-keychains" ]; then
  python3 - "$RUNNER_TEMP/agent-wechat-original-keychains" <<'PYCODE' || true
import shlex, subprocess, sys
subprocess.run(['security', 'list-keychains', '-d', 'user', '-s', *shlex.split(open(sys.argv[1]).read())], check=True)
PYCODE
fi
security delete-keychain "$RUNNER_TEMP/agent-wechat-signing.keychain-db" 2>/dev/null || true
rm -f "$RUNNER_TEMP/agent-wechat-signing.p12" "$RUNNER_TEMP/agent-wechat-original-keychains"
