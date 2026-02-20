# Release Process

This repo releases three artifacts together on tag push:

1. npm CLI package: `@agent-wechat/cli`
2. npm OpenClaw extension: `@agent-wechat/wechat`
3. Docker image: `ghcr.io/agent-wechat/agent-wechat`

## Prepare A Release

1. Add changelog entries:

```bash
pnpm changeset
```

2. Apply version bumps:

```bash
pnpm changeset:version
```

3. Commit the result.

4. Create and push a semver tag that matches package versions:

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

## What CI/CD Does On Tag

- Publishes `@agent-wechat/cli` and `@agent-wechat/wechat` to npm with provenance.
- Builds and pushes `amd64` and `arm64` images.
- Publishes a multi-arch manifest tag.

Docker tags:

- `vX.Y.Z`
- `vX.Y`
- `latest`

## Trusted Publishing

Configure npm trusted publishers for both packages to this repo/workflow.

No npm access token is required once trusted publishing is configured.

If you need a different GHCR path, update `IMAGE_NAME` in `.github/workflows/release.yml`.
