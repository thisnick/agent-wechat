# @agent-wechat/shared

[English](README.md) | [简体中文](README.zh-CN.md)

`agent-wechat` 的私有 TypeScript SDK 与生成的 Rust 数据契约。

本包提供：
- **原生 HTTP/SSE 客户端**：用于通过 REST API 和 Server-Sent Events (SSE) 与原生微信守护进程通信的 TypeScript SDK。
- **生成的契约**：从 `wechat-api` 中的 Rust 结构体自动生成的数据类型和接口定义（`native.generated.ts`）。

## 开发

运行测试：

```bash
pnpm --filter @agent-wechat/shared test
```

在 Rust 契约更新后重新生成类型定义：

```bash
pnpm generate-types
```
