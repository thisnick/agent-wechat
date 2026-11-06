# wechat-media

[English](README.md) | [简体中文](README.zh-CN.md)

Image inspection and decoding support shared by `wechat-protocol` and
`wechat-profile`. It identifies supported image formats, checks their dimensions
and handles supported WeChat image containers.

This supporting crate follows the same release version as those libraries.
It does not download attachments, enqueue transfers or manage message delivery.
Some decoding paths need FFmpeg on `PATH`.

See the [Rust libraries reference](https://thisnick.github.io/agent-wechat/reference/rust-libraries/)
for dependencies, compatibility and examples.
