# wechat-platform

[English](README.md) | [简体中文](README.zh-CN.md)

供 `wechat-protocol`、`wechat-profile` 与 `wechat-media` 使用的平台支撑库：提供私有文件与临时目录管理、进程身份与本地网络支持。绝大多数应用应直接依赖协议库或 Profile 库。

该支撑 Crate 与上述核心库保持相同的版本发布节奏。它不实现账号认证、Profile 存储或后台守护进程策略。

依赖配置、兼容性说明及调用示例请参见 [Rust 开发库参考](https://thisnick.github.io/agent-wechat/zh-cn/reference/rust-libraries/)。
