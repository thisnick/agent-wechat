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
changes. A crate change does not automatically bump the applications, or the
reverse. All five public crates keep the same version and exact internal
dependency requirements.

`pnpm version-packages` updates the manifests and Cargo lockfiles. Unpublished
Rust crates keep the application workspace version. The generated Version
Packages PR can contain application releases, library releases, or both.

Library releases use `crates-vVERSION` Git tags and their own GitHub release
notes. Publication retains the existing `native-release.yml` caller,
`native-release` environment and crates.io trusted publishers. See
[RELEASING.md](../../RELEASING.md) for checks and recovery.
