# Native build, release and deployment

These assets prepare the standalone native daemon. They do not include the official executable, X11, VNC, desktop automation, Docker socket access or ptrace privileges. Nothing here automatically deploys when this branch is merged. The existing UI/container backend has separate images and remains available through `wx-legacy`.

The enabled native distribution target is Linux x86_64 with glibc 2.36 or newer. Other targets require their own executable, dependency, account and profile-format validation before extending the release matrix. A successful synthetic/profile fixture test does not establish live protocol or official-client roundtrip behavior.

## Prepare and validate locally

From the repository root, install workspace dependencies and build the TypeScript legacy CLI, then:

```bash
pnpm --filter @agent-wechat/cli... build
pnpm --filter @agent-wechat/cli test
bash scripts/native-build-release.sh
node scripts/native-package-smoke.mjs artifacts/native
# Substitute the exact artifact version emitted by the build:
node --experimental-strip-types scripts/native-openclaw-smoke.mjs artifacts/native/wx-vVERSION-linux-x64-gnu
python3 scripts/native-service-verify.py artifacts/native/wx-vVERSION-linux-x64-gnu
docker build --platform linux/amd64 -f deploy/native/Dockerfile -t agent-wechat-native:local .
docker run --rm --network none --read-only --cap-drop ALL --security-opt no-new-privileges agent-wechat-native:local --json version
python3 scripts/native-container-smoke.py agent-wechat-native:local
```

The build script uses the Bookworm builder to keep the release glibc baseline at 2.36, refuses existing artifact output files, runs Rust tests and the local daemon smoke harness, validates version/native identity/target, and emits a raw executable plus a checksums manifest. Local uncommitted source is marked `sourceTreeDirty`; publication rejects those validation artifacts. It never uploads artifacts, accesses account credentials or starts a persistent service. The package smoke uses temporary directories and local assets. The image checks print executable identity and start/stop one disposable service with empty tmpfs state and no network/ports or account material. They do not start a persistent deployment.

The Dockerfile-specific ignore file excludes local target output and private data from its build context. Review dependency/base-image changes and pin approved image digests for an operator's reproducible production build. No cross-build support is implied by `--platform linux/amd64`.

## Release preparation, before npm publication

This is an operator sequence for a future approved release, not an instruction to publish during implementation:

1. Changesets versions `@agent-wechat/cli`; `native-sync-version.mjs` synchronizes the Rust workspace and lockfile in the version commit. Review/merge that version change first.
2. Run **Prepare native release assets** manually against that exact main commit/version with explicit publish confirmation. Protect the `native-release` GitHub environment with required reviewers. It tests the Rust workspace, npm launcher/installer and local tarball installation, then creates provenance attestations and uploads new exact-version assets. It refuses to replace existing asset names.
3. The normal npm release workflow downloads the prepared manifest and binary, requires their `sourceCommit` to match its own commit, verifies size/SHA-256 and executes the bounded native identity check. It embeds the exact checksums in the npm package before publishing. Missing or mismatched assets fail publication closed. Rerun that same release workflow after asset preparation; changing source requires a new version/asset set.
4. npm provenance and the manifest inside the npm tarball anchor installer hashes. Remote checksum files alone are not an independent authenticity guarantee. GitHub artifact attestations provide an additional source/workflow check (`gh attestation verify ASSET --repo thisnick/agent-wechat`); verify them when provisioning direct release binaries outside npm.
5. When image publication is authorized, run **Publish native runtime image** manually with the exact version and explicit confirmation. It requires the same prepared source assets, builds/tests the image without account data, then pushes only the versioned `-native` image with provenance/SBOM. It never restarts a host service. Deployment is a separate operator action.

The current package version may have earlier legacy-only releases. The new installer must not silently accept one of those as a native release. A future version must contain the matching new assets and embedded manifest; no `latest` fallback is used.

If publication fails after npm succeeds, preserve the exact assets/manifest and recover the remaining legacy artifact jobs from the same commit. Do not rebuild and overwrite a published native asset to make its checksum agree. Release reruns that find existing asset names fail explicitly so the operator can compare retained artifacts rather than replace them.

## Configuration and durable data

`server.user.toml` follows the Linux default `~/.config/agent-wechat/server/config.toml`. `server.toml` selects the explicit container locations:

