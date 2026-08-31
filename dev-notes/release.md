# Release Process

> **Package renamed (2026-08):** the OpenClaw extension moved from `@agent-wechat/wechat` to `@agent-wechat/agent-wechat` so the unscoped package basename matches the plugin id `agent-wechat` (OpenClaw derives/validates plugin ids from the npm basename; the official catalog also reserves the `wechat` id). First release under the new name must be published manually (`npm publish` from packages/openclaw-extension after `pnpm build`), because npm trusted publishing (OIDC) can only be configured once the package exists. After the first publish, configure the trusted publisher at https://www.npmjs.com/package/@agent-wechat/agent-wechat/access and deprecate the old package: `npm deprecate @agent-wechat/wechat "Renamed to @agent-wechat/agent-wechat (plugin id agent-wechat); openclaw >=2026.8 reserves the wechat id for the official Tencent plugin."`


This repo releases three artifacts together:

1. npm CLI package: `@agent-wechat/cli`
2. npm OpenClaw extension: `@agent-wechat/agent-wechat`
3. Docker image: `ghcr.io/thisnick/agent-wechat`

## Prepare A Release

1. Add changelog entries:

```bash
pnpm changeset
```

2. Commit the generated changeset file with your code.

3. Merge to main. The changesets GitHub Action opens a "Version Packages" PR with bumped versions and changelogs.

4. Merge the Version Packages PR to publish.

## What CI/CD Does

On merge of the Version Packages PR:

- Publishes `@agent-wechat/cli` and `@agent-wechat/agent-wechat` to npm with provenance.
- Builds and pushes `amd64` and `arm64` Docker images.
- Publishes a multi-arch manifest tag.

Docker tags:

- `<version>` (e.g., `0.2.0`)
- `latest`

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

If you need a different GHCR path, update the image name in `.github/workflows/release.yml`.
