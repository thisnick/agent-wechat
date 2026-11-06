# wechat-media

[English](README.md) | [简体中文](README.zh-CN.md)

由 `wechat-protocol` 与 `wechat-profile` 共享的图像检查与解码支撑库。它用于识别受支持的图像格式、检查其图像尺寸并解析受支持的微信图像容器。

该支撑 Crate 与上述核心库保持相同的版本发布节奏。它不负责下载附件、调度传输队列或管理消息投递。部分解码路径需要系统 PATH 中具备 FFmpeg。

依赖配置、兼容性说明及调用示例请参见 [Rust 开发库参考](https://thisnick.github.io/agent-wechat/zh-cn/reference/rust-libraries/)。
