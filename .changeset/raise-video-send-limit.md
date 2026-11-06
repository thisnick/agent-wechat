---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
"@agent-wechat/desktop": minor
---

Video sends now accept sources up to 512 MiB, matching ordinary files, instead of 50 MiB. The CLI, HTTP API capabilities, desktop and web composer, and OpenClaw extension all use the new limit; OpenClaw still applies `channels.agent-wechat.mediaMaxMb`, which defaults to 50. Original-quality videos of 25 MiB and above upload in blocks through the same path as large files.
