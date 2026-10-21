# Native build, release and deployment

These assets prepare the standalone native daemon. They do not include the official executable, X11, VNC, desktop automation, Docker socket access or ptrace privileges. Nothing here automatically deploys when this branch is merged. The former official-client UI server/image, container CLI and Wechaty integrations are retired from the new distribution. Existing installed containers, volumes and published versions are not deleted.

## Distribution matrix

| Deliverable | x64 Linux | ARM64 Linux | Delivery / purpose |
| --- | --- | --- | --- |
| Rust `wx` executable (includes `serve`) | `wx-vVERSION-linux-x64-gnu` | `wx-vVERSION-linux-arm64-gnu` | GitHub release assets; glibc 2.36+, no Node/Docker needed |
| `@agent-wechat/cli` | Matching executable selected | Matching executable selected | Existing npm package with verified installer/launcher; Node 24.16+ |
| Native runtime image | `linux/amd64` | `linux/arm64` | One versioned multi-architecture `ghcr.io/thisnick/agent-wechat-native:vVERSION` tag; includes FFmpeg/FFprobe, no official client |
| `@agent-wechat/agent-wechat` | Managed or external daemon | Managed or external daemon | Existing public OpenClaw npm plugin; external mode can also run on other host platforms |
| `@agent-wechat/shared` | Source SDK | Source SDK | Existing private TypeScript package; bundled into OpenClaw, no npm publication |
| Rust library crates | Workspace/git/path | Workspace/git/path | `publish = false`; no crates.io distribution in this release |

The container packages the same daemon as the executable. It replaces the previous official-client-plus-UI-server runtime; it is optional when `wx serve` runs on the host. macOS/Windows/musl executables are not provided. Protocol device identity belongs to the saved account/profile and is preserved independently of the host CPU architecture. Arbitrary official-build schemas still require separate evidence. Controlled live account validation was on x64; ARM64 CI exercises synthetic protocol/profile data and the real compiled runtime.

## CI build and validation

**Native client** runs format, Clippy, the Rust workspace tests, generated-contract checks and a real daemon smoke on separate x64 and native ARM64 workers. **Native distribution validation** builds each release in Bookworm (glibc 2.36), installs an actual npm tarball from local assets, tests CLI lifecycle/account/receipt behavior, exercises managed/external OpenClaw hosting, builds/runs the container, and validates Compose/service templates. Pinned OpenClaw Gateway/model fixtures use local synthetic data. A final job combines both independently tested architecture manifests and checks source/version/hash consistency. These PR jobs do not publish, log into WeChat or deploy.

CI is the normal build path. An already configured remote Docker builder is also usable without provisioning a local compiler: run Docker against its existing context on a native worker. Do not include private worker addresses or credentials in this repository.

For an explicit local/worker reproduction:

```bash
pnpm install --frozen-lockfile
pnpm --filter @agent-wechat/cli build
pnpm --filter @agent-wechat/agent-wechat... build
pnpm --filter @agent-wechat/cli test
bash scripts/native-build-release.sh
node scripts/native-package-smoke.mjs artifacts/native
docker build -f deploy/native/Dockerfile -t agent-wechat-native:local .
docker run --rm --network none --read-only --cap-drop ALL --security-opt no-new-privileges agent-wechat-native:local --json version
python3 scripts/native-container-smoke.py agent-wechat-native:local
```

The build script requires Linux x64 or ARM64 and tests/runs its own native architecture. It refuses existing output files and emits an executable, single-architecture checksum fragment and codec notices; `native-merge-release.mjs` combines the independently tested fragments for publication. Dirty source is marked and rejected by publication. Tests use temporary directories without account material; no persistent service starts.

The runtime includes FFmpeg/FFprobe and independently compiled public SILK code. Direct binary operators must provision FFmpeg/FFprobe for audio/video source preparation. `wx licenses` and release notices preserve attribution. The Dockerfile-specific ignore file sends only reviewed build inputs. A `--platform` argument alone does not establish cross-architecture correctness; CI uses separate native workers.

## Release preparation, before npm publication

This is an operator sequence for a future approved release, not an instruction to publish during implementation:

