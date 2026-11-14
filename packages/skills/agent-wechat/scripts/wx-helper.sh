#!/usr/bin/env bash
set -euo pipefail

# Locate the configured native CLI without splitting executable paths or argv.
WX_CMD=()
is_native() {
  local help
  help="$("$1" --help 2>/dev/null)" || return 1
  [[ "$help" == *accounts* && "$help" == *messages* && "$help" == *daemon* ]]
}
select_native() {
  local candidate script_dir repo_root
  if [[ -n "${AGENT_WECHAT_NATIVE_BINARY:-}" ]]; then
    if [[ -x "$AGENT_WECHAT_NATIVE_BINARY" ]] && is_native "$AGENT_WECHAT_NATIVE_BINARY"; then
      WX_CMD=("$AGENT_WECHAT_NATIVE_BINARY")
      return 0
    fi
    printf '%s\n' 'The configured AGENT_WECHAT_NATIVE_BINARY is not a usable native wx CLI.' >&2
    return 1
  fi
  candidate="$(type -P wx || true)"
  if [[ -n "$candidate" ]] && is_native "$candidate"; then
    WX_CMD=("$candidate")
    return 0
  fi
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
  repo_root="$(cd "$script_dir/../../../.." && pwd -P)"
  for candidate in "$repo_root/packages/native-client-rust/target/release/wx" "$repo_root/packages/native-client-rust/target/debug/wx"; do
    if [[ -x "$candidate" ]] && is_native "$candidate"; then
      WX_CMD=("$candidate")
      return 0
    fi
  done
  printf '%s\n' 'No native wx CLI found. Use a configured installation or set AGENT_WECHAT_NATIVE_BINARY.' >&2
  return 1
}
if ! select_native; then exit 2; fi
if [[ "${1:-}" == "--doctor" || "${1:-}" == "doctor" ]]; then
  if [[ "$#" != 1 ]]; then
    printf '%s\n' 'Usage: wx-helper.sh --doctor' >&2
    exit 2
  fi
  printf '%s\n' 'Native CLI:'
  "${WX_CMD[@]}" --version
  if "${WX_CMD[@]}" --json status >/dev/null; then
    printf '%s\n' 'Configured hub status: reachable.' 'Inspect accounts status for the intended account login.'
  else
    printf '%s\n' 'Configured hub status failed; inspect wx --json config show and the reported error.' >&2
    exit 1
  fi
  exit 0
fi
exec "${WX_CMD[@]}" "$@"
