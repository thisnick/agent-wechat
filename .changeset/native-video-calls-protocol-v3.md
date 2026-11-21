---
"@agent-wechat/cli": major
"@agent-wechat/agent-wechat": major
"@agent-wechat/shared": major
"@agent-wechat/rust-crates": major
---

Add native direct and conference video calling and move call WebSockets to protocol version 3.

- Upgrade the call control and media WebSocket protocol from version 2 to version 3. Media WebSockets now carry binary `WXCM` v3 frames (36-byte big-endian header) for both PCM audio and video (`h265`, `h264`, `jpeg`) instead of base64 JSON audio frames, and support single-use `?ticket=` authentication from `POST /v1/accounts/{account}/calls/tickets` alongside `Authorization: Bearer`. Older v2 clients are rejected with `unsupported_protocol_version`; upgrade the daemon, `wx` CLI, `@agent-wechat/shared` SDK, and OpenClaw plugin together.
- Add direct (`voip-2p`) and conference (`voip-mt`) video calling to the native Rust daemon (`wechat-protocol`, `wechat-calls`, `wechat-core`, `wechat-api`), including standard H.265/H.264 pass-through and FFmpeg transcoding fallback, mid-call camera toggling, automatic conference video subscription, RSM video packetization/reassembly with GF(2^8) Reed-Solomon FEC recovery, and orientation metadata propagation.
- Breaking Rust crate change in `wechat-protocol`: add `CallOperation::Subscribe` to the exhaustive `CallOperation` enum.
- Extend `wx calls` (`dial`, `answer`, `monitor`, `conference start/join`, `devices`, `status`) with `--video`, `--camera`, `--camera-off`, and `--muted`, live mid-call camera toggling (`v`), inline 24-bit TrueColor half-block terminal video preview, FFmpeg camera capture (`auto`, `off`, `test`, `file:<path>`, or a device path/name), and smoother microphone/speaker playout jitter handling.
- Update `@agent-wechat/shared` with Calls Protocol v3 (`WXCM` binary audio/video codecs, `NativeWeChatClient.issueCallTicket()`, `NativeCallsClient.setCamera()`, `NativeCallMediaClient.requestKeyframe()`, and video capabilities), and upgrade the OpenClaw plugin's voice calling client to protocol v3 (audio-only).
