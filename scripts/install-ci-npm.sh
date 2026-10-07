#!/usr/bin/env bash
set -euo pipefail

# Bootstrap the complete, pinned npm distribution without depending on the
# runner's npm installation or Corepack npm shim. Trusted publishing needs npm 11.
ci_npm_dir="$(mktemp -d "${RUNNER_TEMP:-/tmp}/agent-wechat-npm.XXXXXX")"
ci_npm_version=11.12.1
ci_npm_integrity='zcoUuF1kezGSAo0CqtvoLXX3mkRqzuqYdL6Y5tdo8g69NVV3CkjQ6ZBhBgB4d7vGkPcV6TcvLi3GRKPDFX+xTA=='
curl --fail --silent --show-error --location --retry 3 \
  "https://registry.npmjs.org/npm/-/npm-${ci_npm_version}.tgz" \
  --output "$ci_npm_dir/npm.tgz"
ci_npm_actual="$(openssl dgst -sha512 -binary "$ci_npm_dir/npm.tgz" | openssl base64 -A)"
if [[ "$ci_npm_actual" != "$ci_npm_integrity" ]]; then
  echo 'Pinned npm archive integrity check failed' >&2
  exit 1
fi
mkdir -p "$ci_npm_dir/package" "$ci_npm_dir/bin"
tar -xzf "$ci_npm_dir/npm.tgz" --strip-components=1 -C "$ci_npm_dir/package"
ln -s "$ci_npm_dir/package/bin/npm-cli.js" "$ci_npm_dir/bin/npm"
ln -s "$ci_npm_dir/package/bin/npx-cli.js" "$ci_npm_dir/bin/npx"
test "$("$ci_npm_dir/bin/npm" --version)" = "$ci_npm_version"
printf '%s\n' "$ci_npm_dir/bin" >> "${GITHUB_PATH:?GITHUB_PATH must be set}"
echo "Verified standalone npm ${ci_npm_version}"
