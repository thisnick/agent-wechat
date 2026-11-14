---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/agent-wechat": patch
---

Received WeChat Channels cards now show the post's cover, publisher, description and length instead of WeChat's "please upgrade" notice, and the message body no longer contains that notice or its link. The post's video is listed as a `video` asset that downloads only on request: open it in the desktop app, run `wx messages media CHAT MESSAGE_ID --quality normal`, or have an OpenClaw agent call `wechat_media`. Each request fetches a fresh copy from WeChat, and the downloaded video is kept in the daemon's media files. OpenClaw agents get the post text and cover by default. Media tools in a sandboxed OpenClaw agent now say a downloaded file isn't reachable from the agent's shell instead of returning a path it can't open.
