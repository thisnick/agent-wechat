---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Fix freezing and blurry incoming video in direct video calls.

- Large keyframes now assemble. Phones send a big keyframe in several bursts, each with its own timestamp, and the daemon treated every new timestamp as a new frame, so these keyframes never completed and video froze until a smaller one arrived.
- Lost video packets are now retransmitted. Our NACKs set byte 6 to a counter, which official senders treat as an unknown format and ignore, so it is now 0 as they expect. Retransmitted packets (`0x67`, sent as Live98 with extension byte 4) are accepted and added to their frame, and P2S reports count them in the ARQ field.
- When the other side NACKs our video, the daemon resends the requested packets once from the last 300 ms of sent video, as the official client does.
- Incomplete frames are held for 500 ms of sender time, instead of a fixed number of frames, so retransmissions have time to arrive. NACKs wait 20–100 ms depending on jitter, are sent once per gap, and stop after 300 ms.
- Live98 NACKs and retransmissions carry a separate counter in the sequence field, so they no longer go through the video replay check, where a repeated value could close the media channel.

`wechat_protocol::calls::video_packet::encode_video_nack` no longer takes an `arq_seq` argument, `VideoNack::arq_seq` is now `VideoNack::format`, `VideoChannelPayload` has a new `ArqResponse` variant, and the new `wechat_protocol::calls::video_arq` module provides the sender-side retransmission cache.
