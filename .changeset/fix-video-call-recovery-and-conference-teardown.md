---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Improve incoming video call keyframe recovery under packet loss, add direct ringing relay pre-connect, and clean up conference remote-end teardown.

- Track missing 16-bit `pkg_seq` ranges on direct and conference incoming video streams and send bounded `0x06` ARQ/NACK retransmission requests matched to the active `Live96`/`Live98` relay framing, report live `0x0b` MP P2S video/link telemetry (separating received `recv_fps` from playable `play_fps` and reporting heavy freeze while awaiting initial or post-drop keyframe sync so QoS servers trigger `u8IRequestFlag` on hardware H.264 peers), and send immediate forced `0x66` `IRequest` recovery before the first self-contained keyframe arrives.
- Reassemble 1D multi-group RSM video frames whose trailing FEC group carries fewer source packets than group 0, and recover 2D multi-group frames across full-group loss via vertical Reed-Solomon parity.
- Advertise the video transport capability bit (`1 << 17`) in `Cap11` field 7 on video offers while leaving audio-only offers unchanged.
- Bound mid-stream post-drop P-frame holds to 1200 ms on initialized video streams so playback resumes with periodic background keyframe requests if a peer drops recovery IDRs, while still requiring a fresh IDR after a transcoder reset.
- Pre-connect the incoming direct UDP relay during the ringing phase when `ConfAck` returns relay candidates, buffering early post-Ready packets (such as initial `seq = 0` keyframes) and handing off a fresh matching socket to the media session on `Join`.
- Require a startup grace window before resetting an FFmpeg receive transcoder for zero output, drain transcoder output during synchronous early-conference replay, and clear stale conference cache entries when a track ends.
- Reconcile early-announced conference tracks, exit the conference receive control loop promptly when all remote members leave, and extend the CLI media-closed-before-control-end grace window so remote conference teardown reports clean completion instead of `termination outcome unknown`.
