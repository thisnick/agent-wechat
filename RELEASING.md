# Release Process

> **Package renamed (2026-08-31):** the OpenClaw extension moved from `@agent-wechat/wechat` to `@agent-wechat/agent-wechat` so the unscoped package basename matches the plugin id `agent-wechat` (OpenClaw validates plugin ids against the npm basename and hard-fails updates on mismatch; the official catalog also reserves the `wechat` id). The first version (0.11.15) was published manually with a one-off granular token — OIDC trusted publishing only covers packages that already exist — and the token was revoked afterward. The trusted publisher for the new package is configured at https://www.npmjs.com/package/@agent-wechat/agent-wechat/access (GitHub Actions: thisnick/agent-wechat, workflow release.yml), so CI publishes via OIDC as before. Once rollout is done, deprecate the old `@agent-wechat/wechat`: `npm deprecate @agent-wechat/wechat "Renamed to @agent-wechat/agent-wechat (plugin id agent-wechat); openclaw >=2026.8 reserves the wechat id for the official Tencent plugin."`


The native/npm release process publishes three artifacts:

1. npm CLI package: `@agent-wechat/cli`
2. npm OpenClaw extension: `@agent-wechat/agent-wechat`
3. Native executables and a checksum manifest on the matching GitHub release

## Prepare A Release

1. Add changelog entries:

```bash
pnpm changeset
```

2. Commit the generated changeset file with your code.

3. Merge to main. The changesets GitHub Action opens a "Version Packages" PR with bumped versions and changelogs.

4. Merge the Version Packages PR. The Release workflow checks the npm channel and
   reports **Waiting for native release assets** if the matching artifacts have
   not been published. This is an expected waiting state, not a failed build.

5. That merge automatically starts **Prepare native release assets**
   (`native-release.yml`) for the exact version commit. Only merged PRs from this
   repository's `changeset-release/main` branch qualify; ordinary pushes do not
   publish native assets. It builds and tests the configured targets, verifies
   source and checksums, and publishes the GitHub release assets. Existing
   `native-release` environment reviewer requirements still apply. For recovery,
   run the workflow manually on main with the version and `confirm_publish=true`.

   Releases skip the development-only `wx-dev` upstream simulator and its smoke
   checks. PR validation runs the simulator on Linux x64, Windows x64 and macOS
   ARM64. Production Rust tests, native platform/media checks, packaged CLI
   checks and desktop acceptance still gate publication.

6. Successful native asset publication calls **Publish native runtime image** for
   the same commit/version. It tests and pushes Linux x64/ARM64 images and combines
   them into the versioned multi-architecture tag. Stable versions also update
   `latest`. npm waits until image manifest publication succeeds.

7. Successful completion automatically retries the original Release push run for
   the same source commit. That run verifies the native assets, embeds their
   manifest in the CLI package, and publishes both npm packages with provenance.
   Prereleases use their Changesets channel (such as `beta`); stable releases use
   `latest`.

## Switch to a stable release

Run `pnpm changeset pre exit` and merge the generated `.changeset/pre.json`
change. The next Version Packages PR removes the prerelease suffixes, updates
the changelogs and synchronizes the CLI, Rust and desktop versions. Review that
PR before merging it to publish the stable release. Stable builds use npm's
`latest` tag, the runtime image's `latest` tag and a regular GitHub Release.

## Upgrade compatibility

Every release must accept the consumer state an earlier release left on disk.
The baseline is 1.0.0-beta.13: a release may not stop loading or dispatching a
consumer file written by it. Two tests hold this contract:

- `packages/native-client-rust/crates/wechat-core/src/native_compat_tests.rs`
  pins the daemon's message JSON in
  `crates/wechat-core/tests/fixtures/native-compat/messages.json`. Consumers save
  this JSON when a message arrives and compare it with a fresh read before
  dispatching, so any change to it (including an added field) is an upgrade risk.
- `packages/openclaw-extension/src/native-consumer-compat.test.ts` loads the
  frozen `test/fixtures/native-compat/consumer-state-1.0.0-beta.13.json` and
  dispatches its buffered group messages against the pinned daemon JSON.

To change the message JSON on purpose, regenerate the pinned file with
`UPDATE_NATIVE_COMPAT_FIXTURES=1 cargo +1.93.1 test -p wechat-core native_compat`
and make the extension test pass without touching the frozen consumer file.
Never regenerate a frozen consumer file. If a later release needs a newer
baseline as well, add another frozen file beside it.

The frozen file only holds messages its release could already read. When the
daemon learns to read something an earlier release saved as unsupported, such
as a quoted link card, the fresh read no longer matches the saved copy.
`sameEmbeddedQuote` in `native-quote.ts` accepts a quote that moves from
`unsupported` to `embedded` with the same source message; cover any similar
change with a unit test, since the frozen file can't show it.

## What CI/CD Does

The npm workflow waits until every configured native target, checksum manifest,
and third-party notice is uploaded and the same source commit has a successfully
published runtime image. Registry/API errors, mismatched source
commits and invalid checksums remain failures. A published CLI version permits
extension-only recovery without requiring another native artifact set.

