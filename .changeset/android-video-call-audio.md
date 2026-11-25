---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Fix audio in both directions on video calls with official Android clients: video offers and answers no longer advertise bit 17 of the audio extended-capability field, which made Android peers stop sending and playing call audio. Acknowledge the in-band `0x50` reliable messages official clients send during a call, so they stop retransmitting them.
