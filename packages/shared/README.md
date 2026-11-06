# @agent-wechat/shared

[English](README.md) | [简体中文](README.zh-CN.md)

Private TypeScript SDK and generated Rust data contracts for `agent-wechat`.

This package provides:
- **Native HTTP/SSE Client**: TypeScript SDK for communicating with the native WeChat daemon via REST API and Server-Sent Events (SSE).
- **Generated Contracts**: Data types and interfaces automatically generated from Rust structs in `wechat-api` (`native.generated.ts`).

## Development

Run tests:

```bash
pnpm --filter @agent-wechat/shared test
```

Regenerate types after Rust contract updates:

```bash
pnpm generate-types
```