1. Changesets versions `@agent-wechat/cli`; `native-sync-version.mjs` synchronizes the Rust workspace and lockfile in the version commit. Review/merge that version change first.
2. Run **Prepare native release assets** manually against that exact main commit/version with explicit publish confirmation. Protect the `native-release` GitHub environment with required reviewers. It validates the versioned public-package changelogs before building, builds/tests both architectures on native workers, verifies and merges the two manifests, then creates provenance attestations and uploads new exact-version assets. GitHub release notes include the exact CLI/OpenClaw entries generated by Changesets. It refuses to replace existing asset names.
3. The normal npm release workflow downloads the prepared complete manifest and host binary, requires their `sourceCommit` to match its own commit, verifies size/SHA-256 and executes the bounded native identity check. It embeds the exact checksums in the npm package before publishing. Missing or mismatched assets fail publication closed. Rerun that same release workflow after asset preparation; changing source requires a new version/asset set.
4. npm provenance and the manifest inside the npm tarball anchor installer hashes. Remote checksum files alone are not an independent authenticity guarantee. GitHub artifact attestations provide an additional source/workflow check (`gh attestation verify ASSET --repo thisnick/agent-wechat`); verify them when provisioning direct release binaries outside npm.
5. When image publication is authorized, run **Publish native runtime image** manually with the exact version and explicit confirmation. It requires the same prepared source assets, builds/tests each architecture without account data, pushes the two digests with provenance/SBOM, and combines them under the versioned `-native` tag. It never restarts a host service. Deployment is a separate operator action.
6. Mark the retired public `@agent-wechat/wechaty-puppet` package deprecated in npm with a migration notice as a release-administration step. The private gateway has no npm release to deprecate. This implementation removes both from the workspace and publishing pipeline; it does not change the registry during validation.

The current package version may have earlier legacy-only releases. The new installer must not silently accept one of those as a native release. A future version must contain the matching new assets and embedded manifest; no `latest` fallback is used.

If npm publication partially succeeds, preserve the exact prepared assets/manifest and rerun the same version commit; already published packages are skipped. Image publication is an independent manual workflow, with no remaining legacy artifact jobs. Do not rebuild and overwrite a published native asset to make its checksum agree. Release reruns that find existing asset names fail explicitly so the operator can compare retained artifacts rather than replace them.

`native-release-notes.mjs` reads the exact version from both public packages' `CHANGELOG.md` files after Changesets versioning has consumed the pending changeset files. It preserves Markdown links, nested lists and package headings, skips empty fixed-group entries and refuses missing/mismatched or entirely empty notes. Reruns update only the marked Changesets block in an existing GitHub release, retaining operator text outside it. Preview without publication with `node scripts/native-release-notes.mjs --output /tmp/agent-wechat-release-notes.md`; for a future version, version the packages/changelogs first. Old releases are not edited by running this preview.

## Existing-profile operating store

V1 uses an existing stopped official Linux profile. The default image starts a daemon with empty sidecars; it does not supply a usable account. Mount the complete selected official HOME separately, give the service user legitimate write permission, then select its container-visible path with `wx auth login --official=/profiles/official`. Keep the official app stopped. The account identity, device and every encrypted database must match accepted native login before writes begin.

For Compose, add an explicit bind to the `volumes` list (the source must already exist):

```yaml
- type: bind
  source: /absolute/stopped-official-home
  target: /profiles/official
  bind:
    create_host_path: false
```

Preserve the profile's ownership and backups when preparing permissions. For the user service, add its actual profile path to `ReadWritePaths`; daemon sidecars remain under the separate configured `dataDir`. Fresh-profile creation, copy export and official-app takeover are experimental extras outside this initial workflow.

## Configuration and durable data

`server.user.toml` follows the Linux default `~/.config/agent-wechat/server/config.toml`. `server.toml` selects the explicit container locations:

| Path | Purpose |
| --- | --- |
| `/etc/agent-wechat/server/config.toml` | Read-only settings and secret-file references |
| `/var/lib/agent-wechat` | Persistent registry, managed official-format profiles and account sidecars |
| `/var/cache/agent-wechat` | Private disk-backed scratch snapshots, selected by TMPDIR; never an official profile |
| `/run/secrets/wechat-api-token` | Read-only API bearer token, independently provisioned |
| Explicit additional profile mount | Existing official-format operating store for the v1 workflow |

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

`agent-wechat-native.service` is an optional systemd **user** unit. It runs `wx serve` for one signed-in OS user and lets systemd handle stop/restart and permissions. It is an alternative process owner to foreground `serve`, `wx daemon start`, Compose or OpenClaw managed mode; choose one. Nothing copies, enables or starts the unit automatically.

The example uses a separately installed native executable at `~/.local/bin/wx`. Provision the default durable directory, config and mode-0600 token before starting. The unit uses filesystem restrictions and a private temporary directory, with write access only to the default durable root. If `dataDir` differs or a direct official profile is used, explicitly extend `ReadWritePaths` and ensure the necessary parent paths exist. Do not weaken all filesystem restrictions to fix a missing path.

`python3 scripts/native-service-verify.py /absolute/path/to/wx` checks a temporary copy of this unit against the built executable, leaving every other setting unchanged. It does not install a binary or unit, enable a service, or start one. Direct `systemd-analyze --user verify deploy/native/agent-wechat-native.service` also works once the configured `~/.local/bin/wx` exists; systemd verifies that executable as well as unit syntax. A headless host may need an approved user-session/linger arrangement; setting that up is an operator deployment step. Stop the service before a source/profile snapshot or official-client handoff. Preserve data when rolling binaries back; a binary rollback is not a schema downgrade.
