---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/cli": patch
"@agent-wechat/shared": patch
"@agent-wechat/agent-server": patch
---

Accept valid phone images with recoverable metadata warnings and add a best-available image retrieval mode. OpenClaw uses this mode for inbound images and labels thumbnail fallbacks; the CLI exposes it as `wx messages media --best` while retaining strict full-resolution behavior by default.
