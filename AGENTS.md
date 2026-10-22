# Agent guide for agent-wechat

This repository provides a standalone native WeChat daemon, Rust CLI and OpenClaw channel. It communicates directly with WeChat and reads/writes supported official-format profiles without an official executable at runtime. Check implementation and validation before describing a capability as supported.

## Repository map

| Path | Responsibility |
| --- | --- |
| `packages/native-client-rust/` | Protocol, profile, sidecar store, account actors, HTTP daemon and Rust `wx` CLI |
| `packages/shared/` | Private native TypeScript HTTP/SSE SDK and generated Rust contracts |
| `packages/cli/` | npm `wx` launcher and verified executable installer |
| `packages/openclaw-extension/` | OpenClaw native hosting, event custody, model handoff and media |
| `deploy/native/` | Native-only image, Compose and optional user-service examples |
| `docs/` | Public documentation site |

Native routes/contracts live in `packages/native-client-rust/crates/wechat-api/`; run `pnpm generate-types` after exported Rust contract changes. The README is a concise user guide, not an exhaustive API specification. The former UI server, image, container CLI and Wechaty integrations are retired; Git history retains their implementation.

## Working rules

- Use neutral, accurate technical language in code, comments, docs, commits, and PRs. Do not imply unauthorized access or conceal how a feature works.
- Keep account credentials, tokens, private infrastructure details, and research-only material out of this public repository. Do not link to private research repositories from public docs.
- Use an isolated test container and test account for behavior changes. Do not send messages to a live bot or modify a production container unless the task explicitly calls for it. Confirm the intended destination before any send.
- Preserve existing container data, official profiles and backups unless removal is explicitly requested. Retiring runtime source does not authorize deleting deployed containers, registry tags or volumes.
- Use one writer per official profile. Stop the official app before takeover; a caller assertion is not proof that an unmanaged app stopped. Do not infer arbitrary official-build or live ARM64 compatibility from synthetic tests.
- Add a changeset for user-facing behavior or package changes. Documentation-only edits do not need one.

## Documentation site

`docs/` is for people setting it up, people using it from the CLI, OpenClaw or their own code, and people who want to know how it works. When a change affects users, update the one page that owns the topic; don't append a paragraph to whichever page is nearest.

- Start each page with the task or answer. Use numbered steps and show the command's output. Caveats and edge cases go in a "Good to know" aside at the end.
- Write plain sentences for a reader who hasn't seen the code. Avoid "explicit", "bounded", "durable", "custody", "projection" and similar internal terms unless the page defines them.
- Prefer a table, diagram (a `mermaid` code block, rendered at build time) or example over a paragraph.
- Keep task pages short, roughly 600 words; split a page that grows past that.
- Test evidence, CI scripts and release mechanics belong in `FEATURES.md`, `dev-notes/` or `deploy/native/README.md`, not in the site.
- Moving or renaming a page needs an entry in `moved` in `docs/astro.config.mjs` so old links keep working.

## Runtime boundaries

### Standalone native backend

Read `packages/native-client-rust/FEATURES.md` before claiming native compatibility. Native tests and live server acceptance do not establish official-client rendering or recipient playback. Keep account/profile identity admission, per-account mutation guards, accepted credential generations, logout fences and durable projection journals intact. Retain unknown sync commands rather than advancing the official projected cursor past them. Local repair uses already owned bytes and receipts; it must never resend or start a transfer. Profile handoff requires exclusive ownership and complete projection checks, or an explicit incomplete-copy request with reported gaps. Preserve unknown official files and never delete FTS databases as a general repair.

### Local data and media

Account registry/sidecars live under the configured `dataDir`; directly attached official profiles stay at their registered paths. Sidecar migrations belong in `crates/wechat-store/migrations/`; official schemas are checked before supported writers run. Never migrate an unknown official layout automatically.

Notifications do not imply that a message or its media is already available. Keep advertised and verified media variants separate. Cached verified bytes precede explicit expiry checks; missing expiry metadata or CDN 403/404 alone does not establish expiration. Never present raw XML as a retrieved attachment. Keep bounded pending/expired/unsupported/available responses and integrity/range checks intact. Local writeback repair uses owned bytes/receipts and must never resend or start a transfer.

Outgoing media/voice uses durable jobs and idempotency keys. Preserve per-note IDs, partial results and uncertain outcomes; stop with `needs_review` instead of blindly retrying a possibly accepted send. Keep account mutation guards, logout epochs and source bounds intact. Voice is independently encoded with the bundled SILK code; FFmpeg/FFprobe handle source preparation.

### Protobuf changes

Before changing a protobuf serializer or parser, cross-check field numbers, wire types, nested message boundaries, scalar types, packed fields and conditional presence against evidence for the supported platform/build. Check first, continuation and final requests for multi-request operations. Synthetic fixtures need an independently justified expected shape; reproducing our current output does not validate the protocol.

