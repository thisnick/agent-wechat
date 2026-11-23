---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Improve direct video call startup and keyframe recovery, including camera-off answers.

- Cache the latest contiguous inbound video GOP (or latest JPEG frame) per active receive track in `CallService` and replay it immediately when a client attaches its media socket (`attach_media`), eliminating the early-keyframe drop race between call connection and media WebSocket setup; if pre-attach frames arrived without a contiguous keyframe, trigger `request_video_keyframe` immediately on attach.
- Apply receive P-frame loss gating and cached VPS/SPS/PPS parameter-set prepending to passthrough (`H.264`/`H.265`) tracks as well as transcoded tracks, so passthrough WebCodecs/H.264 decoders do not stall on unreferenced P-frames or bare recovery IDRs.
- Join and invite direct video calls with the video status bit set (`av_status = 3`) even when `cameraEnabled` is `false` (while keeping local camera send inactive) so the relay forwards initial keyframe requests, then send `SwitchAV` to clear the camera bit once the first remote frame arrives or the startup grace window elapses.
- Send unforced (`force = false`) direct-call `0x66` `IRequest` packets by default so remote hardware encoders run their keyframe-request callback, escalating to `force = true` after unanswered retries and pacing retries at 1000 ms.
