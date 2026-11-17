---
"@agent-wechat/agent-wechat": patch
---

The OpenClaw plugin no longer holds a received file or video in the gateway's memory. Files of any size and videos now stream from the daemon into OpenClaw's media store, checked against their size and SHA-256 as they are written; before, files up to 128 MiB and every video were read whole first. Images, voice notes and stickers are unchanged. Videos are still offered to the agent as media, and the 128 MiB video limit is unchanged.
