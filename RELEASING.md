# Release Process

> **Package renamed (2026-08-31):** the OpenClaw extension moved from `@agent-wechat/wechat` to `@agent-wechat/agent-wechat` so the unscoped package basename matches the plugin id `agent-wechat` (OpenClaw validates plugin ids against the npm basename and hard-fails updates on mismatch; the official catalog also reserves the `wechat` id). The first version (0.11.15) was published manually with a one-off granular token — OIDC trusted publishing only covers packages that already exist — and the token was revoked afterward. See [Trusted Publishing](#trusted-publishing-oidc) for the CI publisher configuration. Once rollout is done, deprecate the old `@agent-wechat/wechat`: `npm deprecate @agent-wechat/wechat "Renamed to @agent-wechat/agent-wechat (plugin id agent-wechat); openclaw >=2026.8 reserves the wechat id for the official Tencent plugin."`


The native/npm release process publishes these artifacts:

1. npm CLI package: `@agent-wechat/cli`
2. npm OpenClaw extension: `@agent-wechat/agent-wechat`
3. Native executables and a checksum manifest on the matching GitHub release
4. Rust libraries on crates.io: `wechat-protocol`, `wechat-profile` and their
   required `wechat-media` and `wechat-platform` dependencies

## Prepare A Release

1. Add changelog entries:

```bash
pnpm changeset
```

2. Commit the generated changeset file with your code.

3. Merge to main. The changesets GitHub Action opens a "Version Packages" PR with bumped versions and changelogs.

   GitHub release notes include the CLI, OpenClaw extension and desktop changelog
   entries for that exact version. Empty fixed-group entries are omitted; a
   desktop-only release can leave both npm package entries empty.

4. Merge the Version Packages PR. GitHub automatically starts
   **Prepare native release assets** (`native-release.yml`) for that exact commit.
   The automatic run requires a merged PR from this repository's
   `changeset-release/main` branch. It builds and tests the configured targets, verifies
   source and checksums, and publishes the GitHub release assets. Existing
   `native-release` environment reviewer requirements still apply.

   Releases skip the development-only `wx-dev` upstream simulator and its smoke
   checks. PR validation runs the simulator on Linux x64, Windows x64 and macOS
   ARM64. Production Rust tests, native platform/media checks, packaged CLI
   checks and desktop acceptance still gate publication.

5. Successful native asset publication calls **Publish native runtime image** for
   the same commit/version. It tests and pushes Linux x64/ARM64 images and combines
   them into the versioned multi-architecture tag. Stable versions also update
   `latest`.

6. The same native release run packages and publishes the four Rust libraries,
   after the runtime image succeeds and the `native-release` environment permits
   publication. Each crate uses the native version and exact release source.
   Cargo publishes dependencies before their consumers. The other workspace
   crates and desktop package keep `publish = false`.

7. The same run automatically starts **Publish npm packages** once native
   assets, the multi-architecture image and all four Rust libraries succeed.
   The job verifies the native assets, embeds their manifest in the CLI package
   and publishes both npm packages with provenance. Prereleases use their
   Changesets channel (such as `beta`). Stable releases use `latest`.

## Rust library release contract

`wechat-protocol` and `wechat-profile` implement verified WeChat protocol and
official-profile behavior. Preserve that boundary: daemon scheduling,
authorization policy, event delivery, sidecar coordination and OpenClaw features
belong in the higher layers. Do not add alternative settled storage, retention
or feature semantics to these libraries merely because the daemon needs them.
Use the repository's protocol-evidence and official-state rules when behavior
changes; incomplete analysis is not a reason to invent replacement behavior.

For each release affecting the public libraries:

- Review public functions, types, fields, error variants, feature flags and
  documented behavior for compatibility. Preserve existing callers when it makes
  sense; explain intentional breaks in the changeset and use a compatible release
  version. Beta APIs may change, but changes still need release notes.
- Review dependencies as part of the public interface. Prefer dependencies that
  fit the library's job, avoid leaking daemon internals through public types,
  and assess new native build/runtime requirements and licenses. Do not upgrade
  unrelated dependencies just to cut a release.
- Keep the four public crates' versions and internal dependency requirements
  synchronized through `pnpm version-packages`. A public library may not depend
  on an unpublished workspace crate. Required support crates remain published
  even when users normally depend only on protocol/profile.
- Run the pinned formatter, warning-denying Clippy, relevant library tests and
  public examples. `node scripts/native-crates.mjs package` builds the actual
  four source archives, including their packaged native sources and licenses;
  a workspace build alone is insufficient. Keep the reference page and crate
  documentation aligned with the shipped API and supported profile layouts.

### One-time crates.io setup

The names must be owned by this project's maintainer. crates.io currently
requires an API token for a crate's first publication. Before the first release,
create a short-lived token with only the permissions required to publish these
four crates and store it as `CRATES_IO_BOOTSTRAP_TOKEN` in GitHub's protected
`native-release` environment. Never commit or print it. The workflow uses this
token only while at least one of the four crate names has not been created.

After the first publication, configure each crate's Settings → Trusted
Publishing with repository owner `thisnick`, repository `agent-wechat`, workflow
filename `native-release.yml`, and environment `native-release`. The caller
workflow supplies the OIDC identity even though its job uses `rust-crates.yml`.
Then revoke the bootstrap token and remove the environment secret. Later
releases use the official Rust trusted-publishing action and its temporary token.

PRs build packages without credentials or registry writes. Publication requires
the exact native release tag, clean source checkout, matching channel and the
protected environment. Do not manually publish an old binary version from newer
source. The first crate version is the next release containing this change.

For a failed or uncertain upload, inspect crates.io before retrying the failed
native release job at the same commit. The job verifies existing versions against
the exact validated archive checksums and Git source, then publishes only missing
versions. A different existing source/checksum, registry outage or yanked version
stops recovery. Never overwrite, delete or bump another version to hide a partial
publication. The npm completion handoff waits for the native run, including this
crate job, to succeed.

## Switch to a prerelease channel

The prerelease channel is `beta`. From stable mode, run:

```bash
pnpm changeset pre enter beta
```

Changesets reports `Entered pre mode with tag beta` and creates
`.changeset/pre.json`. Merge that file before merging the Version Packages PR.
The release action updates that PR with versions such as `1.3.0-beta.0` and
synchronizes the CLI, Rust and desktop versions. Entering prerelease mode alone
does not change package versions or publish anything.

Review the regenerated Version Packages PR before merging it. Beta releases use
npm's `beta` tag, a versioned runtime image tag and a GitHub prerelease. They do
not move the stable npm or runtime image `latest` tags.

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

The `release.yml` workflow opens or updates the Changesets version PR.
The automatic `native-release.yml` run publishes from the version PR's merge
commit. The npm job verifies the complete native manifest and each binary
against that commit before publishing. Registry and checksum errors fail the job.

If a job fails, use **Re-run failed jobs** on that original native release run
after resolving the cause. Successful jobs stay in place. If npm publication
partially succeeded, Changesets skips versions already on the registry. A
manual dispatch on `main` with the version and `confirm_publish=true` remains
available for native recovery. npm publication runs only on the automatic
version-merge run. Preserve existing release assets when recovering a release.
The preflight refuses conflicting tags or published npm versions. Host deployment
is a separate operation.

## Trusted Publishing (OIDC)

Configure a GitHub Actions trusted publisher for both public npm packages
before the next release. npm requires a first successful publish within two
days of creating the configuration.

1. Open the [CLI package access settings](https://www.npmjs.com/package/@agent-wechat/cli/access).
2. Under **Trusted publishers**, add GitHub Actions with owner `thisnick`,
   repository `agent-wechat` and workflow filename `native-release.yml`.
   Leave **Environment** blank and enable **Allow npm publish**.
3. Repeat in the [OpenClaw package access settings](https://www.npmjs.com/package/@agent-wechat/agent-wechat/access).

Keep the old `release.yml` publisher until any in-flight releases using it
finish. CI uses the pinned npm CLI to exchange GitHub OIDC credentials and
attach provenance. The native signing and crates.io publisher settings stay
on their current workflows and environments.

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
