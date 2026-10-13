---
"@agent-wechat/agent-wechat": patch
---

Load on OpenClaw 2026.10.2, which removed the `channel-message`, `channel-reply-pipeline`, `config-runtime` and `command-auth` SDK subpaths. Outbound and reply-pipeline helpers now come from `channel-outbound`, group-policy helpers from `runtime-group-policy`, and sender command authorization is resolved in the plugin with the same rules as before. Still works on 2026.9.x.
