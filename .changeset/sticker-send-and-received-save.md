---
"@agent-wechat/agent-wechat": patch
---

Fix sending and saving stickers from OpenClaw. Sending a Saved sticker with the message tool's `sticker` action failed on every chat turn with "requires live host handoff authority", because the plugin required a dispatch hook that OpenClaw only passes on scheduled turns. It now calls that hook when it is present, as ordinary sends do. `wechat_stickers` `add` now accepts just the `messageId` of a received sticker and finds that message's original sticker; received messages never showed the model an `assetId`.
