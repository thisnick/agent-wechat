#!/usr/bin/env bash
set -euo pipefail
umask 077
: "${APPLE_CERTIFICATE:?Missing Apple certificate}"
: "${APPLE_CERTIFICATE_PASSWORD:?Missing Apple certificate password}"
KEYCHAIN="$RUNNER_TEMP/agent-wechat-signing.keychain-db"
P12="$RUNNER_TEMP/agent-wechat-signing.p12"
KEYCHAIN_PASSWORD=$(openssl rand -hex 32)
printf '::add-mask::%s\n' "$KEYCHAIN_PASSWORD"
security list-keychains -d user > "$RUNNER_TEMP/agent-wechat-original-keychains"
python3 - "$P12" <<'PY'
import base64, os, sys
with open(sys.argv[1], 'wb') as output:
    output.write(base64.b64decode(''.join(os.environ['APPLE_CERTIFICATE'].split()), validate=True))
os.chmod(sys.argv[1], 0o600)
PY
security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
security import "$P12" -k "$KEYCHAIN" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign -T /usr/bin/security
python3 - "$KEYCHAIN" "$RUNNER_TEMP/agent-wechat-original-keychains" <<'PYCODE'
import shlex, subprocess, sys
subprocess.run(['security', 'list-keychains', '-d', 'user', '-s', sys.argv[1], *shlex.split(open(sys.argv[2]).read())], check=True)
PYCODE
security set-key-partition-list -S apple-tool:,apple: -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN" >/dev/null
printf 'APPLE_KEYCHAIN=%s\n' "$KEYCHAIN" >> "$GITHUB_ENV"
printf 'APPLE_SIGNING_IDENTITY=Developer ID Application: Hao Gang Yu (248S352SRV)\n' >> "$GITHUB_ENV"
rm -f "$P12"