| Path | Purpose |
| --- | --- |
| `/etc/agent-wechat/server/config.toml` | Read-only settings and secret-file references |
| `/var/lib/agent-wechat` | Persistent registry, managed official-format profiles and account sidecars |
| `/var/cache/agent-wechat` | Private disk-backed scratch snapshots, selected by TMPDIR; never an official profile |
| `/run/secrets/wechat-api-token` | Read-only API bearer token, independently provisioned |
| Explicit additional profile mount | Optional direct existing official-format operating store |

An account in managed-copy mode keeps its official tree under `accounts/<id>/profile/` and additional state under `accounts/<id>/agent/`. A directly attached profile stays at its registered external path; its sidecar still lives under the durable root. Installing/upgrading the executable must not remove these directories. A full backup includes durable sidecars and separately attached profile trees; a profile-only export omits consumer receipts and pending operations. Keys/native credentials are separate from the API bearer token; protect every secret reference and do not assume an ordinary sidecar is encrypted.

## Compose example: review before applying

`compose.yaml` deliberately uses Linux host networking with a loopback listener, no published port, no capabilities, a read-only image and an explicit persistent volume. The service does not restart automatically by default. Use a verified TLS reverse proxy or authenticated encrypted tunnel for cross-host access; do not bypass the daemon's protected transport policy to expose plaintext bearer authentication.

Before a future deployment, provision `secrets/api-token` with at least 32 random bytes and mode 0600, and ensure the service UID 10001 can read it. Local Docker Compose implements file secrets as bind mounts: secret `uid`/`mode` remapping is not portable. A root-owned unreadable secret must be fixed through explicit host provisioning, not by making it world-readable. Configure volume ownership for UID 10001 without changing an existing account's files implicitly. An empty named volume inherits the image directory ownership; an existing volume needs operator review.

The image precreates both data and scratch roots as UID/GID 10001, mode 0700, so fresh volumes inherit the permissions required by the store. `TMPDIR=/var/cache/agent-wechat` directs official read snapshots to the separate disk-backed scratch volume; the 64 MiB `/tmp` tmpfs is only small process scratch. The reader bounds each database at 2 GiB and each snapshot at 8 GiB, including companion files. Size scratch for concurrent accounts (one full snapshot per active account, plus SQLite temporary work and headroom); provision a filesystem quota and free-space monitoring on the deployment host. Portable Compose local named volumes do not enforce a disk quota, and this example does not claim otherwise. A quota/storage failure must surface as a read failure, never as permission to work against the live source instead.

Normal snapshot teardown removes its temporary directory. A crash can leave private `agent-wechat-read-*` directories; retain them for diagnosis or remove only those disposable snapshot directories while the daemon is stopped. Never mount an official profile or the authoritative data root at the scratch path, and never run indiscriminate cleanup over either source. Scratch is not an authoritative store or backup; credentials and profile content can still be sensitive there, so keep it private.

A direct external profile requires an additional explicit mount at the same registered path and the daemon needs legitimate write permission there. Register it while both the official app and daemon are stopped; `--source-stopped` is a caller assertion, not proof that an unmanaged app has stopped. Do not run both writers. Import/export require exclusive offline ownership and a new destination. Preserve source trees and backups.

Validate the template only with a temporary placeholder secret:

```bash
docker compose -f deploy/native/compose.yaml config --quiet
```

This validates configuration; it does not prove native login, account synchronization or successful image execution. `docker compose up`, registry pushes, production restarts and profile registration are deployment actions and are deliberately not included in the implementation validation.

## systemd user unit

`agent-wechat-native.service` is an example user unit using a separately installed native executable at `~/.local/bin/wx`. Provision the default durable directory, config and mode-0600 token before starting. The unit uses filesystem restrictions and a private temporary directory, with write access only to the default durable root. If `dataDir` differs or a direct official profile is used, explicitly extend `ReadWritePaths` and ensure the necessary parent paths exist. Do not weaken all filesystem restrictions to fix a missing path.

`python3 scripts/native-service-verify.py /absolute/path/to/wx` checks a temporary copy of this unit against the built executable, leaving every other setting unchanged. It does not install a binary or unit, enable a service, or start one. Direct `systemd-analyze --user verify deploy/native/agent-wechat-native.service` also works once the configured `~/.local/bin/wx` exists; systemd verifies that executable as well as unit syntax. A headless host may need an approved user-session/linger arrangement; setting that up is an operator deployment step. Stop the service before a source/profile snapshot or official-client handoff. Preserve data when rolling binaries back; a binary rollback is not a schema downgrade.
