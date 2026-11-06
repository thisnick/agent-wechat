# wechat-platform

[English](README.md) | [简体中文](README.zh-CN.md)

Platform support used by `wechat-protocol`, `wechat-profile` and `wechat-media`:
private files and temporary directories, process identity and local networking.
Most applications should depend on the protocol or profile library instead.

This supporting crate follows the same release version as those libraries.
It does not implement account authentication, profile storage or daemon policy.

See the [Rust libraries reference](https://thisnick.github.io/agent-wechat/reference/rust-libraries/)
for dependencies, compatibility and examples.
