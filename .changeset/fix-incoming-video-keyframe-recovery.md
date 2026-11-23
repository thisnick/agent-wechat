---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Preserve early receive video keyframes before media attach and harden passthrough recovery.

- Cache the latest contiguous inbound video GOP (or latest JPEG frame) per active receive track in `CallService` and replay it immediately when a client attaches its media socket (`attach_media`), eliminating the early-keyframe drop race between call connection and media WebSocket setup; if pre-attach frames arrived without a contiguous keyframe, trigger `request_video_keyframe` immediately on attach.
- Apply receive P-frame loss gating and cached VPS/SPS/PPS parameter-set prepending to passthrough (`H.264`/`H.265`) tracks as well as transcoded tracks, so passthrough WebCodecs/H.264 decoders do not stall on unreferenced P-frames or bare recovery IDRs.
- Pace local `IRequest` retransmissions in direct video calls at 1000 ms and add targeted `WX_CALL_VIDEO_TRACE=1` diagnostics for recovery keyframes, outbound `IRequest`s, and bare-IDR drops.