Maintainers with access to the private protocol documentation can set `WECHAT_PROTOCOL_REFERENCE_DIR` outside this checkout to an authorized local clone. Start with its `native-client-spec/protobuf/README.md` and `native-client-spec/protobuf/comparison/crosswalk.json`, then follow the relevant artifact evidence. Check the recorded source commit and package version: Android/Mac counterparts are comparison evidence, not interchangeable Linux schemas. Distinguish retained original names from inferred aliases; a literal value alone does not establish a field's meaning.

If the variable is unset, check `git config --local --get wechat.protocolReferenceDir`, then nearby workspace documentation checkouts. Identify a reference checkout by the two files above, not by its directory name. If several exist, check their branch/source snapshot before choosing one. The variable must point to the checkout root, not its `protobuf/` subdirectory.

If only remote access is available, use an already authenticated GitHub connector or CLI to find an accessible protocol-documentation repository containing those same paths; a code-search query is `path:native-client-spec/protobuf filename:README.md`. Read it through the connector, or clone it outside this public checkout for local review. Remote reads do not require the environment variable. Do not guess private URLs or require a new login merely to run public checks.

After verifying a local checkout, configure the current shell and optionally remember its path in this clone's untracked Git configuration:

```bash
export WECHAT_PROTOCOL_REFERENCE_DIR="/absolute/path/to/reference-checkout"
git config --local wechat.protocolReferenceDir "$WECHAT_PROTOCOL_REFERENCE_DIR"
# In a later shell, restore the value if the saved checkout is still valid:
export WECHAT_PROTOCOL_REFERENCE_DIR="$(git config --local --get wechat.protocolReferenceDir)"
```

Private access is optional. Public clones must build and test without that checkout or variable. When it is unavailable, use checked-in codec fixtures, public documentation and independently available matching artifacts, and record verification gaps in the PR. Maintainers can perform the private cross-check during review. Build/test scripts and CI must not automatically fetch private material or depend on it.

Keep private repository URLs, local reference paths, extracted schema catalogs, raw captures and account data out of public code, docs and PRs. Public regression tests should use synthetic values and describe the behavior they verify without requiring access to private evidence.

### OpenClaw integration

The extension consumes committed inbound events in `packages/openclaw-extension/src/native-monitor.ts`, stores attachments through `src/attachment-store.ts`, and applies media/catch-up rules in `src/media-delivery.ts`. Group mention policy is separate from direct-message policy. Voice transcription, when configured, uses OpenClaw's media-understanding runtime after message gating. Outbound media is handled in `src/outbound-media.ts`; `audioAsVoice` requests a voice job, while ordinary audio remains a file attachment. Preserve pending/unavailable status and file paths rather than claiming media reached the model when it did not.

## Development and verification

Rust work uses the toolchain in `rust-toolchain.toml`, currently **1.93.1**, with
**rustfmt and clippy**. Check those components before editing Rust: a successful
`cargo build` or `cargo test` does not establish that they are installed. A
`minimal` rustup installation alone omits both. Provision the full development
toolchain when preparing a workspace:

```bash
rustup toolchain install 1.93.1 --profile minimal --component rustfmt --component clippy
cargo +1.93.1 fmt --version
cargo +1.93.1 clippy --version
```

Before committing/pushing Rust changes, run the pinned formatter, then its check
and CI's warning-denying Clippy command after the final Rust edit. Run the
relevant tests as well; tests cannot substitute for these lint gates. If a
component is missing or a proxy blocks its download, resolve it using the pinned
installation, an existing cache or a configured remote development environment.
Report any remaining unrun check explicitly. Do not hand-format new Rust or use
repeated PR failures to discover formatter output. Keep the toolchain file and
CI workflow pins aligned when changing Rust versions.

```bash
pnpm install
pnpm build
pnpm typecheck
pnpm --filter @agent-wechat/agent-wechat test
cargo +1.93.1 fmt --all --manifest-path packages/native-client-rust/Cargo.toml
cargo +1.93.1 fmt --all --check --manifest-path packages/native-client-rust/Cargo.toml
cargo +1.93.1 clippy --workspace --all-targets --locked --manifest-path packages/native-client-rust/Cargo.toml -- -D warnings
cargo +1.93.1 test --workspace --locked --manifest-path packages/native-client-rust/Cargo.toml
bash scripts/native-generate-types.sh --check
```

Use `pnpm generate-types` after changing exported Rust types. CI runs tests and builds independently on native x64/ARM64 Linux workers. Prefer CI or an already configured remote builder for release/image builds; do not require local cross-compilers. Build images with `pnpm build:image:amd64` or `pnpm build:image:arm64` on a suitable worker. Builds never authorize deployment. For end-to-end changes, test the built image and CLI against an isolated container in addition to unit tests. The public CLI entry point is `wx`; `pnpm cli` runs the local build.

For user-facing changes, add a `.changeset/*.md` file with the affected package and appropriate patch/minor/major bump. Keep package docs and README commands aligned with the shipped CLI. Do not document stubs, unverified behavior, or planned features as working.
