---
"@agent-wechat/cli": major
"@agent-wechat/agent-wechat": major
"@agent-wechat/shared": major
"@agent-wechat/rust-crates": minor
---

Add native direct and conference video calling and move call WebSockets to protocol version 3.

- Upgrade the call control and media WebSocket protocol from version 2 to version 3. Media WebSockets now carry binary `WXCM` v3 frames for both PCM audio and video (`h265`, `h264`, `jpeg`) instead of base64 JSON audio frames, and support single-use `?ticket=` authentication from `POST /v1/accounts/{account}/calls/ws-ticket` alongside `Authorization: Bearer`. Older v2 clients are rejected with `unsupported_protocol_version`; upgrade the daemon, `wx` CLI, `@agent-wechat/shared` SDK, and OpenClaw plugin together.
- Add direct (`voip-2p`) and conference (`voip-mt`) video calling to the native Rust daemon (`wechat-protocol`, `wechat-calls`, `wechat-core`, `wechat-api`), including standard H.265/H.264 pass-through and FFmpeg transcoding fallback, mid-call camera toggling (`SwitchAV 3197`), automatic conference video subscription (`Subscribe 3359`), RSM video packetization/reassembly with GF(2^8) Reed-Solomon FEC recovery, and orientation metadata propagation.
- Extend `wx calls` (`dial`, `answer`, `monitor`, `conference start/join`, `devices`, `status`) with `--video`, `--camera`, and `--camera-off`, live mid-call camera toggling (`v`), inline terminal video preview (`chafa` / Sixel / Kitty / half-block), FFmpeg camera capture (`auto`, `off`, `test`, `v4l2:...`, `avfoundation:...`, `dshow:...`), and smoother microphone/speaker playout jitter handling.
- Update `@agent-wechat/shared` with Calls Protocol v3 (`WXCM` binary audio/video codecs, `createCallsWsTicket`, video capabilities, `camera()`, `requestVideoKeyframe()`), and upgrade the OpenClaw plugin's voice calling client to protocol v3 (audio-only).
