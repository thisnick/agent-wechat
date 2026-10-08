---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/cli": patch
"@agent-wechat/wechaty-puppet": patch
---

Retrieve supported WeChat type-47 stickers through the existing media endpoint, preserving GIF animation. Download and validate original CDN bytes into a bounded, account-scoped server cache without opening chats or writing to WeChat's cache. Existing CLI media commands can save the returned sticker files; automatic OpenClaw sticker ingestion is unchanged.
