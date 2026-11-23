---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": minor
---

Fix incoming direct video calls failing to receive and decode remote video.

- Send `mode.av_status()` (`3` for video, `1` for voice) in `AckReq` field 13, set `group` field 7 (`member_flag`) to `1` after `AckRsp` on incoming joins, and send `network_type = 4` in `RelayCheckIn` field 13 so peers stream video on incoming direct video calls.
- Buffer non-check-in datagrams arriving during the UDP relay handshake (`connect_relay_with_early`) and match the 200 ms check-in retransmit interval so incoming video frame #0 IDR bursts are preserved instead of discarded.
- Stop dropping real RSM video packets whose `EncFecHeader` byte 13 bit 4 (`0x10`) bandwidth-probe flag is set (mobile H.264 IDR keyframe packets carry this bit alongside valid SPS/PPS/IDR bytes), while keeping `f_tot_pkts == 0 || f_pkt_idx == 0 || s_per_group == 0` filtering for degenerate/padding-only headers.
- Advertise `HW265` (`0x04` and fields 21/22) plus standard video feature bits (`0x3f9d`, excluding proprietary `Vcodec1`/`MixVcodec1`), accept `extension_byte <= 2` on `0x20`/`0x40` video packets, send the initial `0x70` control probe after relay Ready, align `encode_p2s` with the nested `inner.f1` + `inner.f3.f1` wire layout, and encode 64-bit `lost_point_seq` and `force` in `encode_irequest_with_force`.
