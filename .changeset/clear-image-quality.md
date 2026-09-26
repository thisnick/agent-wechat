---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/cli": patch
"@agent-wechat/shared": patch
"@agent-wechat/agent-server": patch
---

Accept valid phone images with recoverable metadata warnings and use best-available image retrieval by default in the HTTP API, CLI, and OpenClaw. The reported quality distinguishes original, standard, and thumbnail copies; callers can request strict full resolution with `quality=full` or `wx messages media --full`.
