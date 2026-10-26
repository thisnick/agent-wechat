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

The container packages the same daemon as the executable. It replaces the previous official-client-plus-UI-server runtime; it is optional when `wx serve` runs on the host. macOS ARM64 and Windows x64/ARM64 have native candidate jobs; machine acceptance and official-app profile compatibility are tracked in [the platform checklist](../../PLATFORM-ACCEPTANCE.md). musl is not provided. Protocol device identity belongs to the saved account/profile and is preserved independently of the host CPU architecture. Arbitrary official-build schemas still require separate evidence. Controlled live account validation was on x64; ARM64 CI exercises synthetic protocol/profile data and the real compiled runtime.

## CI build and validation

PR checks own routine validation; main runs the checks deliberately deferred from PRs. Distribution jobs use separate path filters, so an OpenClaw-only change does not rebuild the Rust CLI or desktop. Tests use local synthetic data and do not publish, log into WeChat or deploy.

| Check | Pull requests | Main after merging | Release preparation |
| --- | --- | --- | --- |
| TypeScript, UI and desktop JavaScript tests | Once on Linux x64 | No repeat | Reviewed version commit |
| Rust format and Clippy | Linux x64; desktop format once | No repeat | Required native checks on the release commit |
| Rust workspace tests and native daemon/simulator smoke | Linux x64 and ARM64 | No repeat | Both Linux architectures gate publication |
| Generated TypeScript contracts | Linux x64 | No repeat | Linux x64 |
| Connected UI/launcher smoke | Linux x64 | No repeat | Linux x64 and ARM64 |
| Pinned OpenClaw Gateway/model fixtures | Linux x64 only | No repeat | Reviewed version commit |
| CLI package, lifecycle, platform and container checks | All five CLI targets; containers on both Linux architectures | No full repeat; build only the CLI inputs needed below | All five CLI targets; runtime images tested before publication |
| Upstream simulator on macOS/Windows | macOS | Windows x64 and ARM64 | All three targets |
| Desktop native tests and credential stores | Linux x64, Windows x64, macOS ARM64 | Linux ARM64 and Windows ARM64 | All five targets |
| Desktop installers | Omitted | Unsigned deferred ARM installers | Signed packages; all five targets required |

Linux release binaries are built in Bookworm (glibc 2.36). The Dockerfile builds only `wx-native`; it does not repeat the Rust unit suite or compile the development simulator. Distribution CI still tests the actual packaged executable and container. A complete PR CLI matrix is merged only when native inputs changed; release preparation always requires the complete matrix.

Distribution and release preparation pass their CLI artifacts to desktop within the same workflow run. Desktop verifies the clean source commit, pinned compiler, target, Rust flags, checksum and executable identity before reuse. Standalone desktop runs build their own CLI. Windows dependency caches are restored even when the exact CLI executable is cached, and simulator/test builds omit debug symbols.

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

## Beta channel

The committed Changesets prerelease state selects `beta`; the pending major changeset plans `1.0.0-beta.0` for the public CLI/OpenClaw packages and the private bundled SDK. This implementation PR keeps current package versions; after its merge, CI generates the separate Version Packages PR with beta versions, changelogs and the Rust lockfile update. Beta publication does not require a local compiler.

`native-release-policy.mjs` requires matching package/prerelease state, protects the existing public stable npm channels, and supplies GitHub prerelease metadata. Native beta releases are not marked Latest; npm uses Changesets beta mode. `latest` remains on the stable container packages until stable promotion. Entering beta mode is configuration, not publication. Exit it through a reviewed `changeset pre exit` and stable version PR. The existing release workflows target main; stable maintenance on another branch requires an explicit workflow setup.

## Release preparation, before npm publication

This is an operator sequence for a future approved release, not an instruction to publish during implementation:

1. Changesets versions `@agent-wechat/cli`; `native-sync-version.mjs` synchronizes the Rust workspace and lockfile in the version commit. Review/merge that version change first.
2. Merging the Changesets **Version Packages** PR automatically starts **Prepare native release assets** for that exact main commit/version. A manual run with the version and publish confirmation remains available for recovery. Protect the `native-release` GitHub environment with required reviewers. It validates the versioned public-package changelogs before building, builds/tests all five OS/CPU targets on native workers, verifies and merges their manifests, then creates provenance attestations and uploads new exact-version assets. GitHub release notes include the exact CLI/OpenClaw entries generated by Changesets. It refuses to replace existing asset names.
3. The normal npm release workflow downloads the prepared complete manifest and host binary, requires their `sourceCommit` to match its own commit, verifies size/SHA-256 and executes the bounded native identity check. It embeds the exact checksums in the npm package before publishing. Missing or mismatched assets fail publication closed. Successful native asset and runtime image publication automatically retries that same Release push run; changing source requires a new version/asset set.
4. npm provenance and the manifest inside the npm tarball anchor installer hashes. Remote checksum files alone are not an independent authenticity guarantee. GitHub artifact attestations provide an additional source/workflow check (`gh attestation verify ASSET --repo thisnick/agent-wechat`); verify them when provisioning direct release binaries outside npm.
5. After native assets are published, **Prepare native release assets** calls **Publish native runtime image** for the same version/commit. It verifies prepared source assets, builds/tests each architecture without account data, pushes both digests with provenance/SBOM, and publishes the versioned multi-architecture tag. Stable versions also update `latest`. npm waits for successful image manifest publication. Manual image publication remains available for recovery; afterwards rerun the original npm Release push run. Host deployment remains a separate operator action.
6. Mark the retired public `@agent-wechat/wechaty-puppet` package deprecated in npm with a migration notice as a release-administration step. The private gateway has no npm release to deprecate. This implementation removes both from the workspace and publishing pipeline; it does not change the registry during validation.

The current package version may have earlier legacy-only releases. The new installer must not silently accept one of those as a native release. A future version must contain the matching new assets and embedded manifest; no `latest` fallback is used.

If npm publication partially succeeds, preserve the exact prepared assets/manifest and rerun the same version commit; already published packages are skipped. Image publication runs after native assets and before npm; a manual image workflow remains available for recovery. Do not rebuild and overwrite a published native asset to make its checksum agree. Release reruns that find existing asset names fail explicitly so the operator can compare retained artifacts rather than replace them.

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
