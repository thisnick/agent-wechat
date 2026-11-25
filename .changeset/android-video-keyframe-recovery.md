---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Fix missing incoming video on calls with official Android clients when the opening keyframe arrives before the relay connection is ready. Video P2S reports are now framed and filled as official clients send them, which lets WeChat's server adapt the sender's stream; when the server switches a sender to WeChat's Vcodec1 packaging, the daemon rewraps it as standard H.264. A stream still waiting for a keyframe asks for a smaller picture size, which makes the sender restart its encoder. Group calls also acknowledge in-band `0x50` messages.
