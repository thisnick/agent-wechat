---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Fix incoming MP/iLink RSM video keyframe parity decoding and out-of-order FEC reassembly.

- Sum the three wire RS parity fields (`r_lo2 + r_mid2 + r_hi4`, max 21) additively instead of bit-shifting them (`r_lo2 | (r_mid2 << 2) | (r_hi4 << 4)`), matching `s_per_group` and `pkg_idx_in_group`. Bit-shifting inflated high-parity keyframes (`r >= 10`) past `MAX_FEC_PARITY (64)` so every keyframe packet was silently dropped even without packet loss, and corrupted `r = 4..9` RS erasure recovery.
- Hold completed newer frames in a bounded in-order ready queue while an older incomplete keyframe (up to 4 frames) or FEC-protected P-frame (up to 2 frames) is still receiving paced packets or parity, instead of immediately evicting the older frame when a smaller subsequent P-frame completes first, and expose `RsmVideoReassembler::pop_ready` / `ConferenceReceiver::pop_ready_video` so callers drain all newly unblocked frames without pipeline lag. Completed recovery keyframes continue to evict strictly older incomplete assemblies immediately.
- Expand bounded keyframe, FEC recovery, drop, and ingest-error logging under `WX_CALL_VIDEO_TRACE=1`.
