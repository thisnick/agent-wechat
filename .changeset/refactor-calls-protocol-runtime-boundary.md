---
"@agent-wechat/rust-crates": major
---

Clean up the calling packet/runtime boundary between `wechat-protocol` and `wechat-calls`.

- Expose canonical RSM kind-3 header parsing (`RsmPacketHeader`, `parse_rsm_packet_header`, `RsmVideoReassembler::ingest_with_header`), `advance_seq16`, and Annex-B NALU helpers (`is_annexb_keyframe`, `annexb_nalu_types`) in `wechat-protocol::calls::video_packet` so callers never slice raw `EncFecHeader` or NALU bytes.
- Add deterministic, clock-injected (`now_ms: u64`) `VideoNackTracker`, `VideoP2sAccumulator`, and `VideoKeyframePolicy` state machines under `wechat-protocol::calls::video_qos` for direct reuse without `wechat-calls` runtime or codec dependencies.
- Unify direct and conference video receive recovery and outbound video burst pacing inside `wechat-calls` so both paths share the same state machines.
- Mark the low-level `wechat_calls::conference_media` and `wechat_calls::conference_send` host-integration modules `#[doc(hidden)]` alongside `admission`, `session`, and `native_media` (breaking change to the `wechat-calls` public API listing).
