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

6. Successful native asset publication calls **Publish native runtime image** for
   the same commit/version. It tests and pushes Linux x64/ARM64 images and combines
   them into the versioned multi-architecture tag. Stable versions also update
   `latest`. npm waits until image manifest publication succeeds.

7. Successful completion automatically retries the original Release push run for
   the same source commit. That run verifies the native assets, embeds their
   manifest in the CLI package, and publishes both npm packages with provenance.
   Prereleases use their Changesets channel (such as `beta`); stable releases use
   `latest`.

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
