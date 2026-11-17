# Repository scripts

These instructions apply to `scripts/`. Follow the repository's root agent guide
as well.

`scripts/` holds tooling that no single package owns. Run scripts from the
repository root with the Node version in `.node-version`: several resolve paths
from the working directory.

## Where a script belongs

| Script | Home |
| --- | --- |
| A test or tool for one package | That package: `packages/client-ui/scripts/`, `packages/openclaw-extension/test/smoke/` or `packages/native-client-rust/scripts/`. Never `packages/cli/scripts/`: npm publishes that folder. |
| CI runner setup, caching and artifact handoff | `ci/` |
| Versioning, release builds, packaging, release notes and publication | `release/` |
| A smoke test that spans packages or checks a built executable, npm package, image or service unit | `smoke/` |
| Translation checks | `i18n/` (`pnpm i18n`) |
| Public interface listings | `interfaces/` (`pnpm interfaces`) |
| Rust-to-TypeScript contract generation | `generate-types.sh` (`pnpm generate-types`) |

Name a script for what it does. The folder already gives its category, so don't
repeat it or add a `native-` prefix. Put a script's unit test beside it as
`<name>.test.mjs` or `<name>.test.py`.

## Paths

Scripts in `ci/`, `release/` and `smoke/` sit two levels below the repository
root. Build paths from the script's own location at that depth:

| Language | Repository root |
| --- | --- |
| JavaScript | `resolve(import.meta.dirname, '../..')` |
| Python | `Path(__file__).resolve().parents[2]` |
| Bash | `$(cd "$(dirname "$0")/../.." && pwd)` |

Moving a script changes that depth, so:

1. Update its self-relative paths and imports. Imports of build output such as
   `packages/shared/dist/index.js` point at files Git doesn't track, so a search
   won't confirm them; run the script.
2. Search the repository, except CHANGELOGs, for the old path. Workflows,
   `package.json`, `RELEASING.md`, `deploy/native/README.md`, the root
   `AGENTS.md` and package READMEs all name scripts by path.
3. Run the script's tests and every script that imports it.

## CI wiring

CI runs most script tests by file name, so a new test runs only once you add it
to its job:

| Tests | Workflow and job |
| --- | --- |
| `release/*.test.mjs`, except `preflight`, `merge` and `image` | `ci.yml` `release-tools`; `release/crates.test.mjs` also runs in `rust-crates.yml` |
| `ci/*.test.*`, `release/build.test.py`, `release/{preflight,merge,image}.test.mjs`, `smoke/media-assertions.test.mjs` | `ci.yml` `helpers` |
| `interfaces/*.test.mjs` | `interfaces.yml` |
| `i18n/*.test.mjs` | `ci.yml` `ts` and `docs` |

Each workflow's top-level `paths` filter uses folder globs such as
`scripts/release/**`. The job-level `dorny/paths-filter` lists in `ci.yml` and
`native-distribution.yml` name scripts individually. If a script is missing
there, a PR that changes it won't run its job.

`native-release.yml` and the publish steps of `rust-crates.yml` run only during a
release, so no PR exercises them. Run the unit tests of the scripts they call and
check every path they name.

## Side effects

Most scripts read the checkout and write only to a temporary folder or a named
output folder. These do more. Don't run them against real services; test them
through their unit tests.

| Script | Effect |
| --- | --- |
| `release/preflight.mjs --reserve-tag` | Creates the version tag on GitHub, using `GH_TOKEN` |
| `release/crates.mjs publish` | Publishes crates to crates.io. `plan` and `publish` refuse to run outside the main-branch release workflow. |
| `release/crates-release.mjs prepare\|finish` | `prepare` creates the `crates-vVERSION` tag on GitHub; `finish` creates its GitHub release. Both refuse to run outside the main-branch release workflow. |
| `release/sync-version.mjs` | Rewrites package manifests, the desktop Tauri config and Cargo lockfiles. `pnpm version-packages` runs it. |
| `ci/install-npm.sh`, `release/trigger.mjs`, `release/crates-release.mjs trigger` | Need GitHub Actions files such as `GITHUB_PATH`, `GITHUB_ENV`, `GITHUB_OUTPUT` or `GITHUB_EVENT_PATH`, and fail without them |
| `ci/media-runtime.mjs DIR` | Downloads the desktop's pinned media helpers listed in `packages/desktop/media-assets.json` |
| `ci/claude-review-comment.mjs` | Creates or replaces the Claude review summary comment on a pull request. Only `claude-code-review.yml` calls it, through `actions/github-script`. |

## Smoke tests

The smoke tests use synthetic accounts, data and temporary folders. None of them
logs in to WeChat or sends a message.

| Command | Needs |
| --- | --- |
| `node scripts/smoke/platform.mjs /abs/path/to/wx` | A built `wx` |
| `node scripts/smoke/package.mjs DIR` | `pnpm install`, and artifacts from `release/build.sh` or `release/portable-build.mjs` in `DIR` |
| `node scripts/smoke/simulator.mjs` | Binaries from `cargo build --workspace --profile test`; `pnpm native:simulator:smoke` builds and runs it |
| `node --experimental-strip-types scripts/smoke/calls-api.mjs` | `pnpm build` and the same Rust binaries. `WX_BINARY` and `WX_CALLS_FIXTURE_BINARY` override their paths. |
| `python3 scripts/smoke/container.py IMAGE` | Docker and an image from `pnpm build:image` |
| `python3 scripts/smoke/service.py /abs/path/to/wx` | `systemd-analyze` with a running user session |
| `bash scripts/release/build.sh [DIR]` | Docker. Builds the Linux release in a Debian Bookworm container and runs `packages/native-client-rust/scripts/smoke.py` on it. |
