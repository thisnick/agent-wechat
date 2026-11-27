# Remote Builds on Boat

Use `scripts/boat/run.mjs` to offload Rust and TypeScript builds, lint, and tests to a [Boat](https://boat.dev) Ubuntu 24.04 sandbox without storing repository config, environment variables, or secrets in Boat.

Every sandbox is created with `--no-env --no-monitoring`:
- **No Boat dashboard setup** is required; any developer or agent with `boat login` can run it immediately.
- **No account secrets, GitHub tokens, or AI credentials** are injected into build sandboxes.
- **All toolchain pins live in this repo** ([rust-toolchain.toml](../../rust-toolchain.toml), [.node-version](../../.node-version), [package.json](../../package.json)).

## Quick start (for any fresh agent or checkout)

Prerequisite: `boat` CLI installed and signed in (`boat status`).

```bash
# Run a preset remotely (syncs your code, runs, stops sandbox at $0/hr)
node scripts/boat/run.mjs --preset rust-lint
node scripts/boat/run.mjs --preset rust-test
node scripts/boat/run.mjs --preset ts
node scripts/boat/run.mjs --preset verify

# Or run any arbitrary command in the remote repo root
node scripts/boat/run.mjs -- cargo +1.93.1 test --locked --manifest-path packages/native-client-rust/Cargo.toml -p wechat-core
```

Optional: warm a shared stopped base sandbox on `main` once so new branches/agents fork an already-compiled `target/` and `node_modules/` in seconds:

```bash
node scripts/boat/run.mjs seed
```

## How incremental builds work (committed & uncommitted files)

- **One sandbox per checkout & branch (`aw-build-<branch>-<hash>`):** Separate clones, worktrees, and branches each get a distinct sandbox name (including a checkout path hash so two clones on `main` or a branch named `base` never collide with each other or with the reserved `aw-build-base` seed). On each run, `run.mjs` checks `boat list --all --json` and picks the fastest path:
  1. Reuse `aw-build-<branch>-<hash>` if idle/running (refusing if `busy` or if an atomic per-sandbox lock under `/tmp/aw-boat-run.lock` is held by another concurrent run; pass `--name <other>` for parallel runs from the same checkout, or `--force-unlock` if a previous run was killed).
  2. Resume `aw-build-<branch>-<hash>` if stopped (restoring warm `target/`, `/usr/local/cargo`, and `node_modules/`).
  3. Fork `aw-build-base` if a warm seed exists.
  4. Create a fresh sandbox and run [setup.sh](./setup.sh).
- **Zero-token `git bundle` sync:**
  - **Committed files:** Syncs only the incremental commit delta from the sandbox's `HEAD` to local `HEAD` (works for unpushed local commits and private forks too). Pass `--committed` (or `--ref <sha|branch>`) to ignore local uncommitted edits.
  - **Uncommitted files (default):** Captures tracked modifications, staged changes, deletions, renames, and untracked non-ignored files in a temporary Git object directory (never modifying your local `.git/`). On the sandbox, `git read-tree -m -u` updates **only** files whose content changed so untouched files keep their timestamps and Cargo/Turborepo perform true incremental builds.
  - **Scratch-folder safety:** Untracked files under `work/` are skipped by default (override with `--include-work`), and untracked files over 10 MiB (or 50 MiB total) fail fast unless excluded (`--exclude <path>`) or allowed (`--allow-large-untracked`).
- **Auto-stop after each run:** Sandboxes stop automatically when the command finishes (`$0/hr` compute while stopped; disk kept for at least 14 days; frees concurrent sandbox slots). Running TTL defaults to 1 hour (`--ttl 3600`, refreshed on reuse); pass `--ttl 7200` for longer builds or `--keep` while iterating rapidly.

## Useful commands

```bash
# Inspect your branch sandbox and warm seed
node scripts/boat/run.mjs status

# Build strictly committed HEAD (ignoring dirty working tree)
node scripts/boat/run.mjs --committed --preset rust-test

# Build a release binary and copy it back locally
node scripts/boat/run.mjs --pull packages/native-client-rust/target/release/wx:./wx -- \
  cargo +1.93.1 build --release --locked --package wx-native --manifest-path packages/native-client-rust/Cargo.toml

# Use a larger machine size (paid Boat plans)
node scripts/boat/run.mjs --type large --preset rust-test

# Stop or permanently delete this branch's sandbox
node scripts/boat/run.mjs stop
node scripts/boat/run.mjs delete
```
