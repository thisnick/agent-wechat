---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
"@agent-wechat/desktop": patch
"@agent-wechat/client-ui": patch
---

Fix video calling orientation metadata, conference join/create reliability, local preview smoothness, and incoming CLI/transcoded video recovery:

- Align outgoing kind-3 virtual-frame orientation metadata packets with `workMode = 3` framing (`pkg_seq = 0`, `hdr[12] = 0x07`, `hdr[15] = 0x00`, `VideoPktExtHeader` fields `1..=9` and `16` with `f2 = 0`) and emit them after packetizing the first video frame (`frame_idx >= 1`).
- Fix conference media mutex starvation by releasing the conference roster lock before media encoding/decoding and UDP reads, allow `connected()` to race ahead of `Admitted` without regressing state, accept initial `sequence = 0` and proto3-omitted zero values in `AVMemberList`, preserve conference `video_flags = 100`, add periodic UDP relay check-in retransmits and bounded `SubscribeVideo` retries, and hide the desktop self-view corner tile until the call is connected.
- Reduce desktop camera preview and in-call self-view lag with event-driven drop-oldest MJPEG capture (`480x360 @ 24 fps`, `-fflags nobuffer -flags low_delay`), low-latency single-threaded `SelfViewDecoder` without `fps=10` rate-conversion buffering, and single-in-flight `createImageBitmap` canvas coalescing.
- Prevent incoming transcoded video (`wx calls monitor` / JPEG clients) from stalling on `Waiting for video...` by caching codec parameter sets (`VPS`/`SPS`/`PPS`), requiring parameter sets before latching the keyframe gate, resetting exited or stalled FFmpeg transcoders, upgrading `wechat-calls` JPEG extraction to segment-aware marker scanning, tracking the peer's observed `stream_id` for `IRequest`, and sending bounded initial keyframe requests.
