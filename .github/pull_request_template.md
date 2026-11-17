<!--
Thanks for contributing. A reviewer needs to see two things: the problem is
real, and this change fixes it. Answer each section with output from commands
you actually ran. "Tests pass" on its own isn't enough evidence.

- For a new feature or a change of design, agree on the approach in an issue
  before you open the PR.
- Test with a test account. Send only to your own File Transfer chat
  (filehelper) or to people who agreed to receive test messages.
- Never paste tokens, keys, wxids, phone numbers, names or message text.
- Delete sections that don't apply, and these comments.
-->

Fixes #

## Problem

<!--
What was wrong or missing, in a user's terms, and how you found it.

Show it happening on `main`: the command you ran and what it printed, or a
test that fails on `main`. The best reproduction doesn't need your account,
for example a generated file (an ffmpeg command), the image's size and format,
or the field values involved.
-->

## Cause

<!-- Bug fixes: the root cause, naming the file and function. Features: delete this section. -->

## Changes

<!--
What this PR changes, as bullets or a before/after table. Say what
deliberately stays the same.
-->

### Difference from the official client

<!--
Only if this behaves or stores data differently from the official WeChat
client, or you couldn't check what the official client does. Say how you know
(which client, platform and version). Delete otherwise.
-->

## Risk and rollback

<!--
What could break, for whom, and how someone would notice. Can installing the
previous version undo this change? Say so plainly if not: a new migration, a
changed stored format or a write to the official profile can make going back
impossible.
-->

## Evidence

### Tests

<!--
Each new or changed test and what it checks. Confirm each one fails with the
fix reverted.
-->

### Live run

<!--
What you ran against a real test account, with output from before and after
the change. Include:

- `wx --json version` or the commit, OS and architecture
- the profile: created by agent-wechat (`wx auth login`) or official (`--official`)
- where you sent anything
- media and voice: whether it played or opened on the receiving phone
- speed or memory: numbers before and after, and how you measured them

If you didn't do a live run, say so and why.
-->

## How to test

<!--
Steps a reviewer can follow on their own test account: what to run on `main`
to see the problem, what to run on this branch, and the output to expect.

To run a branch without touching your normal daemon or profiles, give it its
own data folder and port. Use a folder that doesn't exist yet: the daemon
creates it with private permissions, and refuses an existing folder that
other users can read.

  cargo build --manifest-path packages/native-client-rust/Cargo.toml -p wx-native
  WX="$PWD/packages/native-client-rust/target/debug/wx"
  TEST_DIR="$HOME/agent-wechat-test"
  "$WX" --data-dir "$TEST_DIR" --listen 127.0.0.1:6180 serve

  # in a second terminal
  export AGENT_WECHAT_URL=http://127.0.0.1:6180
  export AGENT_WECHAT_TOKEN_FILE="$HOME/agent-wechat-test/api-token"
  "$WX" accounts create --alias test
  "$WX" auth login
  "$WX" messages list filehelper --limit 1

WeChat allows one desktop session per account, so this signs the test account
out of any other computer.
-->

## Interfaces and release notes

- Interface changes: <!-- "None", or each change from `pnpm interfaces`, marking anything breaking -->
- Changeset: <!-- package and bump, or "not needed" for docs, CI or tests only -->

## Checks

<!--
Rust:
  cargo +1.93.1 fmt --all --check --manifest-path packages/native-client-rust/Cargo.toml
  cargo +1.93.1 clippy --workspace --all-targets --locked --manifest-path packages/native-client-rust/Cargo.toml -- -D warnings
  cargo +1.93.1 test --workspace --locked --manifest-path packages/native-client-rust/Cargo.toml
TypeScript: pnpm typecheck, then the affected package's tests
-->

- [ ] Rust: fmt, clippy and tests pass (list any failure that also happens on `main`)
- [ ] TypeScript: typecheck and tests pass
- [ ] `pnpm interfaces` run and its changes committed
- [ ] `bash scripts/generate-types.sh --check` (exported Rust types changed)
- [ ] `pnpm i18n check` (UI text or docs changed)
- [ ] Scene added or updated in `packages/client-ui/scenes/` (a screen changed)

## Not verified

<!-- What you didn't test, known gaps, and follow-ups. -->
