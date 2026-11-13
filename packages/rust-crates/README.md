# Published Rust library versions

`@agent-wechat/rust-crates` is a private Changesets package. It supplies the
version and changelog for `wechat-platform`, `wechat-media`, `wechat-protocol`,
`wechat-profile` and `wechat-calls`; it is not published to npm.

Add this package to a changeset when a published Rust library changes:

```md
---
"@agent-wechat/rust-crates": patch
---

Describe the library change and any migration needed by Rust callers.
```

Use `minor` for compatible additions and `major` for breaking library APIs.
Include the CLI or OpenClaw package in the same changeset when its behavior also
changes. This package is in the same fixed Changesets group as the CLI, OpenClaw
extension and desktop app: every release gives the crates and the applications
one shared version, taking the largest bump any of them asks for. All five
public crates keep that version and exact internal dependency requirements.

`pnpm version-packages` updates the manifests and Cargo lockfiles. Every release
publishes the five crates, with an empty changelog entry when no library
changed.

Library releases use `crates-vVERSION` Git tags (the same `VERSION` as the
application's `vVERSION` tag) and their own GitHub release
notes. Publication retains the existing `native-release.yml` caller,
`native-release` environment and crates.io trusted publishers. See
[RELEASING.md](../../RELEASING.md) for checks and recovery.