The native completion handler only retries the original main-branch push run
from this repository. It does not publish npm directly from a `workflow_run`
event: that event's default-branch SHA may have advanced since the artifacts
were built. Retrying the original run preserves both its source SHA and its npm
OIDC/provenance context.

If automatic recovery fails, rerun the original Release push run for the native
manifest's exact source commit. Do not push an unrelated commit to retry
publication or replace existing release assets. If main advanced before native
assets were prepared, reconcile the version/source tag before starting another
native release; the preflight refuses conflicting tags or published npm versions.

Docker image publication and deployment remain separate from these workflows.

## Trusted Publishing (OIDC)

npm trusted publishing lets GitHub Actions publish without a long-lived token. Setup:

1. Go to https://www.npmjs.com/package/@agent-wechat/cli/access
2. Under "Trusted publishers", add GitHub Actions:
   - **Owner**: `thisnick`
   - **Repository**: `agent-wechat`
   - **Workflow**: `release.yml`
   - **Environment**: (leave blank)
3. Repeat for https://www.npmjs.com/package/@agent-wechat/agent-wechat/access

Once configured, delete the `NPM_TOKEN` secret from GitHub repo settings. The workflow uses OIDC automatically (requires npm >= 11.5.1, installed in CI).

**Note**: Trusted publishing can only be configured for packages that already exist on npm. For brand-new packages, the first publish must use a token.

## Desktop installers

`@agent-wechat/desktop` is a separate private workspace package on the native prerelease train. Its version, Rust manifest and Tauri configuration are synchronized by `version-packages`. The npm CLI remains a native executable launcher; it does not contain the desktop UI.

A merged version-package PR starts `native-release.yml`, which also calls `desktop.yml`. Desktop reuses the verified CLI artifacts from that same run. Desktop PR checks cover Linux x64, Windows x64 and macOS ARM64 and omit installers. Linux/Windows ARM64 validation runs during releases or manual validation, which builds unsigned installers for all five platforms.

Native distribution validation runs only on PRs; ordinary main-branch pushes do not repeat those builds. Linux Rust PR checks also use x64, while release checks retain both Linux architectures. Complete CLI manifest verification runs during release preparation.

Production signing runs only on trusted release refs; pull requests cannot use production signing. Existing `native-release` approval and source/version checks remain in effect. Publication waits for the Rust checks on the release commit, the complete CLI matrix, and successful desktop signing and notarization.

Windows uses the reusable workflow introduced in PR #278, with GitHub OIDC and the existing `windows-signing` environment. Keep its main/v* environment restrictions and profile-scoped Certificate Profile Signer identity. The environment supplies `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`, `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT` and `AZURE_SIGNING_PROFILE`. Application binaries, bundled CLI and media helpers are signed first; installers built from those signed inputs are signed in a second pass. Both passes use an x64 runner, including ARM64 builds, and require the expected publisher, valid signatures and verified timestamps. Installer packaging disables binary patching so it preserves the already signed application payloads, then verifies their signatures again.

macOS uses repository secrets `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID`. The certificate is a base64 PKCS#12 archive. Each signing job creates a temporary keychain, imports the archive, configures unattended codesign access and cleans up in an always-run step. `APPLE_SIGNING_IDENTITY` is the configured Developer ID Application identity, supplied as a workflow environment value. Reusable workflow callers pass secrets with `secrets: inherit`.

Tauri signs nested executables and the app with hardened runtime and timestamps. The release then requires notarization `Accepted`, staples and validates the app, recreates its ZIP, creates and signs the final DMG, and notarizes/staples/validates that DMG. Code signature and Gatekeeper assessments must pass. Rejected notarization retrieves its log and fails the release. Missing credentials are failures, not unsigned-release fallbacks.

Desktop checksums are computed after final signing, notarization, stapling and packaging. The publish job verifies every file against its platform manifest and source commit before combining the final checksum manifest. It never replaces existing release assets.

The first trusted prerelease run must report its CI URL, final artifact names, Windows publisher/signature/timestamp results, Apple `Accepted` results, and successful stapler/Gatekeeper validation. Download and run those exact GitHub assets on real desktops to validate fresh installation, keyring persistence, local and remote connections, CLI setup, restart/quit, WeChat login and media. Unsigned CI builds do not establish those acceptance results.

## Desktop update feed

Desktop releases require repository secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The corresponding public key is committed under `plugins.updater.pubkey` in the Tauri configuration. Keep a protected backup of the private key and password: installed apps trust this key. These credentials are separate from Apple certificates, Azure OIDC and hub keys. Never replace the public key without a migration for already installed apps.

Release jobs set the Tauri installer marker before OS signing and preserve it during later packaging. After Windows signing or Apple notarization/stapling, they sign each final updater package with its app version. Linux DEBs receive the same update signature. Verification checks the payload, trusted version comment and public key before generating checksums and the combined `desktop-updater.json`. All five platform fragments must be present. PR jobs do not sign production updates. The immutable feed lives on the version's GitHub Release, so prereleases do not depend on GitHub's stable-only `/releases/latest` redirect.

First installed-app acceptance must cover discovery on the intended release channel, a rejected modified payload/version, graceful local-hub shutdown, successful installation/restart, and retained credentials/profiles on macOS, Windows and Linux. Local fixture and native-window tests do not replace this two-release acceptance.
