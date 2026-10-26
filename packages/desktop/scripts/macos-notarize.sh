#!/usr/bin/env bash
set -euo pipefail
umask 077
: "${APPLE_ID:?Missing Apple ID}"
: "${APPLE_PASSWORD:?Missing Apple app-specific password}"
: "${APPLE_TEAM_ID:?Missing Apple team ID}"
APP="$1"
OUT="$2"
VERSION="$3"
mkdir -p "$OUT"
submit() {
  local container="$1" result="$RUNNER_TEMP/notarization-result.json"
  local exit_status=0
  xcrun notarytool submit "$container" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait --output-format json > "$result" || exit_status=$?
  if python3 - "$result" <<'PYCODE'
import json, sys
try:
    result=json.load(open(sys.argv[1]))
except (ValueError, OSError):
    sys.exit(1)
print('Apple notarization:', result.get('status'), result.get('id'))
sys.exit(0 if result.get('status') == 'Accepted' else 1)
PYCODE
  then
    if [ "$exit_status" -eq 0 ]; then return 0; fi
  fi
  local id
  id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id", ""))' "$result" 2>/dev/null || true)
  if [ -n "$id" ]; then
    xcrun notarytool log "$id" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" || true
  fi
  return 1
}
stapler_with_retry() {
  local operation="$1" target="$2" result="$RUNNER_TEMP/stapler-result.log"
  local delay exit_status
  for delay in 0 5 15 30; do
    if [ "$delay" -gt 0 ]; then
      printf 'Retrying Apple ticket %s in %s seconds.\n' "$operation" "$delay"
      sleep "$delay"
    fi
    exit_status=0
    xcrun stapler "$operation" "$target" > "$result" 2>&1 || exit_status=$?
    cat "$result"
    if [ "$exit_status" -eq 0 ]; then
      rm -f "$result"
      return 0
    fi
    # Error 68 also covers permanent ticket errors. Retry only an identified
    # transient network failure, without resubmitting an accepted notarization.
    if [ "$exit_status" -ne 68 ] || ! grep -Eq 'NSURLErrorDomain Code=-(1001|1003|1004|1005|1006|1009)([^0-9]|$)' "$result"; then
      rm -f "$result"
      return "$exit_status"
    fi
  done
  rm -f "$result"
  return "$exit_status"
}
staple_and_validate() {
  stapler_with_retry staple "$1"
  stapler_with_retry validate "$1"
}
codesign --verify --deep --strict --verbose=2 "$APP"
codesign --display --verbose=4 "$APP"
UPLOAD="$RUNNER_TEMP/agent-wechat-notarization.zip"
ditto -c -k --keepParent "$APP" "$UPLOAD"
submit "$UPLOAD"
staple_and_validate "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
spctl --assess --type execute --verbose=4 "$APP"
# Recreate the distributable ZIP after the enclosed app has been stapled.
ditto -c -k --keepParent "$APP" "$OUT/AgentWechat-v$VERSION-macos-arm64.zip"
STAGE="$RUNNER_TEMP/agent-wechat-dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/AgentWechat.app"
ln -s /Applications "$STAGE/Applications"
DMG="$OUT/AgentWechat-v$VERSION-macos-arm64.dmg"
hdiutil create -volname AgentWechat -srcfolder "$STAGE" -ov -format UDZO "$DMG"
codesign --force --sign "$APPLE_SIGNING_IDENTITY" --timestamp "$DMG"
codesign --verify --strict --verbose=2 "$DMG"
submit "$DMG"
staple_and_validate "$DMG"
spctl --assess --type open --context context:primary-signature --verbose=4 "$DMG"
printf 'Apple app and DMG: notarization Accepted; stapler and Gatekeeper passed.\n' >> "$GITHUB_STEP_SUMMARY"
printf '%s\n' "AgentWechat-v$VERSION-macos-arm64.zip" "AgentWechat-v$VERSION-macos-arm64.dmg" >> "$GITHUB_STEP_SUMMARY"
rm -f "$UPLOAD" "$RUNNER_TEMP/notarization-result.json"
