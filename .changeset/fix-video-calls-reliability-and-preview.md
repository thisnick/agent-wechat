---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
"@agent-wechat/desktop": patch
"@agent-wechat/client-ui": patch
"@agent-wechat/agent-wechat": patch
---

Fix video calling orientation, conference join/create reliability, local preview smoothness, incoming CLI/transcoded video recovery, and OpenClaw call-end media teardown:

- Pre-rotate outgoing call video 90° CCW across native capture (`LocalVideoCapture`), browser WebCodecs H.264, and daemon TX transcoding while signaling wire `orientation = 1` (+90° CW) in both `EncFecHeader` and `workMode = 3` virtual-frame metadata packets (`pkg_seq = 0`, `hdr[12] = 0x07`, `hdr[15] = 0x00`, `VideoPktExtHeader` fields `1..=9` and `16` with `f2 = 0`), with desktop `SelfViewDecoder` undoing the rotation for upright local self-view so outgoing video displays upright on Android, iOS/iPadOS, Official Linux, and native clients.
- Fix conference media mutex starvation by releasing the conference roster lock before media encoding/decoding and UDP reads, allow `connected()` to race ahead of `Admitted` without regressing state, accept initial `sequence = 0` and proto3-omitted zero values in `AVMemberList`, preserve conference `video_flags = 100`, add periodic UDP relay check-in retransmits and bounded `SubscribeVideo` retries, and hide the desktop self-view corner tile until the call is connected.
- Reduce desktop camera preview and in-call self-view lag with event-driven drop-oldest MJPEG capture (`480x360 @ 24 fps`, `-fflags nobuffer -flags low_delay`), low-latency single-threaded `SelfViewDecoder` without `fps=10` rate-conversion buffering, and single-in-flight `createImageBitmap` canvas coalescing.
- Prevent incoming transcoded video (`wx calls monitor` / JPEG clients) from stalling on `Waiting for video...` by caching codec parameter sets (`VPS`/`SPS`/`PPS`), requiring parameter sets before latching the keyframe gate, resetting exited or stalled FFmpeg transcoders, upgrading `wechat-calls` JPEG extraction to segment-aware marker scanning, tracking the peer's observed `stream_id` for `IRequest`, and sending bounded initial keyframe requests.
- Avoid spurious `needs_review` escalation in OpenClaw `CallsService` when `media_closed` races slightly ahead of a concurrent `call.state(ended)` control event.
