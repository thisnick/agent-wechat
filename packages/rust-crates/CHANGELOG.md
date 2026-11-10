# @agent-wechat/rust-crates

## 2.1.0

### Minor Changes

- [#441](https://github.com/thisnick/agent-wechat/pull/441) [`1b4c215`](https://github.com/thisnick/agent-wechat/commit/1b4c2152d4fcfa302c6bebc48a473eab97064f0f) Thanks [@Lazily01](https://github.com/Lazily01)! - Add a native OpenIM avatar lookup path and optional group context to avatar refresh. Keep server-returned OpenIM failure codes available in operation results. A controlled macOS ARM64 account accepted two group-member avatar downloads; broader account and platform acceptance remains separate.

## 2.0.0

### Patch Changes

- [#414](https://github.com/thisnick/agent-wechat/pull/414) [`147ccb2`](https://github.com/thisnick/agent-wechat/commit/147ccb2d9be71d556e2379d026deaa6120b1ef6e) Thanks [@thisnick](https://github.com/thisnick)! - Version the five published Rust libraries together through a private Changesets
  package, independently of application and npm releases. Library releases retain
  exact dependency versions and source verification, and use separate `crates-v`
  tags and library changelogs.
