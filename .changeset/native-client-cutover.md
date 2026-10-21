---
"@agent-wechat/cli": major
"@agent-wechat/agent-wechat": major
"@agent-wechat/shared": major
---

Replace the official-client container backend with the standalone native WeChat daemon and Rust `wx` CLI. This is a breaking release: remove `wx-legacy`, the UI server/image and Wechaty integrations; use native v1 HTTP/SSE and managed or external OpenClaw hosting. Existing container API configurations must be migrated explicitly.

Take over a stopped, supported official Linux profile in place with `auth login --official[=/path]`, preserving account/device/database identity and saved-login material. Continue supported encrypted database, local read-state and media writes with durable sync, idempotent text/media jobs, independently encoded voice and explicit uncertainty handling. Add offline inspection, metadata catalogs and generated API contracts. Fresh-profile creation and copy import/export remain experimental extras.

Distribute verified Linux x64 and ARM64 GNU executables (glibc 2.36+), an npm launcher and a native-only multi-architecture container. Add CI package/runtime tests, exact-source publication gates, codec notices and optional service/Compose examples. Deployment and publication remain separate operator actions. The shared TypeScript SDK remains private and is bundled into the OpenClaw package.

Use OpenClaw environment/SecretRef credentials instead of `native.tokenFile`; owned daemon startup passes the token through a private stdin pipe without writing it or inheriting Gateway secrets. Clarify daemon-host profile paths and the unverified status of macOS/Windows profile compatibility in the published migration guide.
