//! MP/iLink (`workMode = 3`) video packetization, Annex-B framing and video RTCP/QoS codecs.
//!
//! These codecs perform no network I/O, key management or encoder/decoder lifecycle
//! management. Callers must authenticate packets via [`crate::calls::audio_packet::Live96`]
//! before invoking parsers or dispatching decoded frames.

use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, ensure, Result};
use std::collections::BTreeMap;

/// Maximum unframed Annex-B access unit accepted by the packetizer or reassembler.
pub const MAX_VIDEO_FRAME_BYTES: usize = 512 * 1024;
/// Maximum packet sequence ranges encoded in a single `0x06` video NACK payload.
pub const MAX_VIDEO_NACK_RANGES: usize = 16;
/// Default source payload chunk size per UDP video packet (leaves room for Live96/Live98 + RSM headers inside MTU).
pub const DEFAULT_RSM_CHUNK_BYTES: usize = 1100;
/// Default wire orientation code (`2` = upright 0° in `EncFecHeader` byte 13 bits 7..5).
pub const DEFAULT_RSM_ORIENTATION: u8 = 2;
const MAX_ACTIVE_ASSEMBLIES: usize = 8;
/// Reordering/pacing grace window (in frames) before evicting an older incomplete
/// FEC-protected P-frame when newer frames complete ahead of it.
const FEC_P_GRACE_FRAMES: u32 = 2;
/// Reordering/pacing grace window (in frames) before evicting an older incomplete
/// keyframe whose paced source/parity burst overlaps subsequent small P-frames.
const IFRAME_GRACE_FRAMES: u32 = 4;
const MAX_FRAGMENTS_PER_FRAME: usize = 512;
const MAX_FEC_PARITY: usize = 64;

/// Clockwise display rotation derived from the `EncFecHeader` kind-3 orientation code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VideoRotation {
    /// Upright (0 degrees).
    #[default]
    Deg0,
    /// Rotate 90 degrees clockwise.
    Deg90,
    /// Rotate 180 degrees.
    Deg180,
    /// Rotate 270 degrees clockwise (90 degrees counter-clockwise).
    Deg270,
}

impl VideoRotation {
    /// Map the 3-bit wire orientation code (`EncFecHeader` byte 13 bits 7..5) to clockwise display rotation.
    ///
    /// Matches the official receiver mapping (`1` = 90° CW, `2` = 0°, `3` = 270° CW, `4` = 180°),
    /// including the mode-dependent handling of `0` and extended desktop codes `5..=7`.
    pub const fn from_wire(orientation: u8, is_conference: bool) -> Self {
        match orientation & 0x07 {
            1 => Self::Deg90,
            2 => Self::Deg0,
            3 => Self::Deg270,
            4 => Self::Deg180,
            0 => {
                if is_conference {
                    Self::Deg0
                } else {
                    Self::Deg90
                }
            }
            _ => {
                if is_conference {
                    Self::Deg90
                } else {
                    Self::Deg0
                }
            }
        }
    }

    /// Return clockwise degrees (`0`, `90`, `180` or `270`).
    pub const fn degrees(self) -> u16 {
        match self {
            Self::Deg0 => 0,
            Self::Deg90 => 90,
            Self::Deg180 => 180,
            Self::Deg270 => 270,
        }
    }
}

/// Wire codec identifier in byte 0 of an MP/iLink video payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoWireCodec {
    /// Standard H.264 / AVC (`codecType = 9`).
    H264,
    /// Standard H.265 / HEVC (`codecType = 15`).
    H265,
    /// Proprietary Vcodec1 (`codecType = 0`).
    Vcodec1,
    /// Proprietary MixVcodec1 (`codecType = 17`).
    MixVcodec1,
    /// Any other codec identifier byte.
    Unknown(u8),
}

impl VideoWireCodec {
    /// Decode a wire `codecType` byte.
    pub const fn from_wire(value: u8) -> Self {
        match value {
            9 => Self::H264,
            15 => Self::H265,
            0 => Self::Vcodec1,
            17 => Self::MixVcodec1,
            other => Self::Unknown(other),
        }
    }

    /// Encode to the wire `codecType` byte.
    pub const fn wire(self) -> u8 {
        match self {
            Self::H264 => 9,
            Self::H265 => 15,
            Self::Vcodec1 => 0,
            Self::MixVcodec1 => 17,
            Self::Unknown(other) => other,
        }
    }
}

/// Decoded `0x66` video keyframe request (`IRequest`) carried on `0x60..0x6f` (TX) / `0x20..0x2f` (RX).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoIRequest {
    /// Requesting member ID.
    pub local_member: u16,
    /// Stream ID requested by the sender.
    pub local_stream_id: u8,
    /// Target member ID whose encoder should emit a keyframe.
    pub target_member: u16,
    /// Target stream ID whose encoder should emit a keyframe.
    pub target_stream_id: u8,
    /// Monotonic millisecond timestamp carried in the body.
    pub timestamp_ms: u64,
}

/// Dispatched payload category on inbound `0x20..0x2f` video channels.
///
/// The relay rewrites outbound `0x60`/`0x61` (`0x79`) video RTCP packets to
/// `0x20`/`0x21` before delivery, so receivers inspect leading payload bytes
/// before RSM video parsing.
#[derive(Debug, PartialEq, Eq)]
pub enum VideoChannelPayload<'a> {
    /// Peer-to-peer `0x66` keyframe request (58 bytes).
    IRequest(VideoIRequest),
    /// Peer-to-peer `0x06` video NACK packet.
    Nack(&'a [u8]),
    /// MP/iLink RSM video packet (`[codecType, LE16(mid | (sid << 11)), EncFecHeader, ...]`).
    RsmVideo(&'a [u8]),
}

/// Classify a decrypted payload arriving on `packed_kind` `0x20..=0x2f`.
pub fn classify_video_channel_payload(payload: &[u8]) -> Result<VideoChannelPayload<'_>> {
    ensure!(!payload.is_empty(), "Empty video channel payload");
    match payload[0] {
        0x66 => Ok(VideoChannelPayload::IRequest(parse_irequest(payload)?)),
        0x06 => {
            ensure!(payload.len() >= 6, "Truncated video NACK payload");
            Ok(VideoChannelPayload::Nack(payload))
        }
        _ => Ok(VideoChannelPayload::RsmVideo(payload)),
    }
}

/// Encode a 58-byte `0x66` `IRequest` keyframe request packet.
pub fn encode_irequest(
    local_member: u16,
    target_member: u16,
    stream_id: u8,
    timestamp_ms: u64,
    sequence: u32,
) -> Vec<u8> {
    encode_irequest_with_force(
        local_member,
        target_member,
        stream_id,
        timestamp_ms,
        u64::from(sequence),
        false,
    )
}

/// Encode a 58-byte `0x66` `IRequest` camera keyframe request packet with explicit
/// `ullLostPointSeq` (`body[0x24..0x2c]`) and `force` flag (`body[0x2c] & 1`).
///
/// Bit 1 of `body[0x2c..0x30]` selects screen-share (`1`) versus camera (`0`)
/// video and remains `0` for camera streams.
pub fn encode_irequest_with_force(
    local_member: u16,
    target_member: u16,
    stream_id: u8,
    timestamp_ms: u64,
    lost_point_seq: u64,
    force: bool,
) -> Vec<u8> {
    let mut out = vec![0u8; 58];
    let local_packed = pack_rtcp_member(local_member, stream_id);
    let target_packed = pack_rtcp_member(target_member, stream_id);
    let flags = u32::from(force);
    out[0] = 0x66;
    out[1] = local_packed[0];
    out[2] = local_packed[1];
    out[3] = 0x00;
    out[4] = target_packed[0];
    out[5] = target_packed[1];
    // 52-byte body matching official Linux/Android SendIReqFeedBack layout.
    out[6] = 0x02;
    out[6 + 0x0c] = 0x03;
    out[6 + 0x1c..6 + 0x24].copy_from_slice(&timestamp_ms.to_le_bytes());
    out[6 + 0x24..6 + 0x2c].copy_from_slice(&lost_point_seq.to_le_bytes());
    out[6 + 0x2c..6 + 0x30].copy_from_slice(&flags.to_le_bytes());
    out
}

/// Parse a 58-byte `0x66` `IRequest` keyframe request packet.
pub fn parse_irequest(payload: &[u8]) -> Result<VideoIRequest> {
    ensure!(
        payload.len() >= 58 && payload[0] == 0x66,
        "Invalid video IRequest packet"
    );
    let (local_member, local_stream_id) = unpack_rtcp_member(payload[1], payload[2]);
    let (target_member, target_stream_id) = unpack_rtcp_member(payload[4], payload[5]);
    let timestamp_ms = u64::from_le_bytes(payload[6 + 0x1c..6 + 0x24].try_into()?);
    Ok(VideoIRequest {
        local_member,
        local_stream_id,
        target_member,
        target_stream_id,
        timestamp_ms,
    })
}

/// Decoded `0x06` video ARQ/NACK payload carried on `0x60..0x6f` (TX) / `0x20..0x2f` (RX).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoNack {
    /// Target member ID whose sender should retransmit the missing packets.
    pub target_member: u16,
    /// Target video stream ID.
    pub target_stream_id: u8,
    /// Requesting member ID.
    pub local_member: u16,
    /// Requesting video stream ID.
    pub local_stream_id: u8,
    /// Nonzero for screen-share streams; `0` for camera video.
    pub is_screen: u8,
    /// Monotonic wrapping NACK sequence number.
    pub arq_seq: u8,
    /// Half-open `[start_seq, end_seq)` missing 16-bit packet sequence ranges (`pkg_seq`).
    pub ranges: Vec<(u16, u16)>,
}

/// Encode a `0x06` video ARQ/NACK payload for `packed_kind = 0x60 | stream_id`.
///
/// Empty ranges (`start == end`) are omitted and the list is bounded to
/// [`MAX_VIDEO_NACK_RANGES`].
pub fn encode_video_nack(
    local_member: u16,
    target_member: u16,
    stream_id: u8,
    arq_seq: u8,
    ranges: &[(u16, u16)],
) -> Vec<u8> {
    let valid: Vec<(u16, u16)> = ranges
        .iter()
        .copied()
        .filter(|(start, end)| start != end)
        .take(MAX_VIDEO_NACK_RANGES)
        .collect();
    let target_packed = pack_rtcp_member(target_member, stream_id);
    let local_packed = pack_rtcp_member(local_member, stream_id);
    let mut out = Vec::with_capacity(8 + valid.len() * 4 + 16);
    out.push(0x06);
    out.extend_from_slice(&target_packed);
    out.extend_from_slice(&local_packed);
    out.push(0x00); // camera video (non-screen)
    out.push(arq_seq);
    out.push(valid.len() as u8);
    for (start, end) in valid {
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&end.to_be_bytes());
    }
    out.extend_from_slice(&[0u8; 16]);
    out
}

/// Parse a `0x06` video ARQ/NACK payload.
pub fn parse_video_nack(payload: &[u8]) -> Result<VideoNack> {
    ensure!(
        payload.len() >= 8 && payload[0] == 0x06,
        "Invalid video NACK packet"
    );
    let (target_member, target_stream_id) = unpack_rtcp_member(payload[1], payload[2]);
    let (local_member, local_stream_id) = unpack_rtcp_member(payload[3], payload[4]);
    let is_screen = payload[5];
    let arq_seq = payload[6];
    let count = usize::from(payload[7]);
    ensure!(
        count <= 49 && payload.len() >= 8 + count * 4,
        "Truncated video NACK ranges"
    );
    let mut ranges = Vec::with_capacity(count);
    for i in 0..count {
        let off = 8 + i * 4;
        let start = u16::from_be_bytes([payload[off], payload[off + 1]]);
        let end = u16::from_be_bytes([payload[off + 2], payload[off + 3]]);
        ranges.push((start, end));
    }
    Ok(VideoNack {
        target_member,
        target_stream_id,
        local_member,
        local_stream_id,
        is_screen,
        arq_seq,
        ranges,
    })
}

fn pack_rtcp_member(member: u16, stream_id: u8) -> [u8; 2] {
    let lo = (member & 0xff) as u8;
    let hi = (((member >> 8) & 0x07) as u8) | ((stream_id & 0x0f) << 3);
    [lo, hi]
}

fn unpack_rtcp_member(lo: u8, hi: u8) -> (u16, u8) {
    let member = u16::from(lo) | (u16::from(hi & 0x07) << 8);
    let stream_id = (hi >> 3) & 0x0f;
    (member, stream_id)
}

/// Parsed per-stream `video_s2p` QoS directive from a `0x0c` S2P message on `0x30`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoS2pCommand {
    /// QoS algorithm identifier (`field 1: u8Alg`).
    pub alg: u32,
    /// Target bitrate in kbps (`field 2: u16VKbps`).
    pub kbps: u32,
    /// Target frame rate (`field 3: u8Fps`).
    pub fps: u32,
    /// Resolution tier (`field 4: u8Resolution`).
    pub resolution: u32,
    /// Keyframe interval (`field 5: u16IPeriod`).
    pub i_period: u32,
    /// Reed-Solomon FEC level (`field 9: u8RsLevel`).
    pub rs_level: u32,
    /// Nonzero when the server requests an immediate keyframe (`field 10: u8IRequestFlag`).
    pub i_request_flag: u32,
    /// Server-commanded codec selector (`field 11: u8CodecType`).
    pub codec_type: u32,
    /// Target video stream identifier (`field 12: u32StreamID`).
    pub stream_id: u32,
}

/// Parsed `0x0c` MP S2P packet arriving on `packed_kind = 0x30` from relay member `0xffff`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedS2p {
    /// Room identifier from the outer S2P envelope.
    pub room_id: u64,
    /// Room key from the outer S2P envelope.
    pub room_key: u64,
    /// Target member ID from the outer S2P envelope.
    pub member_id: u32,
    /// Capability sequence counter if present (`field 5`).
    pub cap_seq: u32,
    /// Video QoS directives (`inner field 3: repeated video_s2p`).
    pub video_commands: Vec<VideoS2pCommand>,
}

/// Parse a `0x0c` MP S2P packet arriving on `packed_kind = 0x30`.
pub fn parse_s2p(payload: &[u8]) -> Result<ParsedS2p> {
    ensure!(
        payload.len() >= 2 && payload[0] == 0x0c,
        "Invalid MP S2P packet"
    );
    let outer = p::decode(&payload[1..])?;
    let mut parsed = ParsedS2p {
        room_id: opt_u64(&outer, 1)?.unwrap_or(0),
        room_key: opt_u64(&outer, 2)?.unwrap_or(0),
        member_id: opt_u32(&outer, 3)?.unwrap_or(0),
        cap_seq: opt_u32(&outer, 5)?.unwrap_or(0),
        video_commands: Vec::new(),
    };
    if let Some(inner_bytes) = p::optional_data(&outer, 4)? {
        let inner = p::decode(inner_bytes)?;
        for field in inner.iter().filter(|f| f.number == 3) {
            let Value::Bytes(raw) = &field.value else {
                continue;
            };
            let vf = p::decode(raw)?;
            parsed.video_commands.push(VideoS2pCommand {
                alg: opt_u32(&vf, 1)?.unwrap_or(0),
                kbps: opt_u32(&vf, 2)?.unwrap_or(0),
                fps: opt_u32(&vf, 3)?.unwrap_or(0),
                resolution: opt_u32(&vf, 4)?.unwrap_or(0),
                i_period: opt_u32(&vf, 5)?.unwrap_or(0),
                rs_level: opt_u32(&vf, 9)?.unwrap_or(0),
                i_request_flag: opt_u32(&vf, 10)?.unwrap_or(0),
                codec_type: opt_u32(&vf, 11)?.unwrap_or(0),
                stream_id: opt_u32(&vf, 12)?.unwrap_or(0),
            });
        }
    }
    Ok(parsed)
}

/// Per-interval video QoS telemetry reported in `0x0b` MP P2S packets (`video_p2s` + `video_new_p2s`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoP2sReport {
    /// Remote sender member ID (`video_p2s` field 1).
    pub remote_member_id: u32,
    /// Video stream ID (`video_p2s` field 2).
    pub stream_id: u32,
    /// Relay connection ID (`video_new_p2s.videoLink` field 1).
    pub conn_id: u32,
    /// Maximum inter-packet arrival gap in the reporting interval in milliseconds (`field 5`).
    pub recv_data_interval_ms: u32,
    /// Smoothed inter-arrival delay/jitter estimate in milliseconds (`field 6`).
    pub delay_ms: u32,
    /// Estimated receive bandwidth in kbps (`field 7`).
    pub est_bw_kbps: u32,
    /// This client's outbound video bitrate in kbps. Not encoded: P2S field 8 carries the
    /// remote sender's rate as received.
    pub send_kbps: u32,
    /// Inbound video bitrate in kbps (`field 9`).
    pub recv_kbps: u32,
    /// This client's outbound video frame rate. Not encoded: P2S field 10 carries the remote
    /// sender's frame rate as received.
    pub send_fps: u32,
    /// Inbound video frame rate (`field 11`).
    pub recv_fps: u32,
    /// This client's capture frame rate. Not encoded: P2S field 12 carries the remote
    /// sender's frame rate as received.
    pub capt_fps: u32,
    /// Packet loss percentage (`0..=100`, `field 14`).
    pub loss_pct: u32,
    /// Codecs the remote sender may send us (`u8VCodecSupport`, field 24): `0x08` HW H.264,
    /// `0x04` HW H.265, `0x10` SW H.265 (Vcodec2), `0x02` proprietary Vcodec1. Bit `0x01`
    /// (SW H.264 decode) is never valid here.
    pub codec_support_mask: u32,
    /// Lost packet count in the reporting interval (`field 26`).
    pub loss_pkts: u32,
    /// Active receive wire codec (`9` = H.264, `15` = H.265, `0` = Vcodec1). The encoder
    /// reports it as the decoder-type bit official clients use (`u32VDecodeType`, field 29).
    pub decode_type: u32,
    /// Rendered/delivered frame rate (`field 30`).
    pub play_fps: u32,
    /// Frames delivered in the reporting interval (`u32PlayFrames`, field 31).
    pub play_frames: u32,
    /// Largest picture size this receiver asks for (`u8MaxSizeFormat`, field 16): `4` = 320,
    /// `6` = 480, `8` = 640 wide. Lowering it makes the sender restart its encoder.
    pub max_size_format: u32,
    /// Light freeze event count (`200..500 ms` gaps, `field 32`).
    pub light_freeze_cnt: u32,
    /// Heavy freeze event count (`>= 500 ms` gaps, `field 33`).
    pub heavy_freeze_cnt: u32,
    /// Percentage of the interval spent in light freeze (`0..=100`, `field 34`).
    pub light_freeze_time_ratio: u32,
    /// Percentage of the interval spent in heavy freeze (`0..=100`, `field 35`).
    pub heavy_freeze_time_ratio: u32,
    /// Short-window packet loss percentage (`0..=100`, `field 47`).
    pub short_loss_pct: u32,
    /// Smoothed long-window packet loss percentage (`0..=100`, `field 48`).
    pub long_loss_pct: u32,
    /// Received packet count in the reporting interval (`videoLink` field 2).
    pub recv_pkgs: u32,
    /// Expected active remote member count (`videoLink` field 4).
    pub expected_member_num: u32,
    /// Actual active remote member count delivering video (`videoLink` field 5).
    pub actual_member_num: u32,
    /// Packets recovered via ARQ/NACK retransmission in the interval (`videoLink` field 6).
    pub recv_arq_resp_pkgs: u32,
}

impl Default for VideoP2sReport {
    fn default() -> Self {
        Self {
            remote_member_id: 0,
            stream_id: 1,
            conn_id: 0,
            recv_data_interval_ms: 0,
            delay_ms: 0,
            est_bw_kbps: 500,
            send_kbps: 500,
            recv_kbps: 500,
            send_fps: 15,
            recv_fps: 15,
            capt_fps: 15,
            loss_pct: 0,
            codec_support_mask: P2S_CODEC_SUPPORT_H264_H265,
            loss_pkts: 0,
            decode_type: 15,
            play_fps: 15,
            play_frames: 0,
            max_size_format: P2S_MAX_SIZE_FORMAT_640,
            light_freeze_cnt: 0,
            heavy_freeze_cnt: 0,
            light_freeze_time_ratio: 0,
            heavy_freeze_time_ratio: 0,
            short_loss_pct: 0,
            long_loss_pct: 0,
            recv_pkgs: 0,
            expected_member_num: 1,
            actual_member_num: 1,
            recv_arq_resp_pkgs: 0,
        }
    }
}

/// `u8VCodecSupport` for a receiver that decodes H.264 and H.265 (`0x08` HW H.264 |
/// `0x04` HW H.265 | `0x10` SW H.265). Proprietary Vcodec1 (`0x02`) stays unset.
pub const P2S_CODEC_SUPPORT_H264_H265: u32 = 0x1c;
/// `u8VCodecSupport` allowing only H.264 (`0x08`).
pub const P2S_CODEC_SUPPORT_H264: u32 = 0x08;
/// `u8VCodecSupport` allowing only H.265 (`0x04` HW | `0x10` SW).
pub const P2S_CODEC_SUPPORT_H265: u32 = 0x14;
/// `u8VCodecSupport` allowing only Vcodec1 (`0x02`), which this client rewraps as H.264.
pub const P2S_CODEC_SUPPORT_VCODEC1: u32 = 0x02;

/// `u8VCodecSupport` that keeps a sender on the codec it already uses (`wire_codec`: `9`
/// H.264, `15` H.265, `0` Vcodec1): offering a codec the sender cannot encode (for example
/// H.265 to an Android phone with only a hardware H.264 encoder) makes it fall back to
/// proprietary Vcodec1. A Vcodec1 sender is offered only Vcodec1: codec masks did not stop
/// the server from selecting it, and whether offering H.264 again moves the sender back is
/// untested. Other wire codecs get `fallback`.
pub fn p2s_codec_support_for(wire_codec: u32, fallback: u32) -> u32 {
    match wire_codec {
        0x00 => P2S_CODEC_SUPPORT_VCODEC1,
        0x09 => P2S_CODEC_SUPPORT_H264,
        0x0f => P2S_CODEC_SUPPORT_H265,
        _ => fallback,
    }
}
/// `u8MaxSizeFormat` asking for pictures up to 640 wide (the normal request).
pub const P2S_MAX_SIZE_FORMAT_640: u32 = 8;
/// `u8MaxSizeFormat` asking for pictures up to 480 wide.
pub const P2S_MAX_SIZE_FORMAT_480: u32 = 6;

/// Map an RSM wire codec byte to the decoder-type bit official receivers report in
/// `u32VDecodeType`: Vcodec1 → `2`, H.264 → `8` (HW H.264), H.265 → `4` (HW H.265),
/// anything else → `0`.
fn p2s_decoder_type(wire_codec: u32) -> u32 {
    match wire_codec {
        0x00 => 2,
        0x09 => 8,
        0x0f => 4,
        _ => 0,
    }
}

/// `head_p2s`: `{1: u8Type = 11, 2: u8Version = 0 (video/combined), 3: u16Seq,
/// 4: u32LocalMemberID, 5..7: client fields}`.
fn encode_p2s_client_header(sequence: u16, local_member: u16) -> Result<Vec<u8>> {
    p::message(&[
        p::uint(1, 11),
        p::uint(2, 0),
        p::uint(3, u64::from(sequence)),
        p::uint(4, u64::from(local_member)),
        p::uint(5, 4),
        p::uint(6, 111),
        p::uint(7, 0x050000cb),
    ])
}

/// Encode an `0x0b` MP P2S QoS heartbeat payload for `packed_kind = 0x30`
/// using the supplied live receiver/sender telemetry (`inner.f1` + `inner.f3.f1` + `inner.f6.f1`).
///
/// This is a video P2S: like official clients, it leaves the outer `p2s_type` (field 4)
/// unset. `p2s_type = 2` marks an audio P2S carrying `audio_p2s` (inner field 2).
pub fn encode_p2s_report(
    sequence: u16,
    cap_seq: u32,
    local_member: u16,
    report: &VideoP2sReport,
) -> Result<Vec<u8>> {
    let client_hdr = encode_p2s_client_header(sequence, local_member)?;
    let loss_clamped = report.loss_pct.min(100);
    let short_loss = report.short_loss_pct.min(100);
    let long_loss = report.long_loss_pct.min(100);
    // Fields 8/10/12 describe the remote sender's stream as received (official defaults
    // 500 kbps / 15 fps before any packet), not this client's own transmission.
    let remote_kbps = if report.recv_kbps > 0 {
        report.recv_kbps
    } else {
        500
    };
    let remote_fps = if report.recv_fps > 0 {
        report.recv_fps.clamp(3, 30)
    } else {
        15
    };
    let video_p2s = p::message(&[
        p::uint(1, u64::from(report.remote_member_id)),
        p::uint(2, u64::from(report.stream_id)),
        p::uint(4, 0x7e),
        p::uint(5, u64::from(report.recv_data_interval_ms)),
        p::uint(6, u64::from(report.delay_ms)),
        p::uint(7, u64::from(report.est_bw_kbps.min(u32::from(u16::MAX)))),
        p::uint(8, u64::from(remote_kbps.min(u32::from(u16::MAX)))),
        p::uint(9, u64::from(report.recv_kbps.min(u32::from(u16::MAX)))),
        p::uint(10, u64::from(remote_fps)),
        p::uint(11, u64::from(report.recv_fps.min(255))),
        p::uint(12, u64::from(remote_fps)),
        p::uint(14, u64::from(loss_clamped)),
        p::uint(15, 15),
        p::uint(16, u64::from(report.max_size_format.min(255))),
        // u8Cabac / u8Hpp / u8Denoise: 0, as official receivers report for H.264.
        p::uint(17, 0),
        p::uint(19, 0),
        p::uint(20, 0),
        p::uint(24, u64::from(report.codec_support_mask & 0xfe)),
        p::uint(26, u64::from(report.loss_pkts)),
        p::uint(29, u64::from(p2s_decoder_type(report.decode_type))),
        p::uint(30, u64::from(report.play_fps.min(255))),
        p::uint(31, u64::from(report.play_frames)),
        p::uint(32, u64::from(report.light_freeze_cnt)),
        p::uint(33, u64::from(report.heavy_freeze_cnt)),
        p::uint(34, u64::from(report.light_freeze_time_ratio.min(100))),
        p::uint(35, u64::from(report.heavy_freeze_time_ratio.min(100))),
        p::uint(47, u64::from(short_loss)),
        p::uint(48, u64::from(long_loss)),
    ])?;
    let video_wrapper = p::message(&[p::bytes(1, &video_p2s)])?;
    let mut inner_fields = vec![p::bytes(1, &client_hdr), p::bytes(3, &video_wrapper)];
    if report.conn_id != 0 || report.recv_pkgs != 0 || report.recv_arq_resp_pkgs != 0 {
        let video_link = p::message(&[
            p::uint(1, u64::from(report.conn_id)),
            p::uint(2, u64::from(report.recv_pkgs.min(u32::from(u16::MAX)))),
            p::uint(3, u64::from(loss_clamped)),
            p::uint(4, u64::from(report.expected_member_num.min(255))),
            p::uint(5, u64::from(report.actual_member_num.min(255))),
            p::uint(
                6,
                u64::from(report.recv_arq_resp_pkgs.min(u32::from(u16::MAX))),
            ),
        ])?;
        let video_new_wrapper = p::message(&[p::bytes(1, &video_link)])?;
        inner_fields.push(p::bytes(6, &video_new_wrapper));
    }
    let inner = p::message(&inner_fields)?;
    let outer = p::message(&[
        p::uint(1, u64::from(sequence)),
        p::uint(2, u64::from(cap_seq)),
        p::bytes(3, &inner),
    ])?;
    let mut out = Vec::with_capacity(1 + outer.len());
    out.push(0x0b);
    out.extend_from_slice(&outer);
    Ok(out)
}

/// Encode an `0x0b` MP P2S QoS heartbeat payload for `packed_kind = 0x30`
/// matching the nested `inner.f1` (client header) + `inner.f3.f1` (`video_p2s`) wire layout.
/// Like [`encode_p2s_report`], this video P2S leaves the outer `p2s_type` unset.
pub fn encode_p2s(
    sequence: u16,
    cap_seq: u32,
    local_member: u16,
    codec_support_mask: u32,
    decode_type: u32,
) -> Result<Vec<u8>> {
    let client_hdr = encode_p2s_client_header(sequence, local_member)?;
    let video_p2s = p::message(&[
        p::uint(1, 0),
        p::uint(2, 1),
        p::uint(4, 0x7e),
        p::uint(8, 500),
        p::uint(9, 500),
        p::uint(10, 15),
        p::uint(11, 15),
        p::uint(12, 15),
        p::uint(15, 15),
        p::uint(16, u64::from(P2S_MAX_SIZE_FORMAT_640)),
        p::uint(17, 0),
        p::uint(19, 0),
        p::uint(20, 0),
        p::uint(24, u64::from(codec_support_mask & 0xfe)),
        p::uint(29, u64::from(p2s_decoder_type(decode_type))),
    ])?;
    let video_wrapper = p::message(&[p::bytes(1, &video_p2s)])?;
    let inner = p::message(&[p::bytes(1, &client_hdr), p::bytes(3, &video_wrapper)])?;
    let outer = p::message(&[
        p::uint(1, u64::from(sequence)),
        p::uint(2, u64::from(cap_seq)),
        p::bytes(3, &inner),
    ])?;
    let mut out = Vec::with_capacity(1 + outer.len());
    out.push(0x0b);
    out.extend_from_slice(&outer);
    Ok(out)
}

/// Split an Annex-B H.264/H.265 access unit into parameter-set (XPS) bytes and
/// slice bytes, returning the 4-byte WeChat framing prefix plus payload bytes.
pub struct FramedAnnexB {
    /// Complete framed payload (`[len0_be16, len1_be16]` + Annex-B bytes).
    pub bytes: Vec<u8>,
    /// True when the access unit contains an IDR keyframe slice.
    pub is_keyframe: bool,
    /// Number of framed sections (`2` when XPS + slice are both present, else `1`).
    pub frame_count: u8,
    /// High byte of XPS length (`xps_len >> 16`) when `>= 65536`.
    pub xps_len_hi: u8,
    /// High byte of slice length (`slice_len >> 16`) when `>= 65536`.
    pub slice_len_hi: u8,
}

/// Frame a single standard Annex-B access unit (H.265 or H.264) with WeChat's
/// 4-byte `[xps_len_be16, idr_len_be16]` / `[slice_len_be16, 0, 0]` prefix.
pub fn frame_annexb_access_unit(codec: VideoWireCodec, annexb: &[u8]) -> Result<FramedAnnexB> {
    ensure!(
        !annexb.is_empty() && annexb.len() <= MAX_VIDEO_FRAME_BYTES,
        "Invalid Annex-B access unit size"
    );
    let nalus = split_annexb_nalus(annexb);
    ensure!(!nalus.is_empty(), "Missing Annex-B start codes");

    let mut xps = Vec::new();
    let mut slices = Vec::new();
    let mut is_keyframe = false;

    for &(start, end, sc_len) in &nalus {
        let header_byte = annexb[start + sc_len];
        let (is_param, is_idr) = classify_nalu(codec, header_byte);
        if is_idr {
            is_keyframe = true;
        }
        if is_param {
            xps.extend_from_slice(&annexb[start..end]);
        } else {
            slices.extend_from_slice(&annexb[start..end]);
        }
    }

    ensure!(
        !slices.is_empty() || !xps.is_empty(),
        "Empty Annex-B access unit"
    );
    if slices.is_empty() {
        slices = std::mem::take(&mut xps);
    }

    let (len0, len1, frame_count, xps_hi, slice_hi) = if !xps.is_empty() {
        let xlen = xps.len();
        let slen = slices.len();
        (
            (xlen & 0xffff) as u16,
            (slen & 0xffff) as u16,
            2u8,
            ((xlen >> 16) & 0xff) as u8,
            ((slen >> 16) & 0xff) as u8,
        )
    } else {
        let slen = slices.len();
        (
            (slen & 0xffff) as u16,
            0u16,
            1u8,
            0u8,
            ((slen >> 16) & 0xff) as u8,
        )
    };

    let mut bytes = Vec::with_capacity(4 + xps.len() + slices.len());
    bytes.extend_from_slice(&len0.to_be_bytes());
    bytes.extend_from_slice(&len1.to_be_bytes());
    bytes.extend_from_slice(&xps);
    bytes.extend_from_slice(&slices);

    Ok(FramedAnnexB {
        bytes,
        is_keyframe,
        frame_count,
        xps_len_hi: xps_hi,
        slice_len_hi: slice_hi,
    })
}

/// Strip WeChat's 4-byte framing prefix from a reassembled MP/iLink payload,
/// returning standard Annex-B bytes plus keyframe/codec-config flags.
pub fn unframe_annexb_access_unit(
    codec: VideoWireCodec,
    framed: &[u8],
    xps_len_hi: u8,
    slice_len_hi: u8,
) -> Result<(Vec<u8>, bool, bool)> {
    unframe_annexb_access_unit_with_count(codec, framed, 0, xps_len_hi, slice_len_hi)
}

fn unframe_annexb_access_unit_with_count(
    codec: VideoWireCodec,
    framed: &[u8],
    frame_count: u8,
    xps_len_hi: u8,
    slice_len_hi: u8,
) -> Result<(Vec<u8>, bool, bool)> {
    ensure!(framed.len() >= 4, "Truncated framed video access unit");
    let first_lo = u16::from_be_bytes([framed[0], framed[1]]) as usize;
    let second_lo = u16::from_be_bytes([framed[2], framed[3]]) as usize;
    let has_two_sections =
        second_lo != 0 || xps_len_hi != 0 || (frame_count >= 2 && slice_len_hi != 0);
    let (total_len, has_xps) = if has_two_sections {
        let xps_len = first_lo | (usize::from(xps_len_hi) << 16);
        let slice_len = second_lo | (usize::from(slice_len_hi) << 16);
        (xps_len.saturating_add(slice_len), xps_len > 0)
    } else {
        let slice_len = first_lo | (usize::from(slice_len_hi) << 16);
        (slice_len, false)
    };
    ensure!(
        total_len > 0 && total_len <= MAX_VIDEO_FRAME_BYTES && 4 + total_len <= framed.len(),
        "Invalid framed video access unit length"
    );
    let annexb = framed[4..4 + total_len].to_vec();
    let nalus = split_annexb_nalus(&annexb);
    let mut is_keyframe = false;
    let mut has_codec_config = has_xps;
    for &(start, _end, sc_len) in &nalus {
        let (is_param, is_idr) = classify_nalu(codec, annexb[start + sc_len]);
        has_codec_config |= is_param;
        is_keyframe |= is_idr;
    }
    Ok((annexb, is_keyframe, has_codec_config))
}

fn classify_nalu(codec: VideoWireCodec, header_byte: u8) -> (bool, bool) {
    match codec {
        VideoWireCodec::H265 => {
            let nal_type = (header_byte >> 1) & 0x3f;
            let is_param = matches!(nal_type, 32..=34);
            let is_idr = matches!(nal_type, 16..=21);
            (is_param, is_idr)
        }
        VideoWireCodec::H264 => {
            let nal_type = header_byte & 0x1f;
            let is_param = matches!(nal_type, 7 | 8);
            let is_idr = nal_type == 5;
            (is_param, is_idr)
        }
        _ => (false, false),
    }
}

/// Locate `(start, end, start_code_len)` spans for every Annex-B NALU in `data`.
pub fn split_annexb_nalus(data: &[u8]) -> Vec<(usize, usize, usize)> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                starts.push((i, 3));
                i += 3;
                continue;
            }
            if i + 4 < data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                starts.push((i, 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    let mut out = Vec::with_capacity(starts.len());
    for idx in 0..starts.len() {
        let (start, sc_len) = starts[idx];
        let end = starts.get(idx + 1).map_or(data.len(), |&(next, _)| next);
        if start + sc_len < end {
            out.push((start, end, sc_len));
        }
    }
    out
}

/// Return `true` when an Annex-B access unit contains an IDR/IRAP keyframe NALU for `codec`.
pub fn is_annexb_keyframe(codec: VideoWireCodec, annexb: &[u8]) -> bool {
    split_annexb_nalus(annexb)
        .into_iter()
        .any(|(start, _end, sc_len)| classify_nalu(codec, annexb[start + sc_len]).1)
}

/// Extract the wire NALU type number for every Annex-B NALU in `annexb`.
pub fn annexb_nalu_types(codec: VideoWireCodec, annexb: &[u8]) -> Vec<u8> {
    split_annexb_nalus(annexb)
        .into_iter()
        .map(|(start, _end, sc_len)| {
            let header_byte = annexb[start + sc_len];
            match codec {
                VideoWireCodec::H265 => (header_byte >> 1) & 0x3f,
                _ => header_byte & 0x1f,
            }
        })
        .collect()
}

/// Advance a monotonic 64-bit sequence counter from a wrapping 16-bit wire sequence number.
///
/// When `prev` is `None`, anchors at `u64::from(seq16)` (supporting high initial values such as
/// `>= 0x8000`). Older or out-of-order sequences (negative signed 16-bit delta) leave the
/// counter unchanged.
pub const fn advance_seq16(prev: Option<u64>, seq16: u16) -> u64 {
    match prev {
        None => seq16 as u64,
        Some(current) => {
            let delta = seq16.wrapping_sub(current as u16) as i16;
            if delta > 0 {
                current.wrapping_add(delta as u64)
            } else {
                current
            }
        }
    }
}

/// Parsed header of an MP/iLink kind-3 RSM video packet (`[codecType, mid/sid, EncFecHeader, TLV]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RsmPacketHeader {
    /// Wire codec identifier from byte 0 of the MP/iLink prefix.
    pub codec: VideoWireCodec,
    /// Sender member ID from bytes 1..3.
    pub member_id: u16,
    /// Video stream ID from bytes 1..3 (`0..=15`).
    pub stream_id: u8,
    /// Full reconstructed 16-bit frame index (`(frame_idx_hi << 5) | frame_idx_lo`).
    pub frame_index: u32,
    /// Wrapping 16-bit total packet sequence (`EncFecHeader[2..4]`, incremented for all kind-3 packets).
    pub total_pkg_seq: u16,
    /// Wrapping 16-bit media packet sequence (`EncFecHeader[4..6]`), or `None` for virtual-frame
    /// metadata and degenerate/padding packets outside the media sequence space.
    pub pkg_seq: Option<u16>,
    /// Sender millisecond timestamp (`EncFecHeader[6..10]`).
    pub timestamp_ms: u32,
    /// Raw 3-bit orientation code from `EncFecHeader` byte 13 bits 7..5 (`0..=7`).
    pub orientation: u8,
    /// Display orientation parsed from a virtual-frame `VideoPktExtHeader` protobuf (`field 3`), if present.
    pub virtual_frame_orientation: Option<u8>,
    /// True when `EncFecHeader` byte 0 bit 2 marks an IDR/keyframe packet.
    pub is_iframe: bool,
    /// True when the header matches a virtual-frame metadata packet (`s = 1, r = 0, x = 1, pkg_idx = 3`).
    pub is_virtual_frame: bool,
    /// Number of framed sections (`(hdr[12] & 0x03) + 1`, `1..=4`).
    pub frame_count: u8,
    /// Number of horizontal FEC groups (`1..=7`).
    pub x_groups: u8,
    /// Zero-based horizontal FEC group index (`0..=7`).
    pub group_idx_x: u8,
    /// Number of 2D vertical FEC parity groups (`0..=7`).
    pub y_groups: u8,
    /// True when this packet carries 2D vertical RS parity data.
    pub is_vert_parity: bool,
    /// Zero-based vertical FEC parity group index (`0..=7`).
    pub vert_group_idx: u8,
    /// Source chunk count in this horizontal group (`s_per_group`).
    pub s_per_group: u16,
    /// Horizontal Reed-Solomon parity count in this group (`r_per_group`).
    pub r_per_group: u16,
    /// One-based packet index within its FEC group (`1..=s_per_group + r_per_group`).
    pub pkg_idx_in_group: u16,
    /// One-based packet index within the overall frame (`f_pkt_idx`).
    pub frame_pkt_idx: u16,
    /// Total advertised packets in the overall frame (`f_tot_pkts`).
    pub frame_total_pkts: u16,
    /// High byte of XPS section length (`xps_len >> 16`) from TLV `0x0002`.
    pub xps_len_hi: u8,
    /// High byte of slice section length (`slice_len >> 16`) from TLV `0x0002`.
    pub slice_len_hi: u8,
    /// Byte offset where the RSM packet payload starts (`3 + 21 + ext_len`).
    pub payload_offset: usize,
}

/// Parse the 3-byte MP/iLink prefix, 21-byte kind-3 `EncFecHeader`, and TLV extensions of a
/// decrypted RSM video packet without mutating reassembly state.
pub fn parse_rsm_packet_header(packet: &[u8]) -> Result<RsmPacketHeader> {
    ensure!(
        packet.len() >= 3 + 21,
        "Truncated MP/iLink RSM video packet"
    );
    let codec = VideoWireCodec::from_wire(packet[0]);
    let ms = u16::from_le_bytes([packet[1], packet[2]]);
    let member_id = ms & 0x7ff;
    let stream_id = ((ms >> 11) & 0x0f) as u8;

    let hdr = &packet[3..];
    let hkind = hdr[0] & 0x03;
    ensure!(hkind == 3, "Unsupported RSM video header kind {hkind}");
    let is_iframe = (hdr[0] & 0x04) != 0;
    let frame_idx_lo = u32::from(hdr[0] >> 3);

    let r_lo2 = u32::from(hdr[1] & 0x03);
    let pkg_idx_lo6 = usize::from(hdr[1] >> 2);
    let total_pkg_seq = u16::from_be_bytes([hdr[2], hdr[3]]);
    let raw_pkg_seq = u16::from_be_bytes([hdr[4], hdr[5]]);
    let timestamp_ms = u32::from_be_bytes([hdr[6], hdr[7], hdr[8], hdr[9]]);
    let frame_count = (hdr[12] & 0x03) + 1;
    let s_lo6 = usize::from(hdr[12] >> 2);
    let orientation = hdr[13] >> 5;
    let r_mid2 = u32::from(hdr[13] & 0x03);
    let r_hi4 = u32::from(hdr[14] & 0x0f);
    // RS parity count uses additive overflow across `r_lo2` (0..=3), `r_mid2` (0..=3),
    // and `r_hi4` (0..=15), matching `s_per_group` and `pkg_idx_in_group`.
    let r_full = (r_lo2 + r_mid2 + r_hi4) as usize;
    let group_idx_x = hdr[14] >> 5;
    let x_groups = (hdr[15] & 0x07).max(1);

    let s_ovf_lo = usize::from(hdr[16]);
    let pkg_ovf_lo = usize::from(hdr[17]);
    let f_pkt_lo = usize::from(hdr[18]);
    let f_tot_lo = usize::from(hdr[19]);
    let ext_len = usize::from(hdr[20]);
    ensure!(21 + ext_len <= hdr.len(), "Truncated RSM video extension");

    let ext = &hdr[21..21 + ext_len];
    let mut s_ovf_hi = 0usize;
    let mut pkg_ovf_hi = 0usize;
    let mut f_pkt_hi = 0usize;
    let mut f_tot_hi = 0usize;
    let mut xps_len_hi = 0u8;
    let mut slice_len_hi = 0u8;
    let mut frame_idx_hi = 0u32;
    let mut y_groups = 0u8;
    let mut is_vert_parity = false;
    let mut vert_group_idx = 0u8;

    if ext_len >= 2 {
        let ext_mask = u16::from_be_bytes([ext[0], ext[1]]);
        let mut pos = 2usize;
        if ext_mask & 0x0001 != 0 && pos + 2 <= ext_len {
            s_ovf_hi = usize::from(ext[pos] & 0x0f);
            pkg_ovf_hi = usize::from(ext[pos] >> 4);
            f_pkt_hi = usize::from(ext[pos + 1] & 0x0f);
            f_tot_hi = usize::from(ext[pos + 1] >> 4);
            pos += 2;
        }
        if ext_mask & 0x0002 != 0 && pos + 2 <= ext_len {
            xps_len_hi = ext[pos];
            slice_len_hi = ext[pos + 1];
            pos += 2;
        }
        if ext_mask & 0x0004 != 0 && pos + 2 <= ext_len {
            frame_idx_hi = u32::from(ext[pos]) | (u32::from(ext[pos + 1] & 0x07) << 8);
            pos += 2;
        }
        if ext_mask & 0x0010 != 0 && !is_iframe && pos < ext_len {
            let b = ext[pos];
            let num_entries = usize::from(b & 0x3f);
            let bits_per_entry = usize::from(b >> 6) + 1;
            let bit_bytes = if num_entries == 0 {
                0
            } else {
                (num_entries * bits_per_entry).div_ceil(8)
            };
            pos = pos.saturating_add(1 + bit_bytes);
        }
        if ext_mask & 0x0020 != 0 && pos < ext_len {
            let b = ext[pos];
            y_groups = b & 0x07;
            is_vert_parity = (b & 0x08) != 0;
            vert_group_idx = (b >> 4) & 0x07;
            pos += 1;
        }
        let _ = pos;
    }

    let s_per_group = (s_lo6 + s_ovf_lo + (s_ovf_hi << 8)) as u16;
    let r_per_group = r_full as u16;
    let pkg_idx_in_group = (pkg_idx_lo6 + pkg_ovf_lo + (pkg_ovf_hi << 8)) as u16;
    let frame_pkt_idx = (f_pkt_lo + (f_pkt_hi << 8)) as u16;
    let frame_total_pkts = (f_tot_lo + (f_tot_hi << 8)) as u16;
    let frame_index = (frame_idx_hi << 5) | frame_idx_lo;
    let payload_offset = 3 + 21 + ext_len;
    let payload = &packet[payload_offset..];

    // Virtual-frame metadata packets (`s = 1, r = 0, x = 1, pkg_idx = 3`) carry the
    // `workMode = 3` protobuf header (including `field 3 = orientation`) after a
    // 128-byte leading block and do not advance `pkg_seq`.
    let is_virtual_frame =
        s_per_group == 1 && r_per_group == 0 && x_groups == 1 && pkg_idx_in_group == 3;
    let virtual_frame_orientation = if is_virtual_frame && payload.len() > 0x80 {
        p::decode(&payload[0x80..])
            .ok()
            .and_then(|fields| opt_u32(&fields, 3).ok().flatten())
            .map(|v| (v & 0x07) as u8)
    } else {
        None
    };
    let pkg_seq = (!is_virtual_frame
        && frame_total_pkts > 0
        && frame_pkt_idx > 0
        && s_per_group > 0
        && pkg_idx_in_group > 0)
        .then_some(raw_pkg_seq);

    Ok(RsmPacketHeader {
        codec,
        member_id,
        stream_id,
        frame_index,
        total_pkg_seq,
        pkg_seq,
        timestamp_ms,
        orientation,
        virtual_frame_orientation,
        is_iframe,
        is_virtual_frame,
        frame_count,
        x_groups,
        group_idx_x,
        y_groups,
        is_vert_parity,
        vert_group_idx,
        s_per_group,
        r_per_group,
        pkg_idx_in_group,
        frame_pkt_idx,
        frame_total_pkts,
        xps_len_hi,
        slice_len_hi,
        payload_offset,
    })
}

/// Stateful MP/iLink kind-3 RSM video packetizer (zero-FEC uncoded `x = 1, r = 0, y = 0`).
pub struct RsmVideoPacketizer {
    local_member: u16,
    stream_id: u8,
    chunk_bytes: usize,
    orientation: u8,
    frame_idx: u32,
    total_pkg_seq: u16,
    pkg_seq: u16,
    bw_est_seq: u8,
}

impl RsmVideoPacketizer {
    /// Construct a new packetizer bound to a local member and video stream ID.
    pub fn new(local_member: u16, stream_id: u8) -> Self {
        Self {
            local_member,
            stream_id: stream_id & 0x0f,
            chunk_bytes: DEFAULT_RSM_CHUNK_BYTES,
            orientation: DEFAULT_RSM_ORIENTATION,
            frame_idx: 0,
            total_pkg_seq: 0,
            pkg_seq: 0,
            bw_est_seq: 1,
        }
    }

    /// Override the per-packet payload chunk budget (bounded to `64..=1350`).
    pub fn with_chunk_bytes(mut self, chunk_bytes: usize) -> Result<Self> {
        ensure!(
            (64..=1350).contains(&chunk_bytes),
            "Invalid RSM video chunk size"
        );
        self.chunk_bytes = chunk_bytes;
        Ok(self)
    }

    /// Override the 3-bit wire orientation code (`0..=7`) written into `EncFecHeader` byte 13
    /// and virtual-frame orientation metadata packets.
    pub fn with_orientation(mut self, orientation: u8) -> Self {
        self.orientation = orientation & 0x07;
        self
    }

    /// Build one MP/iLink kind-3 virtual-frame metadata packet carrying the
    /// `workMode = 3` `VideoPktExtHeader` protobuf (`field 3 = orientation`,
    /// `s = 1, r = 0, pkg_idx = 3`, 128-byte leading padding) so mobile and
    /// desktop receivers apply the sender's display orientation.
    pub fn build_orientation_metadata_packet(
        &mut self,
        codec: VideoWireCodec,
        timestamp_ms: u32,
        fps: u8,
    ) -> Result<Vec<u8>> {
        self.total_pkg_seq = self.total_pkg_seq.wrapping_add(1);
        let bw_seq = self.bw_est_seq;
        self.bw_est_seq = if bw_seq == 255 { 1 } else { bw_seq + 1 };

        let frame_idx_lo = ((self.frame_idx & 0x1f) ^ 0x10) as u8;
        let frame_idx_hi = ((self.frame_idx >> 5) & 0x07ff) as u16;
        let mp_member_stream =
            (self.local_member & 0x7ff) | (u16::from(self.stream_id & 0x0f) << 11);
        let proto = p::message(&[
            p::uint(1, u64::from(fps.max(1))),
            p::uint(2, 0),
            p::uint(3, u64::from(self.orientation & 0x07)),
            p::uint(4, u64::from(self.frame_idx)),
            p::uint(5, 0),
            p::uint(6, 0),
            p::uint(7, 0),
            p::uint(8, 0),
            p::uint(9, 0),
            p::uint(16, 0),
        ])?;

        // 5-byte TLV extension matching the kind-3 virtual-frame header (`ext_mask = 0x004c`).
        let ext: [u8; 5] = [
            0x00,
            0x4c,
            (frame_idx_hi & 0xff) as u8,
            (((frame_idx_hi >> 8) & 0x07) as u8) | 0x08,
            bw_seq,
        ];
        let mut pkt = Vec::with_capacity(3 + 21 + ext.len() + 0x80 + proto.len());
        pkt.push(codec.wire());
        pkt.extend_from_slice(&mp_member_stream.to_le_bytes());

        pkt.push(3 | (frame_idx_lo << 3)); // hkind = 3, is_iframe = 0
        pkt.push(3 << 2); // pkg_idx_lo6 = 3, r_lo2 = 0 (virtual frame marker)
        pkt.extend_from_slice(&self.total_pkg_seq.to_be_bytes());
        pkt.extend_from_slice(&0u16.to_be_bytes()); // virtual frames do not advance pkg_seq
        pkt.extend_from_slice(&timestamp_ms.to_be_bytes());
        pkt.push(0);
        pkt.push(0);
        pkt.push((1 << 2) | 3); // s_lo6 = 1, frame_count = 0 -> (0 - 1) & 3 = 3
        pkt.push(5 << 5); // workMode = 3 virtual-frame header code
        pkt.push(0x00);
        pkt.push(0x00); // x_groups = 0 in virtual-frame metadata packets
        pkt.extend_from_slice(&[0, 0, 0, 0]); // s_ovf = 0, pkg_ovf = 0, f_pkt = 0, f_tot = 0
        pkt.push(ext.len() as u8);
        pkt.extend_from_slice(&ext);
        pkt.resize(pkt.len() + 0x80, 0);
        pkt.extend_from_slice(&proto);
        Ok(pkt)
    }

    /// Split one standard Annex-B access unit into ready-to-seal MP/iLink video packets.
    pub fn packetize(
        &mut self,
        codec: VideoWireCodec,
        annexb: &[u8],
        timestamp_ms: u32,
        fps: u8,
        kbps: u16,
    ) -> Result<Vec<Vec<u8>>> {
        let framed = frame_annexb_access_unit(codec, annexb)?;
        let s = framed.bytes.len().div_ceil(self.chunk_bytes);
        ensure!(
            (1..=MAX_FRAGMENTS_PER_FRAME).contains(&s),
            "Video frame exceeds RSM fragment limit"
        );
        self.frame_idx = self.frame_idx.wrapping_add(1);
        let frame_idx = self.frame_idx;
        let frame_idx_lo = (frame_idx & 0x1f) as u8;
        let frame_idx_hi = ((frame_idx >> 5) & 0x07ff) as u16;
        let s_lo6 = (s & 0x3f) as u8;
        let s_overflow = s.saturating_sub(usize::from(s_lo6));
        let s_overflow_lo8 = (s_overflow & 0xff) as u8;
        let needs_hi_nibbles = s >= 256;
        let needs_len_hi = framed.xps_len_hi != 0 || framed.slice_len_hi != 0;

        let mut ext_mask: u16 = 0x0044; // 0x0004 (frame_idx_hi) | 0x0040 (bw_est_seq)
        if needs_hi_nibbles {
            ext_mask |= 0x0001;
        }
        if needs_len_hi {
            ext_mask |= 0x0002;
        }

        let mp_member_stream =
            (self.local_member & 0x7ff) | (u16::from(self.stream_id & 0x0f) << 11);
        let mut packets = Vec::with_capacity(s);

        for idx in 0..s {
            let pkt_1based = idx + 1;
            self.total_pkg_seq = self.total_pkg_seq.wrapping_add(1);
            self.pkg_seq = self.pkg_seq.wrapping_add(1);
            let bw_seq = self.bw_est_seq;
            self.bw_est_seq = if bw_seq == 255 { 1 } else { bw_seq + 1 };

            let pkg_idx_lo6 = (pkt_1based & 0x3f) as u8;
            let pkg_idx_overflow = pkt_1based.saturating_sub(usize::from(pkg_idx_lo6));
            let pkg_idx_overflow_lo8 = (pkg_idx_overflow & 0xff) as u8;
            let f_pkt_lo8 = (pkt_1based & 0xff) as u8;
            let f_tot_lo8 = (s & 0xff) as u8;

            let mut ext = Vec::with_capacity(11);
            ext.extend_from_slice(&ext_mask.to_be_bytes());
            if needs_hi_nibbles {
                let b0 = (((s_overflow >> 8) & 0x0f) as u8)
                    | ((((pkg_idx_overflow >> 8) & 0x0f) as u8) << 4);
                let b1 = (((pkt_1based >> 8) & 0x0f) as u8) | ((((s >> 8) & 0x0f) as u8) << 4);
                ext.push(b0);
                ext.push(b1);
            }
            if needs_len_hi {
                ext.push(framed.xps_len_hi);
                ext.push(framed.slice_len_hi);
            }
            // Official kind-3 TLV packs 11-bit frame_idx_hi little-endian (lo8, hi3 in bits 2..0).
            ext.push((frame_idx_hi & 0xff) as u8);
            ext.push(((frame_idx_hi >> 8) & 0x07) as u8);
            ext.push(bw_seq);

            let chunk_start = idx * self.chunk_bytes;
            let chunk_end = ((idx + 1) * self.chunk_bytes).min(framed.bytes.len());
            let chunk = &framed.bytes[chunk_start..chunk_end];

            let mut pkt = Vec::with_capacity(3 + 21 + ext.len() + chunk.len());
            // Layer A: 3-byte MP/iLink prefix.
            pkt.push(codec.wire());
            pkt.extend_from_slice(&mp_member_stream.to_le_bytes());

            // Layer B: 21-byte kind-3 EncFecHeader + TLV extension.
            pkt.push(3 | (u8::from(framed.is_keyframe) << 2) | (frame_idx_lo << 3));
            pkt.push(pkg_idx_lo6 << 2); // r_low2 = 0 (uncoded)
            pkt.extend_from_slice(&self.total_pkg_seq.to_be_bytes());
            pkt.extend_from_slice(&self.pkg_seq.to_be_bytes());
            pkt.extend_from_slice(&timestamp_ms.to_be_bytes());
            pkt.push((fps & 0x1f) | (((kbps >> 8) as u8 & 0x07) << 5));
            pkt.push((kbps & 0xff) as u8);
            pkt.push(((framed.frame_count.saturating_sub(1)) & 0x03) | (s_lo6 << 2));
            pkt.push((self.orientation & 0x07) << 5); // orient, temporal = 0, bw_probe = 0, r_mid2 = 0
            pkt.push(0x00); // r_hi4 = 0, ext_flag = 0, group_idx_x = 0
            pkt.push(1 | (1 << 3) | (((kbps >> 11) as u8 & 0x03) << 4) | (1 << 6)); // x=1, hpp=1, sw_enc=1
            pkt.push(s_overflow_lo8);
            pkt.push(pkg_idx_overflow_lo8);
            pkt.push(f_pkt_lo8);
            pkt.push(f_tot_lo8);
            pkt.push(ext.len() as u8);
            pkt.extend_from_slice(&ext);

            // Layer C: uncoded chunk bytes (zero-padded only when multiple packets exist to equalize chunk size).
            pkt.extend_from_slice(chunk);
            if s > 1 && chunk.len() < self.chunk_bytes {
                pkt.resize(pkt.len() + (self.chunk_bytes - chunk.len()), 0);
            }
            packets.push(pkt);
        }
        Ok(packets)
    }
}

/// Complete reassembled video access unit produced by [`RsmVideoReassembler`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReassembledVideoFrame {
    /// Wire codec from byte 0 of the MP/iLink prefix.
    pub codec: VideoWireCodec,
    /// Sender member ID from bytes 1..3.
    pub member_id: u16,
    /// Video stream ID from bytes 1..3.
    pub stream_id: u8,
    /// Full reconstructed frame index.
    pub frame_index: u32,
    /// Sender millisecond timestamp from the RSM header.
    pub timestamp_ms: u32,
    /// Raw 3-bit orientation code from `EncFecHeader` byte 13 bits 7..5 (`0..=7`).
    pub orientation: u8,
    /// True when an older incomplete frame was evicted before or alongside this frame.
    pub prior_frames_dropped: bool,
    /// True when one or more missing UDP fragments were recovered via Reed-Solomon FEC.
    pub fec_recovered: bool,
    /// True when the frame contains an IDR keyframe slice.
    pub is_keyframe: bool,
    /// True when the frame includes codec parameter sets (VPS/SPS/PPS).
    pub has_codec_config: bool,
    /// Standard Annex-B access unit bytes (for H.264/H.265) or raw codec bytes for other codecs.
    pub payload: Vec<u8>,
}

/// Outcome of feeding one RSM video packet into [`RsmVideoReassembler`].
#[derive(Debug, PartialEq, Eq)]
pub enum RsmIngestOutcome {
    /// Packet accepted into an in-progress frame assembly or skipped as parity/probe.
    Pending,
    /// A complete frame was assembled.
    Frame(ReassembledVideoFrame),
    /// An older incomplete frame had to be evicted; caller may request a keyframe.
    FrameDroppedNeedKeyframe,
}

struct PendingAssembly {
    codec: VideoWireCodec,
    member_id: u16,
    stream_id: u8,
    timestamp_ms: u32,
    orientation: u8,
    is_iframe: bool,
    frame_count: u8,
    x_groups: usize,
    y_groups: usize,
    xps_len_hi: u8,
    slice_len_hi: u8,
    /// Per-horizontal-group source packet counts (`s_per_group` per `group_idx_x`).
    /// In 1D multi-group frames (`x_groups >= 2, y_groups == 0`), the trailing group
    /// carries fewer source packets when total source chunks is not divisible by `x_groups`.
    group_s: Vec<Option<usize>>,
    /// Per-horizontal-group RS parity packet counts (`r_per_group` per `group_idx_x`).
    group_r: Vec<Option<usize>>,
    /// Uniform `s_per_group` advertised on 2D (`y_groups > 0`) packets, when present.
    vert_s_per_group: Option<usize>,
    /// Per-horizontal-group packets keyed by `slot_in_group` (`0..s_g` source, `s_g..s_g+r_g` parity).
    groups: Vec<BTreeMap<usize, Vec<u8>>>,
    /// 2D vertical parity packets keyed by `(vert_group_idx, col)` where `col in 0..s`.
    vert_parity: BTreeMap<(usize, usize), Vec<u8>>,
    fec_recovered: bool,
    arrival_order: u64,
}

fn grace_frames_for(is_iframe: bool, r_per_group: usize, y_groups: usize) -> u32 {
    if is_iframe {
        IFRAME_GRACE_FRAMES
    } else if r_per_group > 0 || y_groups > 0 {
        FEC_P_GRACE_FRAMES
    } else {
        0
    }
}

impl PendingAssembly {
    fn max_r(&self) -> usize {
        self.group_r.iter().flatten().copied().max().unwrap_or(0)
    }

    fn grace_frames(&self) -> u32 {
        grace_frames_for(self.is_iframe, self.max_r(), self.y_groups)
    }

    fn effective_group_s(&self, g: usize) -> Option<usize> {
        self.group_s
            .get(g)
            .copied()
            .flatten()
            .or(self.vert_s_per_group)
    }

    fn source_chunks_complete(&self) -> bool {
        (0..self.x_groups).all(|g| {
            self.effective_group_s(g)
                .is_some_and(|s_g| s_g > 0 && (0..s_g).all(|col| self.groups[g].contains_key(&col)))
        })
    }

    /// Attempt horizontal (and, when present, 2D vertical) systematic RS erasure recovery.
    /// Returns `true` if any missing source packet was recovered.
    fn try_fec_recover(&mut self) -> bool {
        let mut recovered_any = false;
        recovered_any |= self.recover_horizontal_groups();
        if !self.source_chunks_complete()
            && self.y_groups > 0
            && self.x_groups >= 2
            && self.recover_vertical_columns()
        {
            recovered_any = true;
            recovered_any |= self.recover_horizontal_groups();
        }
        recovered_any
    }

    fn recover_horizontal_groups(&mut self) -> bool {
        let mut recovered = false;
        for g in 0..self.x_groups {
            let Some(s_g) = self.group_s[g].or(self.vert_s_per_group) else {
                continue;
            };
            let Some(r_g) = self.group_r[g] else {
                continue;
            };
            if r_g == 0 || s_g == 0 || s_g + r_g > 255 {
                continue;
            }
            let group = &mut self.groups[g];
            let missing_src = (0..s_g).filter(|col| !group.contains_key(col)).count();
            if missing_src == 0 || group.len() < s_g {
                continue;
            }
            if rs_recover_group(s_g, r_g, group) {
                recovered = true;
            }
        }
        recovered
    }

    fn recover_vertical_columns(&mut self) -> bool {
        let Some(s_vert) = self.vert_s_per_group else {
            return false;
        };
        if self.y_groups == 0
            || self.x_groups < 2
            || s_vert == 0
            || self.x_groups + self.y_groups > 255
        {
            return false;
        }
        let mut recovered = false;
        for col in 0..s_vert {
            let mut col_map = BTreeMap::<usize, Vec<u8>>::new();
            let mut missing_src = 0usize;
            for g in 0..self.x_groups {
                if let Some(pkt) = self.groups[g].get(&col) {
                    col_map.insert(g, pkt.clone());
                } else {
                    missing_src += 1;
                }
            }
            if missing_src == 0 {
                continue;
            }
            for vy in 0..self.y_groups {
                if let Some(pkt) = self.vert_parity.get(&(vy, col)) {
                    col_map.insert(self.x_groups + vy, pkt.clone());
                }
            }
            if col_map.len() < self.x_groups {
                continue;
            }
            if rs_recover_group(self.x_groups, self.y_groups, &mut col_map) {
                for g in 0..self.x_groups {
                    if !self.groups[g].contains_key(&col) {
                        if let Some(pkt) = col_map.remove(&g) {
                            self.groups[g].insert(col, pkt);
                            self.group_s[g] = Some(s_vert);
                            recovered = true;
                        }
                    }
                }
            }
        }
        recovered
    }
}

#[derive(Default)]
struct StreamReassembler {
    active: BTreeMap<u32, PendingAssembly>,
    ready: BTreeMap<u32, ReassembledVideoFrame>,
    last_completed: Option<u32>,
    last_timestamp_ms: Option<u32>,
    arrival_counter: u64,
    vf_orientation: Option<u8>,
    pending_drops: Vec<u32>,
}

/// Returns `true` when `earlier` is strictly older than `later` in 16-bit modular frame order.
fn is_frame_older(earlier: u32, later: u32) -> bool {
    let diff = later.wrapping_sub(earlier) & 0xffff;
    diff > 0 && diff < 0x8000
}

fn frame_distance(earlier: u32, later: u32) -> u32 {
    later.wrapping_sub(earlier) & 0xffff
}

fn oldest_frame_key<T>(map: &BTreeMap<u32, T>) -> Option<u32> {
    let mut keys = map.keys().copied();
    let mut oldest = keys.next()?;
    for key in keys {
        if is_frame_older(key, oldest) {
            oldest = key;
        }
    }
    Some(oldest)
}

/// Reassembles MP/iLink kind-3 RSM video packets (including multi-group and 2D RS FEC)
/// into complete Annex-B frames, keeping independent state per `stream_id`.
pub struct RsmVideoReassembler {
    streams: BTreeMap<u8, StreamReassembler>,
}

impl Default for RsmVideoReassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl RsmVideoReassembler {
    /// Create a fresh reassembler with empty assembly state.
    pub fn new() -> Self {
        Self {
            streams: BTreeMap::new(),
        }
    }

    /// Clear all pending assemblies (for example after a remote camera toggle).
    pub fn reset(&mut self) {
        self.streams.clear();
    }

    /// Pop the next in-order completed frame that is no longer blocked by an older active assembly.
    ///
    /// Callers should drain this after [`Self::ingest`] so newer frames buffered during a paced
    /// keyframe or FEC-protected P-frame grace window are delivered immediately once that older
    /// frame completes or expires.
    pub fn pop_ready(&mut self) -> Option<ReassembledVideoFrame> {
        for stream in self.streams.values_mut() {
            if let Some(frame) = stream.pop_next_ready() {
                return Some(frame);
            }
        }
        None
    }

    /// Feed one decrypted MP/iLink RSM video packet (`[codecType, mid/sid, EncFecHeader, ...]`).
    pub fn ingest(&mut self, packet: &[u8]) -> Result<RsmIngestOutcome> {
        let header = parse_rsm_packet_header(packet)?;
        self.ingest_with_header(packet, &header)
    }

    /// Feed one decrypted MP/iLink RSM video packet together with its pre-parsed
    /// [`RsmPacketHeader`], avoiding redundant `EncFecHeader` and TLV decoding.
    pub fn ingest_with_header(
        &mut self,
        packet: &[u8],
        header: &RsmPacketHeader,
    ) -> Result<RsmIngestOutcome> {
        ensure!(
            header.payload_offset <= packet.len(),
            "RSM packet shorter than parsed header payload offset"
        );
        let stream_id = header.stream_id & 0x0f;
        if !self.streams.contains_key(&stream_id) && self.streams.len() >= 4 {
            return Ok(RsmIngestOutcome::Pending);
        }
        self.streams
            .entry(stream_id)
            .or_default()
            .ingest_stream(packet, header, stream_id)
    }
}

impl StreamReassembler {
    fn record_drop(&mut self, frame_idx: u32) {
        if !self.pending_drops.contains(&frame_idx) {
            self.pending_drops.push(frame_idx);
            if self.pending_drops.len() > MAX_ACTIVE_ASSEMBLIES * 2 {
                let mut oldest_pos = 0usize;
                let mut oldest_val = self.pending_drops[0];
                for (pos, &val) in self.pending_drops.iter().enumerate().skip(1) {
                    if is_frame_older(val, oldest_val) {
                        oldest_val = val;
                        oldest_pos = pos;
                    }
                }
                self.pending_drops.remove(oldest_pos);
            }
        }
    }

    fn pop_next_ready(&mut self) -> Option<ReassembledVideoFrame> {
        let next_ready = oldest_frame_key(&self.ready)?;
        if self
            .active
            .keys()
            .any(|&active_idx| is_frame_older(active_idx, next_ready))
        {
            return None;
        }
        let mut frame = self.ready.remove(&next_ready)?;
        self.last_completed = Some(next_ready);
        self.last_timestamp_ms = Some(frame.timestamp_ms);
        let had_prior_drop = self
            .pending_drops
            .iter()
            .any(|&d| d == next_ready || is_frame_older(d, next_ready));
        if had_prior_drop {
            frame.prior_frames_dropped = true;
            self.pending_drops
                .retain(|&d| is_frame_older(next_ready, d));
        }
        Some(frame)
    }

    fn ingest_stream(
        &mut self,
        packet: &[u8],
        header: &RsmPacketHeader,
        stream_id: u8,
    ) -> Result<RsmIngestOutcome> {
        let codec = header.codec;
        let member_id = header.member_id;
        let is_iframe = header.is_iframe;
        let timestamp_ms = header.timestamp_ms;
        let frame_count = header.frame_count;
        let orientation = header.orientation & 0x07;
        let xps_len_hi = header.xps_len_hi;
        let slice_len_hi = header.slice_len_hi;
        let x_groups = usize::from(header.x_groups);
        let group_idx_x = usize::from(header.group_idx_x);
        let y_groups = usize::from(header.y_groups);
        let is_vert_parity = header.is_vert_parity;
        let vert_group_idx = usize::from(header.vert_group_idx);
        let s_per_group = usize::from(header.s_per_group);
        let r_full = usize::from(header.r_per_group);
        let pkg_idx_in_group = usize::from(header.pkg_idx_in_group);
        let payload = &packet[header.payload_offset..];

        // Virtual-frame metadata packets (`s = 1, r = 0, x = 1, pkg_idx = 3`) carry the
        // `workMode = 3` protobuf header (including `field 3 = orientation`) after a
        // 128-byte leading block. Record orientation before skipping non-media packets.
        if header.is_virtual_frame {
            if let Some(vf_orient) = header.virtual_frame_orientation {
                self.vf_orientation = Some(vf_orient & 0x07);
            }
            return Ok(self
                .pop_next_ready()
                .map_or(RsmIngestOutcome::Pending, RsmIngestOutcome::Frame));
        }

        // Skip degenerate or padding-only headers and reject out-of-range 3-bit group counts
        // (`x_groups in 1..=7`, `y_groups in 0..=7`). Do not drop packets solely because
        // `hdr[13] & 0x10` is set: peers mark real IDR/media packets with that
        // bandwidth-estimation probe bit while carrying valid frame data.
        if header.frame_total_pkts == 0
            || header.frame_pkt_idx == 0
            || s_per_group == 0
            || x_groups == 0
            || x_groups > 7
            || y_groups > 7
        {
            return Ok(RsmIngestOutcome::Pending);
        }
        if s_per_group > MAX_FRAGMENTS_PER_FRAME
            || (y_groups > 0 && x_groups.saturating_mul(s_per_group) > MAX_FRAGMENTS_PER_FRAME)
            || r_full > MAX_FEC_PARITY
            || pkg_idx_in_group == 0
        {
            return Ok(RsmIngestOutcome::Pending);
        }
        if is_vert_parity {
            if y_groups == 0 || vert_group_idx >= y_groups || pkg_idx_in_group > s_per_group {
                return Ok(RsmIngestOutcome::Pending);
            }
        } else if group_idx_x >= x_groups || pkg_idx_in_group > s_per_group + r_full {
            return Ok(RsmIngestOutcome::Pending);
        }

        let full_frame_idx = header.frame_index & 0xffff;
        // Drop trailing FEC parity / late duplicate packets belonging to any already-emitted
        // frame (within a 256-frame modular window) so they never re-emit an older frame
        // out of order via RS recovery or re-insert a zombie assembly that triggers false
        // frame-drop IRequests. Allow clean encoder restarts (IDR whose timestamp advanced
        // or whose index & timestamp reset to 0) and clear pre-restart assemblies so stale
        // chunks can never splice into post-restart frames.
        if let Some(last) = self.last_completed {
            let ts_advanced = self.last_timestamp_ms.is_some_and(|prev_ts| {
                let delta = timestamp_ms.wrapping_sub(prev_ts);
                delta > 0 && delta < 0x8000_0000
            });
            let low_idx_reset = full_frame_idx <= 4
                && last > 32
                && self.last_timestamp_ms.is_none_or(|prev_ts| {
                    timestamp_ms < 1000 && prev_ts.saturating_sub(timestamp_ms) > 5000
                });
            let encoder_reset = is_iframe && (ts_advanced || low_idx_reset);
            let lag = last.wrapping_sub(full_frame_idx) & 0xffff;
            if lag < 256 {
                if encoder_reset {
                    self.active.clear();
                    self.ready.clear();
                    self.last_completed = None;
                    self.pending_drops.clear();
                } else {
                    return Ok(self
                        .pop_next_ready()
                        .map_or(RsmIngestOutcome::Pending, RsmIngestOutcome::Frame));
                }
            }
        }
        if let Some(existing_ready) = self.ready.get(&full_frame_idx) {
            if existing_ready.timestamp_ms == timestamp_ms {
                return Ok(self
                    .pop_next_ready()
                    .map_or(RsmIngestOutcome::Pending, RsmIngestOutcome::Frame));
            }
            let newer = timestamp_ms.wrapping_sub(existing_ready.timestamp_ms) < 0x8000_0000;
            let low_idx_reset = full_frame_idx <= 4
                && timestamp_ms < 1000
                && existing_ready.timestamp_ms.saturating_sub(timestamp_ms) > 5000;
            if is_iframe && (newer || low_idx_reset) {
                self.active.clear();
                self.ready.clear();
                self.last_completed = None;
                self.pending_drops.clear();
            } else {
                return Ok(self
                    .pop_next_ready()
                    .map_or(RsmIngestOutcome::Pending, RsmIngestOutcome::Frame));
            }
        }

        if !self.active.contains_key(&full_frame_idx) {
            let packet_grace = grace_frames_for(is_iframe, r_full, y_groups);
            let ready_low_idx_reset = is_iframe
                && full_frame_idx <= 4
                && timestamp_ms < 1000
                && self.ready.iter().any(|(&ridx, rf)| {
                    ridx > 32 && rf.timestamp_ms.saturating_sub(timestamp_ms) > 5000
                });
            if ready_low_idx_reset {
                self.active.clear();
                self.ready.clear();
                self.last_completed = None;
                self.pending_drops.clear();
            } else if self.pending_drops.contains(&full_frame_idx) {
                return Ok(self
                    .pop_next_ready()
                    .map_or(RsmIngestOutcome::Pending, RsmIngestOutcome::Frame));
            } else if self.ready.keys().any(|&ridx| {
                is_frame_older(full_frame_idx, ridx)
                    && frame_distance(full_frame_idx, ridx) > packet_grace
            }) {
                self.record_drop(full_frame_idx);
                return Ok(self
                    .pop_next_ready()
                    .map_or(RsmIngestOutcome::Pending, RsmIngestOutcome::Frame));
            }
        }

        let mut dropped = false;
        if !self.active.contains_key(&full_frame_idx) && self.active.len() >= MAX_ACTIVE_ASSEMBLIES
        {
            if let Some(oldest_key) = self
                .active
                .iter()
                .min_by_key(|(_, a)| a.arrival_order)
                .map(|(&k, _)| k)
            {
                self.active.remove(&oldest_key);
                dropped = true;
                self.record_drop(oldest_key);
            }
        }

        self.arrival_counter = self.arrival_counter.wrapping_add(1);
        let arrival = self.arrival_counter;
        if let Some(existing) = self.active.get(&full_frame_idx) {
            if existing.timestamp_ms != timestamp_ms {
                let newer = timestamp_ms.wrapping_sub(existing.timestamp_ms) < 0x8000_0000;
                if newer {
                    self.active.remove(&full_frame_idx);
                    self.record_drop(full_frame_idx);
                } else {
                    return Ok(RsmIngestOutcome::Pending);
                }
            }
        }
        let entry = self
            .active
            .entry(full_frame_idx)
            .or_insert_with(|| PendingAssembly {
                codec,
                member_id,
                stream_id,
                timestamp_ms,
                orientation,
                is_iframe,
                frame_count,
                x_groups,
                y_groups,
                xps_len_hi,
                slice_len_hi,
                group_s: vec![None; x_groups],
                group_r: vec![None; x_groups],
                vert_s_per_group: (y_groups > 0 || is_vert_parity).then_some(s_per_group),
                groups: (0..x_groups).map(|_| BTreeMap::new()).collect(),
                vert_parity: BTreeMap::new(),
                fec_recovered: false,
                arrival_order: arrival,
            });
        // Reject inconsistent stream/group geometry for the same frame index while
        // permitting per-group `s_per_group` / `r_per_group` in 1D multi-group frames
        // (where the trailing group carries the remainder of source chunks).
        if entry.stream_id != stream_id || entry.x_groups != x_groups {
            return Ok(RsmIngestOutcome::Pending);
        }
        if y_groups > 0 || is_vert_parity {
            if entry.vert_s_per_group.is_some_and(|vs| vs != s_per_group)
                || entry.group_s.iter().flatten().any(|&gs| gs != s_per_group)
            {
                return Ok(RsmIngestOutcome::Pending);
            }
            entry.vert_s_per_group = Some(s_per_group);
        }
        if !is_vert_parity {
            if entry.vert_s_per_group.is_some_and(|vs| vs != s_per_group)
                || entry.group_s[group_idx_x].is_some_and(|gs| gs != s_per_group)
                || entry.group_r[group_idx_x].is_some_and(|gr| gr != r_full)
            {
                return Ok(RsmIngestOutcome::Pending);
            }
            let other_chunks: usize = entry
                .group_s
                .iter()
                .enumerate()
                .filter(|&(g, _)| g != group_idx_x)
                .filter_map(|(_, &gs)| gs)
                .sum();
            if other_chunks.saturating_add(s_per_group) > MAX_FRAGMENTS_PER_FRAME {
                return Ok(RsmIngestOutcome::Pending);
            }
            entry.group_s[group_idx_x] = Some(s_per_group);
            entry.group_r[group_idx_x] = Some(r_full);
        }
        entry.is_iframe |= is_iframe;
        entry.y_groups = entry.y_groups.max(y_groups);
        entry.frame_count = entry.frame_count.max(frame_count);
        if xps_len_hi != 0 {
            entry.xps_len_hi = xps_len_hi;
        }
        if slice_len_hi != 0 {
            entry.slice_len_hi = slice_len_hi;
        }
        if is_vert_parity {
            entry
                .vert_parity
                .entry((vert_group_idx, pkg_idx_in_group - 1))
                .or_insert_with(|| payload.to_vec());
        } else {
            entry.groups[group_idx_x]
                .entry(pkg_idx_in_group - 1)
                .or_insert_with(|| payload.to_vec());
        }

        if !entry.source_chunks_complete() && entry.try_fec_recover() {
            entry.fec_recovered = true;
        }
        if !entry.source_chunks_complete() {
            if let Some(ready_frame) = self.pop_next_ready() {
                return Ok(RsmIngestOutcome::Frame(ready_frame));
            }
            return Ok(if dropped {
                RsmIngestOutcome::FrameDroppedNeedKeyframe
            } else {
                RsmIngestOutcome::Pending
            });
        }

        let completed = self.active.remove(&full_frame_idx).expect("just inserted");

        let mut total_chunks = 0usize;
        let mut total_bytes = 0usize;
        for g in 0..completed.x_groups {
            let s_g = completed.effective_group_s(g).unwrap_or(0);
            total_chunks = total_chunks.saturating_add(s_g);
            for col in 0..s_g {
                if let Some(chunk) = completed.groups[g].get(&col) {
                    total_bytes = total_bytes.saturating_add(chunk.len());
                }
            }
        }
        if total_chunks > MAX_FRAGMENTS_PER_FRAME || total_bytes > MAX_VIDEO_FRAME_BYTES + 4096 {
            self.record_drop(full_frame_idx);
            bail!("Reassembled video frame exceeds budget");
        }
        let mut assembled = Vec::with_capacity(total_bytes);
        for g in 0..completed.x_groups {
            let s_g = completed.effective_group_s(g).unwrap_or(0);
            for col in 0..s_g {
                if let Some(chunk) = completed.groups[g].get(&col) {
                    assembled.extend_from_slice(chunk);
                }
            }
        }

        let (frame_payload, is_keyframe, has_codec_config) = match completed.codec {
            VideoWireCodec::H264 | VideoWireCodec::H265 => {
                match unframe_annexb_access_unit_with_count(
                    completed.codec,
                    &assembled,
                    completed.frame_count,
                    completed.xps_len_hi,
                    completed.slice_len_hi,
                ) {
                    Ok(unframed) => unframed,
                    Err(err) => {
                        self.record_drop(full_frame_idx);
                        return Err(err);
                    }
                }
            }
            _ => (assembled, completed.is_iframe, false),
        };
        let is_keyframe = is_keyframe || completed.is_iframe;

        // Evict strictly older stalled assemblies once they exceed their reordering/FEC
        // grace window (or immediately when a self-contained keyframe completes).
        let mut evicted = Vec::new();
        if is_keyframe {
            self.active.retain(|&idx, _| {
                if is_frame_older(idx, full_frame_idx) {
                    evicted.push(idx);
                    false
                } else {
                    true
                }
            });
            self.ready
                .retain(|&idx, _| !is_frame_older(idx, full_frame_idx));
        } else {
            self.active.retain(|&idx, asm| {
                if !is_frame_older(idx, full_frame_idx) {
                    return true;
                }
                if frame_distance(idx, full_frame_idx) <= asm.grace_frames() {
                    true
                } else {
                    evicted.push(idx);
                    false
                }
            });
        }
        for idx in evicted {
            self.record_drop(idx);
        }

        self.ready.insert(
            full_frame_idx,
            ReassembledVideoFrame {
                codec: completed.codec,
                member_id: completed.member_id,
                stream_id: completed.stream_id,
                frame_index: full_frame_idx,
                timestamp_ms: completed.timestamp_ms,
                orientation: self.vf_orientation.unwrap_or(completed.orientation),
                prior_frames_dropped: false,
                fec_recovered: completed.fec_recovered,
                is_keyframe,
                has_codec_config,
                payload: frame_payload,
            },
        );
        if self.ready.len() > MAX_ACTIVE_ASSEMBLIES {
            if let Some(oldest_ready) = oldest_frame_key(&self.ready) {
                let mut overflow_evicted = Vec::new();
                self.active.retain(|&idx, _| {
                    if is_frame_older(idx, oldest_ready) {
                        overflow_evicted.push(idx);
                        false
                    } else {
                        true
                    }
                });
                for idx in overflow_evicted {
                    self.record_drop(idx);
                }
            }
        }

        if let Some(ready_frame) = self.pop_next_ready() {
            Ok(RsmIngestOutcome::Frame(ready_frame))
        } else if dropped {
            Ok(RsmIngestOutcome::FrameDroppedNeedKeyframe)
        } else {
            Ok(RsmIngestOutcome::Pending)
        }
    }
}

/// GF(2^8) tables for primitive polynomial `x^8 + x^4 + x^3 + x^2 + 1` (`0x11d`).
struct Gf256Tables {
    exp: [u8; 512],
    log: [u8; 256],
}

fn gf256_tables() -> &'static Gf256Tables {
    static TABLES: std::sync::OnceLock<Gf256Tables> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for i in 0..255usize {
            exp[i] = x as u8;
            exp[i + 255] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11d;
            }
        }
        exp[510] = exp[0];
        exp[511] = exp[1];
        Gf256Tables { exp, log }
    })
}

#[inline]
fn gf_mul(t: &Gf256Tables, a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        t.exp[usize::from(t.log[usize::from(a)]) + usize::from(t.log[usize::from(b)])]
    }
}

#[inline]
fn gf_inv(t: &Gf256Tables, a: u8) -> u8 {
    debug_assert_ne!(a, 0);
    t.exp[255 - usize::from(t.log[usize::from(a)])]
}

/// Systematic Reed-Solomon erasure recovery matching official `CRSCodecInterface`:
/// - `r == 1`: single-parity XOR (`p_0 = d_0 ^ ... ^ d_{s-1}`).
/// - `r >= 2`: systematic RS over `GF(2^8, 0x11d)` with generator `prod_{i=1..=r} (x + alpha^i)`,
///   where codeword `C = [d_0..d_{s-1}, p_0..p_{r-1}]` satisfies `C(alpha^i) = 0` for `i = 1..=r`.
fn rs_recover_group(s: usize, r: usize, packets: &mut BTreeMap<usize, Vec<u8>>) -> bool {
    let n = s + r;
    if s == 0 || r == 0 || n > 255 {
        return false;
    }
    let missing_src: Vec<usize> = (0..s).filter(|idx| !packets.contains_key(idx)).collect();
    if missing_src.is_empty() {
        return false;
    }
    if packets.len() < s {
        return false;
    }
    let chunk_len = packets.values().map(Vec::len).max().unwrap_or(0);
    if chunk_len == 0 {
        return false;
    }

    // `r == 1` fast path: parity packet is exact bytewise XOR of all `s` source packets.
    if r == 1 {
        if missing_src.len() != 1 || !packets.contains_key(&s) {
            return false;
        }
        let mut recovered = vec![0u8; chunk_len];
        for (idx, pkt) in packets.iter() {
            if *idx <= s {
                for (dst, &src) in recovered.iter_mut().zip(pkt.iter()) {
                    *dst ^= src;
                }
            }
        }
        packets.insert(missing_src[0], recovered);
        return true;
    }

    // `r >= 2`: gather all erased positions in `0..n` (both missing source and missing parity)
    // and solve the `e × e` Vandermonde syndrome system over GF(2^8).
    let erasures: Vec<usize> = (0..n).filter(|idx| !packets.contains_key(idx)).collect();
    let e = erasures.len();
    if e == 0 || e > r {
        return false;
    }
    let t = gf256_tables();

    // Build augmented `e × 2e` matrix `[M | I]` where `M[row][col] = alpha^{(row + 1) * (n - 1 - erasures[col])}`.
    let mut aug = vec![vec![0u8; 2 * e]; e];
    for (row, aug_row) in aug.iter_mut().enumerate() {
        let root_exp = row + 1; // alpha^1 .. alpha^e
        for (col, &pos) in erasures.iter().enumerate() {
            let deg = n - 1 - pos;
            aug_row[col] = t.exp[(root_exp * deg) % 255];
        }
        aug_row[e + row] = 1;
    }

    // Gauss-Jordan elimination in GF(2^8).
    for pivot in 0..e {
        let Some(swap_row) = (pivot..e).find(|&row| aug[row][pivot] != 0) else {
            return false;
        };
        if swap_row != pivot {
            aug.swap(pivot, swap_row);
        }
        let inv_p = gf_inv(t, aug[pivot][pivot]);
        for cell in &mut aug[pivot][pivot..2 * e] {
            *cell = gf_mul(t, *cell, inv_p);
        }
        let pivot_tail: Vec<u8> = aug[pivot][pivot..2 * e].to_vec();
        for (row, aug_row) in aug.iter_mut().enumerate() {
            if row == pivot {
                continue;
            }
            let factor = aug_row[pivot];
            if factor == 0 {
                continue;
            }
            for (cell, &pval) in aug_row[pivot..2 * e].iter_mut().zip(pivot_tail.iter()) {
                *cell ^= gf_mul(t, factor, pval);
            }
        }
    }

    // Precompute `alpha^{(row + 1) * (n - 1 - m)}` for every present packet `m`.
    let present: Vec<(usize, Vec<u8>)> = (0..n)
        .filter_map(|m| packets.get(&m).map(|pkt| (m, pkt.clone())))
        .collect();
    let present_evals: Vec<Vec<u8>> = (0..e)
        .map(|row| {
            let root_exp = row + 1;
            present
                .iter()
                .map(|(m, _)| t.exp[(root_exp * (n - 1 - m)) % 255])
                .collect()
        })
        .collect();

    let mut recovered_bufs = vec![vec![0u8; chunk_len]; e];
    let mut syndromes = vec![0u8; e];
    for byte_idx in 0..chunk_len {
        for (row, syn) in syndromes.iter_mut().enumerate() {
            let mut acc = 0u8;
            for ((_, pkt), &coeff) in present.iter().zip(present_evals[row].iter()) {
                let val = pkt.get(byte_idx).copied().unwrap_or(0);
                if val != 0 {
                    acc ^= gf_mul(t, val, coeff);
                }
            }
            *syn = acc;
        }
        for (out_buf, inv_row) in recovered_bufs.iter_mut().zip(aug.iter()) {
            let mut val = 0u8;
            for (&inv_coeff, &syn) in inv_row[e..2 * e].iter().zip(syndromes.iter()) {
                if syn != 0 {
                    val ^= gf_mul(t, inv_coeff, syn);
                }
            }
            out_buf[byte_idx] = val;
        }
    }

    let mut restored_src = false;
    for (pos, buf) in erasures.into_iter().zip(recovered_bufs) {
        if pos < s {
            packets.insert(pos, buf);
            restored_src = true;
        }
    }
    restored_src
}

fn opt_u64(fields: &[Field], number: u32) -> Result<Option<u64>> {
    match p::optional(fields, number)? {
        None => Ok(None),
        Some(Value::Varint(v)) => Ok(Some(*v)),
        _ => bail!("Invalid video QoS scalar field {number}"),
    }
}

fn opt_u32(fields: &[Field], number: u32) -> Result<Option<u32>> {
    opt_u64(fields, number)?
        .map(|v| u32::try_from(v).map_err(Into::into))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irequest_roundtrips_58_byte_wire_layout() {
        let raw = encode_irequest(3, 5, 1, 123_456_789, 7);
        assert_eq!(raw.len(), 58);
        assert_eq!(raw[0], 0x66);
        assert_eq!(raw[6], 0x02);
        assert_eq!(raw[6 + 0x0c], 0x03);
        let classified = classify_video_channel_payload(&raw).unwrap();
        let VideoChannelPayload::IRequest(req) = classified else {
            panic!("expected IRequest");
        };
        assert_eq!(req.local_member, 3);
        assert_eq!(req.local_stream_id, 1);
        assert_eq!(req.target_member, 5);
        assert_eq!(req.target_stream_id, 1);
        assert_eq!(req.timestamp_ms, 123_456_789);
        assert_eq!(u32::from_le_bytes(raw[50..54].try_into().unwrap()), 0);

        let unforced = encode_irequest_with_force(1, 0, 1, 999, 44, false);
        assert_eq!(u64::from_le_bytes(unforced[42..50].try_into().unwrap()), 44);
        assert_eq!(u32::from_le_bytes(unforced[50..54].try_into().unwrap()), 0);
        let forced = encode_irequest_with_force(1, 0, 1, 999, 44, true);
        assert_eq!(u32::from_le_bytes(forced[50..54].try_into().unwrap()), 1);
    }

    #[test]
    fn s2p_and_p2s_roundtrip_expected_fields() {
        let video_s2p = p::message(&[
            p::uint(1, 120),
            p::uint(2, 600),
            p::uint(3, 24),
            p::uint(4, 8),
            p::uint(9, 1),
            p::uint(10, 1),
            p::uint(11, 4),
            p::uint(12, 1),
        ])
        .unwrap();
        let inner = p::message(&[p::bytes(3, &video_s2p)]).unwrap();
        let outer = p::message(&[
            p::uint(1, 999),
            p::uint(2, 888),
            p::uint(3, 1),
            p::bytes(4, &inner),
            p::uint(5, 2),
        ])
        .unwrap();
        let mut pkt = vec![0x0c];
        pkt.extend_from_slice(&outer);
        let parsed = parse_s2p(&pkt).unwrap();
        assert_eq!(parsed.room_id, 999);
        assert_eq!(parsed.video_commands.len(), 1);
        assert_eq!(parsed.video_commands[0].kbps, 600);
        assert_eq!(parsed.video_commands[0].codec_type, 4);
        assert_eq!(parsed.video_commands[0].i_request_flag, 1);

        let p2s = encode_p2s(5, 2, 3, 0x1d, 15).unwrap();
        assert_eq!(p2s[0], 0x0b);
        let decoded = p::decode(&p2s[1..]).unwrap();
        assert_eq!(opt_u32(&decoded, 1).unwrap(), Some(5));
        assert_eq!(opt_u32(&decoded, 2).unwrap(), Some(2));
        // Video P2S leaves `p2s_type` unset; `2` would mark an audio P2S.
        assert_eq!(opt_u32(&decoded, 4).unwrap(), None);
        let inner = p::decode(p::optional_data(&decoded, 3).unwrap().unwrap()).unwrap();
        let head = p::decode(p::optional_data(&inner, 1).unwrap().unwrap()).unwrap();
        assert_eq!(opt_u32(&head, 1).unwrap(), Some(11));
        assert_eq!(opt_u32(&head, 2).unwrap(), Some(0));
        assert_eq!(opt_u32(&head, 3).unwrap(), Some(5));
        assert_eq!(opt_u32(&head, 4).unwrap(), Some(3));
        assert!(p::optional_data(&inner, 2).unwrap().is_none());
        let wrapper = p::decode(p::optional_data(&inner, 3).unwrap().unwrap()).unwrap();
        let video_p2s = p::decode(p::optional_data(&wrapper, 1).unwrap().unwrap()).unwrap();
        assert_eq!(opt_u32(&video_p2s, 24).unwrap(), Some(0x1c));
        assert_eq!(opt_u32(&video_p2s, 29).unwrap(), Some(4));
    }

    #[test]
    fn hevc_annexb_packetizes_and_reassembles_out_of_order_and_skips_probes() {
        let mut hevc_idr = Vec::new();
        hevc_idr.extend_from_slice(&[0, 0, 0, 1, 0x40, 0x01, 0xaa, 0xbb]); // VPS (32)
        hevc_idr.extend_from_slice(&[0, 0, 0, 1, 0x42, 0x01, 0xcc, 0xdd]); // SPS (33)
        hevc_idr.extend_from_slice(&[0, 0, 0, 1, 0x44, 0x01, 0xee]); // PPS (34)
        hevc_idr.extend_from_slice(&[0, 0, 0, 1, 0x26, 0x01]); // IDR_W_RADL (19)
        hevc_idr.extend_from_slice(&vec![0x5a; 300]);

        let mut packetizer = RsmVideoPacketizer::new(2, 1).with_chunk_bytes(100).unwrap();
        let pkts = packetizer
            .packetize(VideoWireCodec::H265, &hevc_idr, 42_000, 15, 500)
            .unwrap();
        assert_eq!(pkts.len(), 4);
        assert_eq!(pkts[0][0], 15);
        assert_eq!(u16::from_le_bytes([pkts[0][1], pkts[0][2]]), 2 | (1 << 11));

        let mut reassembler = RsmVideoReassembler::new();
        // Feed packets out of order.
        assert_eq!(
            reassembler.ingest(&pkts[2]).unwrap(),
            RsmIngestOutcome::Pending
        );
        assert_eq!(
            reassembler.ingest(&pkts[0]).unwrap(),
            RsmIngestOutcome::Pending
        );
        assert_eq!(
            reassembler.ingest(&pkts[3]).unwrap(),
            RsmIngestOutcome::Pending
        );
        let RsmIngestOutcome::Frame(frame) = reassembler.ingest(&pkts[1]).unwrap() else {
            panic!("expected reassembled frame");
        };
        assert_eq!(frame.codec, VideoWireCodec::H265);
        assert_eq!(frame.member_id, 2);
        assert_eq!(frame.stream_id, 1);
        assert_eq!(frame.timestamp_ms, 42_000);
        assert_eq!(frame.orientation, DEFAULT_RSM_ORIENTATION);
        assert!(!frame.prior_frames_dropped);
        assert!(frame.is_keyframe);
        assert!(frame.has_codec_config);
        assert_eq!(frame.payload, hevc_idr);

        // Single P-frame roundtrip.
        let mut p_frame = vec![0, 0, 0, 1, 0x02, 0x01]; // TRAIL_R (1)
        p_frame.extend_from_slice(&[0x33; 40]);
        let p_pkts = packetizer
            .packetize(VideoWireCodec::H265, &p_frame, 42_066, 15, 500)
            .unwrap();
        assert_eq!(p_pkts.len(), 1);
        let RsmIngestOutcome::Frame(p_out) = reassembler.ingest(&p_pkts[0]).unwrap() else {
            panic!("expected P-frame");
        };
        assert!(!p_out.is_keyframe);
        assert!(!p_out.has_codec_config);
        assert_eq!(p_out.orientation, DEFAULT_RSM_ORIENTATION);
        assert!(!p_out.prior_frames_dropped);
        assert_eq!(p_out.payload, p_frame);
    }

    #[test]
    fn orientation_roundtrips_and_maps_direct_and_conference_rotations() {
        assert_eq!(VideoRotation::from_wire(1, false), VideoRotation::Deg90);
        assert_eq!(VideoRotation::from_wire(2, false), VideoRotation::Deg0);
        assert_eq!(VideoRotation::from_wire(3, false), VideoRotation::Deg270);
        assert_eq!(VideoRotation::from_wire(4, false), VideoRotation::Deg180);
        assert_eq!(VideoRotation::from_wire(0, false), VideoRotation::Deg90);
        assert_eq!(VideoRotation::from_wire(0, true), VideoRotation::Deg0);
        assert_eq!(VideoRotation::from_wire(5, false), VideoRotation::Deg0);
        assert_eq!(VideoRotation::from_wire(5, true), VideoRotation::Deg90);

        let mut p_frame = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame.extend_from_slice(&[0x44; 32]);
        for orient in 0..=7u8 {
            let mut tx = RsmVideoPacketizer::new(1, 1).with_orientation(orient);
            let pkts = tx
                .packetize(VideoWireCodec::H265, &p_frame, 1000, 15, 300)
                .unwrap();
            let mut rx = RsmVideoReassembler::new();
            let RsmIngestOutcome::Frame(frame) = rx.ingest(&pkts[0]).unwrap() else {
                panic!("expected frame for orient {orient}");
            };
            assert_eq!(frame.orientation, orient);
        }
    }

    #[test]
    fn incomplete_prior_frame_flags_drop_when_newer_frame_completes() {
        let mut p_frame_big = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame_big.extend_from_slice(&[0x22; 220]);
        let mut p_frame_small = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame_small.extend_from_slice(&[0x33; 40]);

        let mut tx = RsmVideoPacketizer::new(2, 1).with_chunk_bytes(100).unwrap();
        let lost_frame_pkts = tx
            .packetize(VideoWireCodec::H265, &p_frame_big, 100, 15, 300)
            .unwrap();
        assert!(lost_frame_pkts.len() >= 2);
        let next_frame_pkts = tx
            .packetize(VideoWireCodec::H265, &p_frame_small, 166, 15, 300)
            .unwrap();
        assert_eq!(next_frame_pkts.len(), 1);

        let mut rx = RsmVideoReassembler::new();
        assert_eq!(
            rx.ingest(&lost_frame_pkts[0]).unwrap(),
            RsmIngestOutcome::Pending
        );
        let RsmIngestOutcome::Frame(next) = rx.ingest(&next_frame_pkts[0]).unwrap() else {
            panic!("expected next frame to complete");
        };
        assert!(next.prior_frames_dropped);
        assert!(!next.is_keyframe);
    }

    #[test]
    fn virtual_frame_orientation_overrides_workmode3_header_code() {
        let mut p_frame = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame.extend_from_slice(&[0x55; 32]);

        let mut tx = RsmVideoPacketizer::new(2, 1).with_orientation(5);
        let mut rx = RsmVideoReassembler::new();

        // Mobile portrait virtual frame announces orientation = 1 (90° CW) while
        // regular media packet headers carry fixed workMode=3 orientation code 5.
        let mut vf_portrait = RsmVideoPacketizer::new(2, 1).with_orientation(1);
        let vf_pkt = vf_portrait
            .build_orientation_metadata_packet(VideoWireCodec::H265, 100, 15)
            .unwrap();
        assert_eq!(rx.ingest(&vf_pkt).unwrap(), RsmIngestOutcome::Pending);

        let pkts = tx
            .packetize(VideoWireCodec::H265, &p_frame, 120, 15, 300)
            .unwrap();
        let RsmIngestOutcome::Frame(frame) = rx.ingest(&pkts[0]).unwrap() else {
            panic!("expected portrait frame");
        };
        assert_eq!(frame.orientation, 1);
        assert_eq!(
            VideoRotation::from_wire(frame.orientation, false),
            VideoRotation::Deg90
        );

        // Rotating to landscape sends a virtual frame with orientation = 2 (0° upright).
        let mut vf_landscape = RsmVideoPacketizer::new(2, 1).with_orientation(2);
        let vf_pkt2 = vf_landscape
            .build_orientation_metadata_packet(VideoWireCodec::H265, 200, 15)
            .unwrap();
        assert_eq!(rx.ingest(&vf_pkt2).unwrap(), RsmIngestOutcome::Pending);

        let pkts2 = tx
            .packetize(VideoWireCodec::H265, &p_frame, 220, 15, 300)
            .unwrap();
        let RsmIngestOutcome::Frame(frame2) = rx.ingest(&pkts2[0]).unwrap() else {
            panic!("expected landscape frame");
        };
        assert_eq!(frame2.orientation, 2);
        assert_eq!(
            VideoRotation::from_wire(frame2.orientation, false),
            VideoRotation::Deg0
        );
    }

    #[test]
    fn virtual_frame_header_and_protobuf_match_workmode3_layout() {
        let mut p_frame = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame.extend_from_slice(&[0x44; 24]);

        let mut tx = RsmVideoPacketizer::new(7, 1).with_orientation(2);
        let frame1 = tx
            .packetize(VideoWireCodec::H265, &p_frame, 1000, 15, 350)
            .unwrap();
        assert_eq!(u16::from_be_bytes([frame1[0][7], frame1[0][8]]), 1);

        let vf = tx
            .build_orientation_metadata_packet(VideoWireCodec::H265, 1000, 15)
            .unwrap();
        let hdr = &vf[3..];
        assert_eq!(hdr[0] & 0x03, 3);
        assert_eq!(hdr[0] & 0x04, 0);
        assert_eq!(hdr[0] >> 3, (1 ^ 0x10) as u8);
        assert_eq!(hdr[1], 3 << 2);
        assert_eq!(u16::from_be_bytes([hdr[2], hdr[3]]), 2); // total_pkg_seq advanced
        assert_eq!(u16::from_be_bytes([hdr[4], hdr[5]]), 0); // pkg_seq is 0 on virtual frames
        assert_eq!(hdr[12], (1 << 2) | 3);
        assert_eq!(hdr[13], 5 << 5);
        assert_eq!(hdr[15], 0);
        assert_eq!(hdr[20], 5);
        assert_eq!(&hdr[21..25], &[0x00, 0x4c, 0x00, 0x08]);

        let proto = p::decode(&hdr[26 + 0x80..]).unwrap();
        assert_eq!(opt_u32(&proto, 1).unwrap(), Some(15));
        assert_eq!(opt_u32(&proto, 2).unwrap(), Some(0));
        assert_eq!(opt_u32(&proto, 3).unwrap(), Some(2));
        assert_eq!(opt_u32(&proto, 4).unwrap(), Some(1));
        for field in [5, 6, 7, 8, 9, 16] {
            assert_eq!(opt_u32(&proto, field).unwrap(), Some(0));
        }

        // Media pkg_seq stays contiguous across an intervening virtual frame.
        let frame2 = tx
            .packetize(VideoWireCodec::H265, &p_frame, 1066, 15, 350)
            .unwrap();
        assert_eq!(u16::from_be_bytes([frame2[0][5], frame2[0][6]]), 3); // total_pkg_seq
        assert_eq!(u16::from_be_bytes([frame2[0][7], frame2[0][8]]), 2); // pkg_seq
    }

    #[test]
    fn high_frame_indices_roundtrip_with_11bit_le_extension() {
        let mut p_frame = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame.extend_from_slice(&[0x11; 24]);
        let mut tx = RsmVideoPacketizer::new(1, 1);
        let mut rx = RsmVideoReassembler::new();
        for expected_idx in 1..=40u32 {
            let pkts = tx
                .packetize(VideoWireCodec::H265, &p_frame, expected_idx * 66, 15, 300)
                .unwrap();
            let RsmIngestOutcome::Frame(frame) = rx.ingest(&pkts[0]).unwrap() else {
                panic!("expected frame {expected_idx}");
            };
            assert_eq!(frame.frame_index, expected_idx);
        }
    }

    /// Reference systematic RS parity generator matching official `CRSCodecInterface`:
    /// `r == 1` produces bytewise XOR; `r >= 2` uses LFSR with `g(x) = prod_{i=1..=r}(x + alpha^i)`
    /// over GF(2^8, 0x11d) emitting `[reg[r-1], ..., reg[0]]`.
    fn test_rs_encode(sources: &[Vec<u8>], r: usize) -> Vec<Vec<u8>> {
        let len = sources[0].len();
        if r == 1 {
            let mut p = vec![0u8; len];
            for src in sources {
                for (d, &b) in p.iter_mut().zip(src.iter()) {
                    *d ^= b;
                }
            }
            return vec![p];
        }
        let t = gf256_tables();
        let mut gen = vec![0u8; r + 1];
        gen[0] = 1;
        for i in 1..=r {
            let root = t.exp[i];
            for j in (1..=i).rev() {
                gen[j] = gen[j - 1] ^ gf_mul(t, gen[j], root);
            }
            gen[0] = gf_mul(t, gen[0], root);
        }
        let mut parity = vec![vec![0u8; len]; r];
        for b in 0..len {
            let mut reg = vec![0u8; r];
            for src in sources {
                let fb = src[b] ^ reg[r - 1];
                for j in (1..r).rev() {
                    reg[j] = reg[j - 1] ^ gf_mul(t, fb, gen[j]);
                }
                reg[0] = gf_mul(t, fb, gen[0]);
            }
            for (k, out) in parity.iter_mut().enumerate() {
                out[b] = reg[r - 1 - k];
            }
        }
        parity
    }

    #[allow(clippy::too_many_arguments)]
    fn build_2d_rsm_pkt(
        codec: VideoWireCodec,
        frame_idx: u32,
        x_groups: usize,
        group_idx_x: usize,
        s: usize,
        r: usize,
        pkg_idx_1based: usize,
        f_pkt_idx: usize,
        f_tot_pkts: usize,
        y_groups: usize,
        is_vert_parity: bool,
        vert_group_idx: usize,
        payload: &[u8],
    ) -> Vec<u8> {
        let mp_ms: u16 = 2 | (1 << 11);
        let frame_idx_lo = (frame_idx & 0x1f) as u8;
        let frame_idx_hi = ((frame_idx >> 5) & 0x07ff) as u16;
        let r_lo2 = r.min(3) as u8;
        let r_mid2 = r.saturating_sub(3).min(3) as u8;
        let r_hi4 = r.saturating_sub(6).min(15) as u8;
        let mut ext_mask: u16 = 0x0044;
        if y_groups > 0 || is_vert_parity {
            ext_mask |= 0x0020;
        }
        let mut ext = Vec::new();
        ext.extend_from_slice(&ext_mask.to_be_bytes());
        ext.push((frame_idx_hi & 0xff) as u8);
        ext.push(((frame_idx_hi >> 8) & 0x07) as u8);
        if ext_mask & 0x0020 != 0 {
            ext.push(
                (y_groups as u8 & 0x07)
                    | (u8::from(is_vert_parity) << 3)
                    | ((vert_group_idx as u8 & 0x07) << 4),
            );
        }
        ext.push(1); // bw_est_seq

        let mut pkt = Vec::with_capacity(3 + 21 + ext.len() + payload.len());
        pkt.push(codec.wire());
        pkt.extend_from_slice(&mp_ms.to_le_bytes());
        pkt.push(3 | (frame_idx_lo << 3));
        pkt.push(r_lo2 | ((pkg_idx_1based as u8 & 0x3f) << 2));
        pkt.extend_from_slice(&(f_pkt_idx as u16).to_be_bytes());
        pkt.extend_from_slice(&(f_pkt_idx as u16).to_be_bytes());
        pkt.extend_from_slice(&5000u32.to_be_bytes());
        pkt.push(15);
        pkt.push(0);
        pkt.push((s as u8 & 0x3f) << 2);
        pkt.push((2 << 5) | r_mid2);
        pkt.push(r_hi4 | ((group_idx_x as u8 & 0x07) << 5));
        pkt.push((x_groups as u8 & 0x07) | (1 << 3) | (1 << 6));
        pkt.push(0);
        pkt.push(0);
        pkt.push(f_pkt_idx as u8);
        pkt.push(f_tot_pkts as u8);
        pkt.push(ext.len() as u8);
        pkt.extend_from_slice(&ext);
        pkt.extend_from_slice(payload);
        pkt
    }

    #[test]
    fn multi_group_rsm_with_horizontal_fec_reassembles_without_dropping_tail_groups() {
        let mut annexb = vec![0, 0, 0, 1, 0x02, 0x01];
        for i in 0..340u16 {
            annexb.push(((i * 37 + 11) & 0xff) as u8);
        }
        let framed = frame_annexb_access_unit(VideoWireCodec::H265, &annexb).unwrap();
        let x_groups = 2usize;
        let s = 3usize;
        let r = 1usize;
        let chunk_len = framed.bytes.len().div_ceil(x_groups * s);
        let mut padded = framed.bytes.clone();
        padded.resize(x_groups * s * chunk_len, 0);
        let total_pkts = x_groups * (s + r);

        let mut all_pkts = Vec::new();
        let mut f_idx = 0usize;
        for g in 0..x_groups {
            let group_srcs: Vec<Vec<u8>> = (0..s)
                .map(|col| {
                    let start = (g * s + col) * chunk_len;
                    padded[start..start + chunk_len].to_vec()
                })
                .collect();
            let parities = test_rs_encode(&group_srcs, r);
            for (col, src) in group_srcs.iter().enumerate() {
                f_idx += 1;
                all_pkts.push(build_2d_rsm_pkt(
                    VideoWireCodec::H265,
                    7,
                    x_groups,
                    g,
                    s,
                    r,
                    col + 1,
                    f_idx,
                    total_pkts,
                    0,
                    false,
                    0,
                    src,
                ));
            }
            for (p_i, par) in parities.iter().enumerate() {
                f_idx += 1;
                all_pkts.push(build_2d_rsm_pkt(
                    VideoWireCodec::H265,
                    7,
                    x_groups,
                    g,
                    s,
                    r,
                    s + 1 + p_i,
                    f_idx,
                    total_pkts,
                    0,
                    false,
                    0,
                    par,
                ));
            }
        }
        // Group 1's 3rd source packet has f_pkt_idx = 7 > x * s = 6; previously this was dropped.
        assert_eq!(all_pkts.len(), 8);

        // Case 1: All source packets arrive (including f_pkt_idx = 7).
        let mut rx = RsmVideoReassembler::new();
        let mut out = None;
        for pkt in [
            &all_pkts[0],
            &all_pkts[1],
            &all_pkts[2],
            &all_pkts[4],
            &all_pkts[5],
            &all_pkts[6],
        ] {
            if let RsmIngestOutcome::Frame(f) = rx.ingest(pkt).unwrap() {
                out = Some(f);
            }
        }
        let frame = out.expect("multi-group frame with f_pkt_idx > x*s must reassemble");
        assert!(!frame.fec_recovered);
        assert_eq!(frame.payload, annexb);

        // Case 2: Drop 1 source packet in each group and recover via r=1 XOR parity.
        let mut rx2 = RsmVideoReassembler::new();
        let mut out2 = None;
        for pkt in [
            &all_pkts[0],
            &all_pkts[2],
            &all_pkts[3],
            &all_pkts[4],
            &all_pkts[6],
            &all_pkts[7],
        ] {
            if let RsmIngestOutcome::Frame(f) = rx2.ingest(pkt).unwrap() {
                out2 = Some(f);
            }
        }
        let frame2 = out2.expect("r=1 horizontal XOR FEC must recover lost packet in each group");
        assert!(frame2.fec_recovered);
        assert_eq!(frame2.payload, annexb);
    }

    #[test]
    fn horizontal_gf256_rs_fec_recovers_multiple_erased_source_packets() {
        let mut annexb = vec![0, 0, 0, 1, 0x02, 0x01];
        for i in 0..420u16 {
            annexb.push(((i * 53 + 7) & 0xff) as u8);
        }
        let framed = frame_annexb_access_unit(VideoWireCodec::H265, &annexb).unwrap();
        for (s, r, drop_indices) in [
            (5usize, 2usize, vec![1usize, 4]),
            (6usize, 3usize, vec![0usize, 2, 5]),
        ] {
            let chunk_len = framed.bytes.len().div_ceil(s);
            let mut padded = framed.bytes.clone();
            padded.resize(s * chunk_len, 0);
            let srcs: Vec<Vec<u8>> = (0..s)
                .map(|i| padded[i * chunk_len..(i + 1) * chunk_len].to_vec())
                .collect();
            let parities = test_rs_encode(&srcs, r);
            let mut rx = RsmVideoReassembler::new();
            let mut out = None;
            for (i, src) in srcs.iter().enumerate() {
                if drop_indices.contains(&i) {
                    continue;
                }
                let pkt = build_2d_rsm_pkt(
                    VideoWireCodec::H265,
                    12,
                    1,
                    0,
                    s,
                    r,
                    i + 1,
                    i + 1,
                    s + r,
                    0,
                    false,
                    0,
                    src,
                );
                if let RsmIngestOutcome::Frame(f) = rx.ingest(&pkt).unwrap() {
                    out = Some(f);
                }
            }
            for (p_i, par) in parities.iter().enumerate() {
                let pkt = build_2d_rsm_pkt(
                    VideoWireCodec::H265,
                    12,
                    1,
                    0,
                    s,
                    r,
                    s + 1 + p_i,
                    s + 1 + p_i,
                    s + r,
                    0,
                    false,
                    0,
                    par,
                );
                if let RsmIngestOutcome::Frame(f) = rx.ingest(&pkt).unwrap() {
                    out = Some(f);
                }
            }
            let frame = out.expect("GF(256) RS FEC must recover all erased source packets");
            assert!(frame.fec_recovered);
            assert_eq!(frame.payload, annexb);
        }
    }

    #[test]
    fn vertical_2d_rs_fec_recovers_double_loss_in_one_horizontal_group() {
        let mut annexb = vec![0, 0, 0, 1, 0x02, 0x01];
        for i in 0..360u16 {
            annexb.push(((i * 29 + 3) & 0xff) as u8);
        }
        let framed = frame_annexb_access_unit(VideoWireCodec::H265, &annexb).unwrap();
        let x_groups = 2usize;
        let s = 3usize;
        let r = 1usize;
        let y_groups = 1usize;
        let chunk_len = framed.bytes.len().div_ceil(x_groups * s);
        let mut padded = framed.bytes.clone();
        padded.resize(x_groups * s * chunk_len, 0);
        let total_pkts = x_groups * (s + r) + y_groups * s;

        let srcs: Vec<Vec<Vec<u8>>> = (0..x_groups)
            .map(|g| {
                (0..s)
                    .map(|col| {
                        padded[(g * s + col) * chunk_len..(g * s + col + 1) * chunk_len].to_vec()
                    })
                    .collect()
            })
            .collect();
        let h_par: Vec<Vec<u8>> = (0..x_groups)
            .map(|g| test_rs_encode(&srcs[g], r).pop().unwrap())
            .collect();
        let v_par: Vec<Vec<u8>> = (0..s)
            .map(|col| {
                test_rs_encode(&[srcs[0][col].clone(), srcs[1][col].clone()], y_groups)
                    .pop()
                    .unwrap()
            })
            .collect();

        // Lose TWO source packets in group 0 (col 0 and col 2): horizontal r=1 alone cannot
        // recover 2 erasures, so vertical parity recovers (g=0, col=0) and then horizontal
        // parity finishes (g=0, col=2).
        let mut rx = RsmVideoReassembler::new();
        let mut out = None;
        let feed: Vec<Vec<u8>> = vec![
            build_2d_rsm_pkt(
                VideoWireCodec::H265,
                21,
                x_groups,
                0,
                s,
                r,
                2,
                2,
                total_pkts,
                y_groups,
                false,
                0,
                &srcs[0][1],
            ),
            build_2d_rsm_pkt(
                VideoWireCodec::H265,
                21,
                x_groups,
                0,
                s,
                r,
                4,
                4,
                total_pkts,
                y_groups,
                false,
                0,
                &h_par[0],
            ),
            build_2d_rsm_pkt(
                VideoWireCodec::H265,
                21,
                x_groups,
                1,
                s,
                r,
                1,
                5,
                total_pkts,
                y_groups,
                false,
                0,
                &srcs[1][0],
            ),
            build_2d_rsm_pkt(
                VideoWireCodec::H265,
                21,
                x_groups,
                1,
                s,
                r,
                2,
                6,
                total_pkts,
                y_groups,
                false,
                0,
                &srcs[1][1],
            ),
            build_2d_rsm_pkt(
                VideoWireCodec::H265,
                21,
                x_groups,
                1,
                s,
                r,
                3,
                7,
                total_pkts,
                y_groups,
                false,
                0,
                &srcs[1][2],
            ),
            build_2d_rsm_pkt(
                VideoWireCodec::H265,
                21,
                x_groups,
                0,
                s,
                r,
                1,
                9,
                total_pkts,
                y_groups,
                true,
                0,
                &v_par[0],
            ),
        ];
        for pkt in &feed {
            if let RsmIngestOutcome::Frame(f) = rx.ingest(pkt).unwrap() {
                out = Some(f);
            }
        }
        let frame =
            out.expect("2D vertical + horizontal RS FEC must recover double loss in group 0");
        assert!(frame.fec_recovered);
        assert_eq!(frame.payload, annexb);
    }

    #[test]
    fn late_fec_parity_after_newer_frame_does_not_reemit_or_create_zombie() {
        let mut p_frame_10 = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame_10.extend_from_slice(&[0x10; 40]);
        let mut p_frame_11 = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame_11.extend_from_slice(&[0x11; 40]);
        let mut p_frame_12 = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame_12.extend_from_slice(&[0x12; 120]);
        let mut p_frame_13 = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame_13.extend_from_slice(&[0x13; 40]);
        let mut p_frame_14 = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame_14.extend_from_slice(&[0x14; 40]);

        let f10 = frame_annexb_access_unit(VideoWireCodec::H265, &p_frame_10).unwrap();
        let f11 = frame_annexb_access_unit(VideoWireCodec::H265, &p_frame_11).unwrap();
        let f12 = frame_annexb_access_unit(VideoWireCodec::H265, &p_frame_12).unwrap();
        let f13 = frame_annexb_access_unit(VideoWireCodec::H265, &p_frame_13).unwrap();
        let f14 = frame_annexb_access_unit(VideoWireCodec::H265, &p_frame_14).unwrap();

        let mut rx = RsmVideoReassembler::new();

        // Case 1: s=1, r=1 — frame 10 and frame 11 complete from their source packets;
        // frame 10's trailing parity arrives after frame 11 and must NOT re-emit frame 10.
        let pkt10_src = build_2d_rsm_pkt(
            VideoWireCodec::H265,
            10,
            1,
            0,
            1,
            1,
            1,
            1,
            2,
            0,
            false,
            0,
            &f10.bytes,
        );
        let pkt10_par = build_2d_rsm_pkt(
            VideoWireCodec::H265,
            10,
            1,
            0,
            1,
            1,
            2,
            2,
            2,
            0,
            false,
            0,
            &f10.bytes,
        );
        let pkt11_src = build_2d_rsm_pkt(
            VideoWireCodec::H265,
            11,
            1,
            0,
            1,
            1,
            1,
            1,
            2,
            0,
            false,
            0,
            &f11.bytes,
        );

        let RsmIngestOutcome::Frame(out10) = rx.ingest(&pkt10_src).unwrap() else {
            panic!("frame 10 should complete");
        };
        assert_eq!(out10.frame_index, 10);
        assert!(!out10.prior_frames_dropped);

        let RsmIngestOutcome::Frame(out11) = rx.ingest(&pkt11_src).unwrap() else {
            panic!("frame 11 should complete");
        };
        assert_eq!(out11.frame_index, 11);
        assert!(!out11.prior_frames_dropped);

        assert_eq!(
            rx.ingest(&pkt10_par).unwrap(),
            RsmIngestOutcome::Pending,
            "trailing parity of already-emitted s=1,r=1 frame must be ignored, not re-emitted"
        );

        // Case 2: s=2, r=1 — frame 12 completes, frame 13 completes, then frame 12's
        // trailing parity arrives; it must NOT insert a zombie that flags prior_frames_dropped
        // when frame 14 completes.
        let chunk_len = f12.bytes.len().div_ceil(2);
        let mut padded12 = f12.bytes.clone();
        padded12.resize(chunk_len * 2, 0);
        let s12_0 = padded12[..chunk_len].to_vec();
        let s12_1 = padded12[chunk_len..].to_vec();
        let p12 = test_rs_encode(&[s12_0.clone(), s12_1.clone()], 1);

        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H265,
                12,
                1,
                0,
                2,
                1,
                1,
                1,
                3,
                0,
                false,
                0,
                &s12_0
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );
        let RsmIngestOutcome::Frame(out12) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H265,
                12,
                1,
                0,
                2,
                1,
                2,
                2,
                3,
                0,
                false,
                0,
                &s12_1,
            ))
            .unwrap()
        else {
            panic!("frame 12 should complete");
        };
        assert!(!out12.prior_frames_dropped);

        let RsmIngestOutcome::Frame(out13) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H265,
                13,
                1,
                0,
                1,
                0,
                1,
                1,
                1,
                0,
                false,
                0,
                &f13.bytes,
            ))
            .unwrap()
        else {
            panic!("frame 13 should complete");
        };
        assert!(!out13.prior_frames_dropped);

        // Late parity of frame 12 arrives after frame 13.
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H265,
                12,
                1,
                0,
                2,
                1,
                3,
                3,
                3,
                0,
                false,
                0,
                &p12[0]
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );

        let RsmIngestOutcome::Frame(out14) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H265,
                14,
                1,
                0,
                1,
                0,
                1,
                1,
                1,
                0,
                false,
                0,
                &f14.bytes,
            ))
            .unwrap()
        else {
            panic!("frame 14 should complete");
        };
        assert!(
            !out14.prior_frames_dropped,
            "late parity of frame 12 must not create a zombie assembly that triggers false drop on frame 14"
        );
    }

    #[test]
    fn r_full_four_parity_packet_three_is_not_misclassified_as_virtual_frame() {
        let mut annexb = vec![0, 0, 0, 1, 0x02, 0x01];
        annexb.extend_from_slice(&[0x44; 48]);
        let framed = frame_annexb_access_unit(VideoWireCodec::H265, &annexb).unwrap();
        let parities = test_rs_encode(std::slice::from_ref(&framed.bytes), 4);

        // s = 1, r = 4 (r_lo2 = 3, r_mid2 = 1, r_hi4 = 0): pkg_idx = 3 is the second RS parity
        // packet, NOT a virtual-frame metadata packet (which requires r_full == 0).
        let pkt_par2 = build_2d_rsm_pkt(
            VideoWireCodec::H265,
            30,
            1,
            0,
            1,
            4,
            3,
            3,
            5,
            0,
            false,
            0,
            &parities[1],
        );
        let mut rx = RsmVideoReassembler::new();
        let RsmIngestOutcome::Frame(frame) = rx.ingest(&pkt_par2).unwrap() else {
            panic!("s=1, r=4 parity packet #3 must recover missing source packet");
        };
        assert!(frame.fec_recovered);
        assert_eq!(frame.payload, annexb);
    }

    #[test]
    fn high_parity_additive_r_full_keyframes_reassemble_and_recover_fec() {
        let mut h264_idr = Vec::new();
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0xac, 0xd2]); // SPS
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80]); // PPS
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]); // IDR slice
        h264_idr.extend((0..1600u16).map(|i| (i.wrapping_mul(41).wrapping_add(19) & 0xff) as u8));
        let framed = frame_annexb_access_unit(VideoWireCodec::H264, &h264_idr).unwrap();

        // Cover r = 4 (r_mid2 > 0), r = 7 (r_hi4 = 1), r = 12 (r_hi4 = 6, which bit-shifting
        // previously inflated to 111 > MAX_FEC_PARITY and dropped even with 0% loss), and r = 20.
        for (s, r, drop_indices) in [
            (8usize, 4usize, vec![2usize, 6]),
            (10usize, 7usize, vec![0usize, 3, 7, 9]),
            (12usize, 12usize, vec![]),
            (12usize, 12usize, vec![1usize, 4, 8, 11]),
            (14usize, 20usize, vec![0usize, 5, 10, 13]),
        ] {
            let chunk_len = framed.bytes.len().div_ceil(s);
            let mut padded = framed.bytes.clone();
            padded.resize(s * chunk_len, 0);
            let srcs: Vec<Vec<u8>> = (0..s)
                .map(|i| padded[i * chunk_len..(i + 1) * chunk_len].to_vec())
                .collect();
            let parities = test_rs_encode(&srcs, r);
            let mut rx = RsmVideoReassembler::new();
            let mut out = None;

            for (i, src) in srcs.iter().enumerate() {
                if drop_indices.contains(&i) {
                    continue;
                }
                let mut pkt = build_2d_rsm_pkt(
                    VideoWireCodec::H264,
                    40,
                    1,
                    0,
                    s,
                    r,
                    i + 1,
                    i + 1,
                    s + r,
                    0,
                    false,
                    0,
                    src,
                );
                pkt[3] |= 0x04; // is_iframe
                pkt[3 + 12] =
                    ((framed.frame_count.saturating_sub(1)) & 0x03) | ((s as u8 & 0x3f) << 2);
                if let RsmIngestOutcome::Frame(f) = rx.ingest(&pkt).unwrap() {
                    out = Some(f);
                }
            }
            for (p_i, par) in parities.iter().enumerate() {
                let mut pkt = build_2d_rsm_pkt(
                    VideoWireCodec::H264,
                    40,
                    1,
                    0,
                    s,
                    r,
                    s + 1 + p_i,
                    s + 1 + p_i,
                    s + r,
                    0,
                    false,
                    0,
                    par,
                );
                pkt[3] |= 0x04; // is_iframe
                pkt[3 + 12] =
                    ((framed.frame_count.saturating_sub(1)) & 0x03) | ((s as u8 & 0x3f) << 2);
                if let RsmIngestOutcome::Frame(f) = rx.ingest(&pkt).unwrap() {
                    out = Some(f);
                }
            }
            let frame = out.unwrap_or_else(|| {
                panic!("H.264 IDR with s={s}, r={r}, drops={drop_indices:?} must reassemble")
            });
            assert_eq!(frame.fec_recovered, !drop_indices.is_empty());
            assert!(frame.is_keyframe);
            assert!(frame.has_codec_config);
            assert_eq!(frame.payload, h264_idr);
        }
    }

    #[test]
    fn encoder_restart_clears_stale_assemblies_even_if_first_idr_missed_and_isolates_streams() {
        let mut idr = Vec::new();
        idr.extend_from_slice(&[0, 0, 0, 1, 0x40, 0x01, 0xaa]);
        idr.extend_from_slice(&[0, 0, 0, 1, 0x42, 0x01, 0xbb]);
        idr.extend_from_slice(&[0, 0, 0, 1, 0x44, 0x01, 0xcc]);
        idr.extend_from_slice(&[0, 0, 0, 1, 0x26, 0x01]);
        idr.extend_from_slice(&[0x55; 180]);

        let mut p_frame = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame.extend_from_slice(&[0x77; 160]);

        let mut rx = RsmVideoReassembler::new();

        // Advance stream 1 to frame 40 (ts 4000ms), then leave an incomplete partial assembly
        // for frame 41 (old epoch) before the remote encoder restarts and we miss its frame-0 IDR.
        let mut tx1 = RsmVideoPacketizer::new(2, 1).with_chunk_bytes(80).unwrap();
        for n in 0..41u32 {
            let pkts = tx1
                .packetize(VideoWireCodec::H265, &p_frame, 1000 + n * 100, 15, 500)
                .unwrap();
            let mut got = false;
            for pkt in &pkts {
                if matches!(rx.ingest(pkt).unwrap(), RsmIngestOutcome::Frame(_)) {
                    got = true;
                }
            }
            assert!(got);
        }
        // Feed only packet 0 of frame 41 (stalled incomplete assembly) and frame 2 from old epoch
        // wouldn't collide, so also simulate partial frame at index 2 from a fresh restart where
        // frame 0/1 were lost and frame 2 is an IDR with a strictly newer timestamp.
        let mut tx_restarted = RsmVideoPacketizer::new(2, 1).with_chunk_bytes(80).unwrap();
        // Skip frame 0 and frame 1 to simulate lost initial post-restart packets.
        let _lost0 = tx_restarted
            .packetize(VideoWireCodec::H265, &idr, 9000, 15, 500)
            .unwrap();
        let _lost1 = tx_restarted
            .packetize(VideoWireCodec::H265, &p_frame, 9066, 15, 500)
            .unwrap();
        // Frame 2 is an IDR with advanced timestamp (9133 > 5000) and lag = 40 - 2 = 38 < 256.
        let idr_pkts = tx_restarted
            .packetize(VideoWireCodec::H265, &idr, 9133, 15, 500)
            .unwrap();
        assert!(idr_pkts.len() >= 2);
        let mut recovered = None;
        for pkt in &idr_pkts {
            if let RsmIngestOutcome::Frame(f) = rx.ingest(pkt).unwrap() {
                recovered = Some(f);
            }
        }
        let f = recovered.expect("post-restart IDR with missed frame 0 must reassemble");
        assert_eq!(f.frame_index, 3);
        assert_eq!(f.timestamp_ms, 9133);
        assert!(f.is_keyframe);
        assert_eq!(f.payload, idr);

        // Also test timestamp reset back near 0 after a long session (last=40 -> frame 1 @ ts 100ms).
        let mut tx_reset = RsmVideoPacketizer::new(2, 1).with_chunk_bytes(80).unwrap();
        for n in 0..41u32 {
            let pkts = tx_reset
                .packetize(VideoWireCodec::H265, &p_frame, 10_000 + n * 100, 15, 500)
                .unwrap();
            for pkt in &pkts {
                let _ = rx.ingest(pkt).unwrap();
            }
        }
        // Leave a partial assembly at index 1 in the active map? Build a restarted stream at ts=100.
        let mut tx_zero = RsmVideoPacketizer::new(2, 1).with_chunk_bytes(80).unwrap();
        let _skip0 = tx_zero
            .packetize(VideoWireCodec::H265, &idr, 50, 15, 500)
            .unwrap();
        let reset_idr = tx_zero
            .packetize(VideoWireCodec::H265, &idr, 100, 15, 500)
            .unwrap();
        let mut got_reset = None;
        for pkt in &reset_idr {
            if let RsmIngestOutcome::Frame(f) = rx.ingest(pkt).unwrap() {
                got_reset = Some(f);
            }
        }
        assert_eq!(
            got_reset
                .expect("encoder reset back near ts=0 must reassemble")
                .payload,
            idr
        );

        // Multi-stream isolation: stream_id 1 and stream_id 2 keep independent frame indices
        // and virtual-frame orientation state.
        let mut multi_rx = RsmVideoReassembler::new();
        let mut s1 = RsmVideoPacketizer::new(2, 1).with_chunk_bytes(80).unwrap();
        let mut s2 = RsmVideoPacketizer::new(2, 2).with_chunk_bytes(80).unwrap();
        let s1_pkts = s1
            .packetize(VideoWireCodec::H265, &idr, 100, 15, 500)
            .unwrap();
        let s2_pkts = s2
            .packetize(VideoWireCodec::H265, &p_frame, 200, 15, 500)
            .unwrap();
        assert!(s1_pkts.len() >= 2 && s2_pkts.len() >= 2);
        // Interleave packets from stream 1 and stream 2 with the exact same frame_index (0).
        assert_eq!(
            multi_rx.ingest(&s1_pkts[0]).unwrap(),
            RsmIngestOutcome::Pending
        );
        assert_eq!(
            multi_rx.ingest(&s2_pkts[0]).unwrap(),
            RsmIngestOutcome::Pending
        );
        let mut out1 = None;
        let mut out2 = None;
        for pkt in &s1_pkts[1..] {
            if let RsmIngestOutcome::Frame(f) = multi_rx.ingest(pkt).unwrap() {
                out1 = Some(f);
            }
        }
        for pkt in &s2_pkts[1..] {
            if let RsmIngestOutcome::Frame(f) = multi_rx.ingest(pkt).unwrap() {
                out2 = Some(f);
            }
        }
        let f1 = out1.expect("stream 1 frame must reassemble");
        let f2 = out2.expect("stream 2 frame must reassemble");
        assert_eq!(f1.stream_id, 1);
        assert_eq!(f1.payload, idr);
        assert!(!f1.prior_frames_dropped);
        assert_eq!(f2.stream_id, 2);
        assert_eq!(f2.payload, p_frame);
        assert!(!f2.prior_frames_dropped);
    }

    #[test]
    fn rsm_eviction_on_non_final_packet_sets_prior_frames_dropped_on_next_completed_frame() {
        let mut p_frame = vec![0, 0, 0, 1, 0x02, 0x01];
        p_frame.extend((0..240u8).map(|i| i.wrapping_mul(5)));
        let mut tx = RsmVideoPacketizer::new(2, 1).with_chunk_bytes(80).unwrap();
        let mut rx = RsmVideoReassembler::new();

        // Fill MAX_ACTIVE_ASSEMBLIES (8) with incomplete multi-packet frames so the 9th evicts frame 0.
        for i in 0..MAX_ACTIVE_ASSEMBLIES as u32 {
            let pkts = tx
                .packetize(VideoWireCodec::H265, &p_frame, 100 + i * 66, 15, 500)
                .unwrap();
            assert!(pkts.len() >= 2, "expected multi-packet P-frame");
            assert_eq!(rx.ingest(&pkts[0]).unwrap(), RsmIngestOutcome::Pending);
        }

        // First packet of the 9th frame evicts the oldest incomplete assembly and asks for a keyframe.
        let ninth = tx
            .packetize(VideoWireCodec::H265, &p_frame, 1000, 15, 500)
            .unwrap();
        assert!(ninth.len() >= 2);
        assert_eq!(
            rx.ingest(&ninth[0]).unwrap(),
            RsmIngestOutcome::FrameDroppedNeedKeyframe
        );

        // Remaining packets complete the 9th frame; prior_frames_dropped must still be true!
        let mut completed = None;
        for pkt in &ninth[1..] {
            if let RsmIngestOutcome::Frame(frame) = rx.ingest(pkt).unwrap() {
                completed = Some(frame);
            }
        }
        let frame = completed.expect("9th frame must complete");
        assert!(
            frame.prior_frames_dropped,
            "eviction during an earlier packet must propagate prior_frames_dropped to the completed frame"
        );
    }

    #[test]
    fn bwe_probe_flagged_h264_idr_packets_reassemble_and_degenerate_probes_skip() {
        let mut h264_idr = Vec::new();
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0xac, 0xd2]); // SPS
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80]); // PPS
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]); // IDR slice
        h264_idr.extend((0..460u16).map(|i| (i.wrapping_mul(13) & 0xff) as u8));

        let mut tx = RsmVideoPacketizer::new(0, 1)
            .with_orientation(5)
            .with_chunk_bytes(100)
            .unwrap();
        let mut pkts = tx
            .packetize(VideoWireCodec::H264, &h264_idr, 12_345, 15, 500)
            .unwrap();
        assert_eq!(pkts.len(), 5);

        // Set EncFecHeader byte 13 bit 4 (0x10) on the first 4 packets, matching
        // mobile HW264 initial IDR bursts (`hdr[13] = 0xb0`), and verify all packets
        // are retained rather than dropped as padding.
        for pkt in &mut pkts[..4] {
            pkt[3 + 13] |= 0x10;
            assert_eq!(pkt[3 + 13], 0xb0);
        }

        let mut rx = RsmVideoReassembler::new();
        // A degenerate padding-only probe packet (`f_tot_pkts == 0`) must still be skipped.
        let mut degenerate_probe = pkts[0].clone();
        degenerate_probe[3 + 19] = 0;
        assert_eq!(
            rx.ingest(&degenerate_probe).unwrap(),
            RsmIngestOutcome::Pending
        );

        let mut out = None;
        for pkt in &pkts {
            if let RsmIngestOutcome::Frame(frame) = rx.ingest(pkt).unwrap() {
                out = Some(frame);
            }
        }
        let frame = out.expect("BWE-flagged H264 IDR must reassemble");
        assert_eq!(frame.codec, VideoWireCodec::H264);
        assert!(frame.is_keyframe);
        assert!(frame.has_codec_config);
        assert_eq!(frame.orientation, 5);
        assert!(!frame.prior_frames_dropped);
        assert_eq!(frame.payload, h264_idr);
    }

    #[test]
    fn irequest_with_force_encodes_64bit_lost_seq_and_force_flag() {
        let forced = encode_irequest_with_force(1, 0, 1, 0x1122_3344, 0x0001_0000_002a, true);
        assert_eq!(forced.len(), 58);
        assert_eq!(
            u64::from_le_bytes(forced[6 + 0x24..6 + 0x2c].try_into().unwrap()),
            0x0001_0000_002a
        );
        assert_eq!(
            u32::from_le_bytes(forced[6 + 0x2c..6 + 0x30].try_into().unwrap()),
            1
        );
        let unforced = encode_irequest_with_force(1, 0, 1, 0x1122_3344, 42, false);
        assert_eq!(
            u32::from_le_bytes(unforced[6 + 0x2c..6 + 0x30].try_into().unwrap()),
            0
        );
    }

    #[test]
    fn paced_fec_iframe_survives_newer_p_frame_completion_and_emits_in_order() {
        let mut h264_idr = Vec::new();
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0xac, 0xd2]); // SPS
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80]); // PPS
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]); // IDR
        h264_idr.extend((0..600u16).map(|i| (i.wrapping_mul(31).wrapping_add(5) & 0xff) as u8));
        let idr_framed = frame_annexb_access_unit(VideoWireCodec::H264, &h264_idr).unwrap();

        let mut p_frame = vec![0, 0, 0, 1, 0x41, 0x9a];
        p_frame.extend((0..140u8).map(|i| i.wrapping_mul(7).wrapping_add(3)));
        let p_framed = frame_annexb_access_unit(VideoWireCodec::H264, &p_frame).unwrap();

        // Build paced IDR (frame 50: s=5, r=4) where source packet 4 is lost and parity 0 arrives
        // AFTER P-frame 51 (s=2, r=1) has already completed.
        let s_idr = 5usize;
        let r_idr = 4usize;
        let idr_chunk = idr_framed.bytes.len().div_ceil(s_idr);
        let mut idr_pad = idr_framed.bytes.clone();
        idr_pad.resize(s_idr * idr_chunk, 0);
        let idr_srcs: Vec<Vec<u8>> = (0..s_idr)
            .map(|i| idr_pad[i * idr_chunk..(i + 1) * idr_chunk].to_vec())
            .collect();
        let idr_pars = test_rs_encode(&idr_srcs, r_idr);

        let make_idr_pkt = |pkg_1based: usize, payload: &[u8]| {
            let mut pkt = build_2d_rsm_pkt(
                VideoWireCodec::H264,
                50,
                1,
                0,
                s_idr,
                r_idr,
                pkg_1based,
                pkg_1based,
                s_idr + r_idr,
                0,
                false,
                0,
                payload,
            );
            pkt[3] |= 0x04;
            pkt[3 + 12] =
                ((idr_framed.frame_count.saturating_sub(1)) & 0x03) | ((s_idr as u8 & 0x3f) << 2);
            pkt
        };

        let p_chunk = p_framed.bytes.len().div_ceil(2);
        let mut p_pad = p_framed.bytes.clone();
        p_pad.resize(2 * p_chunk, 0);
        let p_srcs = [p_pad[..p_chunk].to_vec(), p_pad[p_chunk..].to_vec()];
        let p_pkt1 = build_2d_rsm_pkt(
            VideoWireCodec::H264,
            51,
            1,
            0,
            2,
            1,
            1,
            1,
            3,
            0,
            false,
            0,
            &p_srcs[0],
        );
        let p_pkt2 = build_2d_rsm_pkt(
            VideoWireCodec::H264,
            51,
            1,
            0,
            2,
            1,
            2,
            2,
            3,
            0,
            false,
            0,
            &p_srcs[1],
        );

        let mut rx = RsmVideoReassembler::new();
        // IDR packets 1..4 arrive (packet 5 lost).
        for (i, src) in idr_srcs.iter().enumerate().take(4) {
            assert_eq!(
                rx.ingest(&make_idr_pkt(i + 1, src)).unwrap(),
                RsmIngestOutcome::Pending
            );
        }
        // Next P-frame 51 completes BEFORE IDR parity arrives: must NOT evict IDR 50!
        assert_eq!(rx.ingest(&p_pkt1).unwrap(), RsmIngestOutcome::Pending);
        assert_eq!(rx.ingest(&p_pkt2).unwrap(), RsmIngestOutcome::Pending);

        // IDR parity packet 6 arrives -> RS recovers packet 5 and emits IDR 50 first.
        let RsmIngestOutcome::Frame(out_idr) = rx.ingest(&make_idr_pkt(6, &idr_pars[0])).unwrap()
        else {
            panic!("expected FEC-recovered IDR 50");
        };
        assert_eq!(out_idr.frame_index, 50);
        assert!(out_idr.is_keyframe);
        assert!(out_idr.has_codec_config);
        assert!(out_idr.fec_recovered);
        assert!(!out_idr.prior_frames_dropped);
        assert_eq!(out_idr.payload, h264_idr);

        // Immediately drain buffered P-frame 51 via pop_ready without waiting for another packet.
        let out_p = rx
            .pop_ready()
            .expect("expected buffered P-frame 51 to drain immediately after IDR 50");
        assert_eq!(out_p.frame_index, 51);
        assert!(!out_p.is_keyframe);
        assert!(!out_p.prior_frames_dropped);
        assert_eq!(out_p.payload, p_frame);
        assert!(rx.pop_ready().is_none());

        // Trailing IDR parity packet 7 is safely ignored after both frames emitted.
        assert_eq!(
            rx.ingest(&make_idr_pkt(7, &idr_pars[1])).unwrap(),
            RsmIngestOutcome::Pending
        );
    }

    #[test]
    fn fec_protected_p_frame_recovers_after_next_frame_completes_without_false_drop() {
        let mut p1 = vec![0, 0, 0, 1, 0x41, 0x9a];
        p1.extend((0..160u8).map(|i| i.wrapping_mul(3).wrapping_add(1)));
        let f1 = frame_annexb_access_unit(VideoWireCodec::H264, &p1).unwrap();
        let mut p2 = vec![0, 0, 0, 1, 0x41, 0x9a];
        p2.extend((0..140u8).map(|i| i.wrapping_mul(11).wrapping_add(9)));
        let f2 = frame_annexb_access_unit(VideoWireCodec::H264, &p2).unwrap();

        let c1 = f1.bytes.len().div_ceil(2);
        let mut pad1 = f1.bytes.clone();
        pad1.resize(2 * c1, 0);
        let s1 = vec![pad1[..c1].to_vec(), pad1[c1..].to_vec()];
        let par1 = test_rs_encode(&s1, 1);

        let c2 = f2.bytes.len().div_ceil(2);
        let mut pad2 = f2.bytes.clone();
        pad2.resize(2 * c2, 0);
        let s2 = vec![pad2[..c2].to_vec(), pad2[c2..].to_vec()];
        let par2 = test_rs_encode(&s2, 1);

        let mut rx = RsmVideoReassembler::new();
        // Frame 10 loses source packet 2; frame 11 completes before frame 10's XOR parity arrives.
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                10,
                1,
                0,
                2,
                1,
                1,
                1,
                3,
                0,
                false,
                0,
                &s1[0]
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                11,
                1,
                0,
                2,
                1,
                1,
                1,
                3,
                0,
                false,
                0,
                &s2[0]
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                11,
                1,
                0,
                2,
                1,
                2,
                2,
                3,
                0,
                false,
                0,
                &s2[1]
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );

        // Frame 10's XOR parity arrives -> recovers frame 10 with fec_recovered=true and no drop!
        let RsmIngestOutcome::Frame(out10) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                10,
                1,
                0,
                2,
                1,
                3,
                3,
                3,
                0,
                false,
                0,
                &par1[0],
            ))
            .unwrap()
        else {
            panic!("expected FEC-recovered P-frame 10");
        };
        assert_eq!(out10.frame_index, 10);
        assert!(out10.fec_recovered);
        assert!(!out10.prior_frames_dropped);
        assert_eq!(out10.payload, p1);

        // Frame 11's parity packet flushes buffered frame 11 in order.
        let RsmIngestOutcome::Frame(out11) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                11,
                1,
                0,
                2,
                1,
                3,
                3,
                3,
                0,
                false,
                0,
                &par2[0],
            ))
            .unwrap()
        else {
            panic!("expected buffered P-frame 11");
        };
        assert_eq!(out11.frame_index, 11);
        assert!(!out11.prior_frames_dropped);
        assert_eq!(out11.payload, p2);
    }

    #[test]
    fn unrecoverable_fec_p_frame_evicts_after_grace_window_and_recovery_keyframe_evicts_immediately(
    ) {
        let mut p_frame = vec![0, 0, 0, 1, 0x41, 0x9a];
        p_frame.extend((0..120u8).map(|i| i.wrapping_mul(5).wrapping_add(1)));
        let pf = frame_annexb_access_unit(VideoWireCodec::H264, &p_frame).unwrap();

        let mut idr = vec![0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0, 0, 0, 1, 0x68, 0xee];
        idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
        idr.extend((0..120u8).map(|i| i.wrapping_mul(9).wrapping_add(2)));
        let idrf = frame_annexb_access_unit(VideoWireCodec::H264, &idr).unwrap();

        // Part 1: Frame 20 (s=3, r=1) only gets 1 packet (unrecoverable). Frames 21..=22 stay
        // buffered within FEC_P_GRACE_FRAMES (2); frame 23 exceeds grace, evicts 20, and emits 21
        // with prior_frames_dropped = true.
        let mut rx = RsmVideoReassembler::new();
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                20,
                1,
                0,
                3,
                1,
                1,
                1,
                4,
                0,
                false,
                0,
                &pf.bytes[..44]
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );
        for fidx in [21u32, 22u32] {
            assert_eq!(
                rx.ingest(&build_2d_rsm_pkt(
                    VideoWireCodec::H264,
                    fidx,
                    1,
                    0,
                    1,
                    0,
                    1,
                    1,
                    1,
                    0,
                    false,
                    0,
                    &pf.bytes
                ))
                .unwrap(),
                RsmIngestOutcome::Pending
            );
        }
        let RsmIngestOutcome::Frame(out21) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                23,
                1,
                0,
                1,
                0,
                1,
                1,
                1,
                0,
                false,
                0,
                &pf.bytes,
            ))
            .unwrap()
        else {
            panic!("expected buffered frame 21 to flush once frame 20 exceeds grace window");
        };
        assert_eq!(out21.frame_index, 21);
        assert!(out21.prior_frames_dropped);

        // Part 2: An incomplete FEC P-frame (frame 30) is immediately evicted when a complete
        // recovery keyframe (frame 31) arrives.
        let mut rx2 = RsmVideoReassembler::new();
        assert_eq!(
            rx2.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                30,
                1,
                0,
                2,
                1,
                1,
                1,
                3,
                0,
                false,
                0,
                &pf.bytes[..64]
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );
        let mut idr_pkt = build_2d_rsm_pkt(
            VideoWireCodec::H264,
            31,
            1,
            0,
            1,
            0,
            1,
            1,
            1,
            0,
            false,
            0,
            &idrf.bytes,
        );
        idr_pkt[3] |= 0x04;
        idr_pkt[3 + 12] = ((idrf.frame_count.saturating_sub(1)) & 0x03) | (1 << 2);
        let RsmIngestOutcome::Frame(out_kf) = rx2.ingest(&idr_pkt).unwrap() else {
            panic!("recovery keyframe must emit immediately");
        };
        assert_eq!(out_kf.frame_index, 31);
        assert!(out_kf.is_keyframe);
        assert!(out_kf.prior_frames_dropped);
        assert_eq!(out_kf.payload, idr);
    }

    #[test]
    fn intermediate_drop_attaches_after_keyframe_and_rejects_zombie_resurrection() {
        let mut idr = vec![0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0, 0, 0, 1, 0x68, 0xee];
        idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
        idr.extend((0..240u8).map(|i| i.wrapping_mul(7).wrapping_add(3)));
        let idrf = frame_annexb_access_unit(VideoWireCodec::H264, &idr).unwrap();

        let mut p_frame = vec![0, 0, 0, 1, 0x41, 0x9a];
        p_frame.extend((0..120u8).map(|i| i.wrapping_mul(5).wrapping_add(1)));
        let pf = frame_annexb_access_unit(VideoWireCodec::H264, &p_frame).unwrap();

        let s_idr = 3usize;
        let r_idr = 2usize;
        let idr_chunk = idrf.bytes.len().div_ceil(s_idr);
        let mut idr_pad = idrf.bytes.clone();
        idr_pad.resize(s_idr * idr_chunk, 0);
        let idr_srcs: Vec<Vec<u8>> = (0..s_idr)
            .map(|i| idr_pad[i * idr_chunk..(i + 1) * idr_chunk].to_vec())
            .collect();
        let idr_pars = test_rs_encode(&idr_srcs, r_idr);
        let make_idr_pkt = |pkg_1based: usize, payload: &[u8]| {
            let mut pkt = build_2d_rsm_pkt(
                VideoWireCodec::H264,
                50,
                1,
                0,
                s_idr,
                r_idr,
                pkg_1based,
                pkg_1based,
                s_idr + r_idr,
                0,
                false,
                0,
                payload,
            );
            pkt[3] |= 0x04;
            pkt[3 + 12] =
                ((idrf.frame_count.saturating_sub(1)) & 0x03) | ((s_idr as u8 & 0x3f) << 2);
            pkt
        };

        let mut rx = RsmVideoReassembler::new();
        // IDR 50 receives 2 of 3 source packets (still within IFRAME_GRACE_FRAMES = 4).
        assert_eq!(
            rx.ingest(&make_idr_pkt(1, &idr_srcs[0])).unwrap(),
            RsmIngestOutcome::Pending
        );
        assert_eq!(
            rx.ingest(&make_idr_pkt(2, &idr_srcs[1])).unwrap(),
            RsmIngestOutcome::Pending
        );
        // P-frame 51 (s=3, r=1, grace=2) receives only 1 packet.
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                51,
                1,
                0,
                3,
                1,
                1,
                1,
                4,
                0,
                false,
                0,
                &pf.bytes[..44]
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );
        // Single-packet P-frames 52, 53, 54 complete: frame 54 evicts P-frame 51 (distance 3 > 2)
        // while keeping IDR 50 alive (distance 4 <= 4).
        for fidx in [52u32, 53u32, 54u32] {
            assert_eq!(
                rx.ingest(&build_2d_rsm_pkt(
                    VideoWireCodec::H264,
                    fidx,
                    1,
                    0,
                    1,
                    0,
                    1,
                    1,
                    1,
                    0,
                    false,
                    0,
                    &pf.bytes
                ))
                .unwrap(),
                RsmIngestOutcome::Pending
            );
        }

        // A late trailing packet for already-evicted P-frame 51 must NOT resurrect a zombie assembly.
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                51,
                1,
                0,
                3,
                1,
                4,
                4,
                4,
                0,
                false,
                0,
                &pf.bytes[..44]
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );

        // IDR 50 recovers via parity -> emits 50 WITHOUT consuming the newer drop of 51.
        let RsmIngestOutcome::Frame(out50) = rx.ingest(&make_idr_pkt(4, &idr_pars[0])).unwrap()
        else {
            panic!("expected FEC-recovered IDR 50");
        };
        assert_eq!(out50.frame_index, 50);
        assert!(out50.is_keyframe);
        assert!(!out50.prior_frames_dropped);

        // Draining pop_ready emits 52 with prior_frames_dropped=true (reporting evicted 51),
        // then 53 and 54 with prior_frames_dropped=false, with zero zombie stall.
        let out52 = rx.pop_ready().expect("expected buffered P-frame 52");
        assert_eq!(out52.frame_index, 52);
        assert!(out52.prior_frames_dropped);
        let out53 = rx.pop_ready().expect("expected buffered P-frame 53");
        assert_eq!(out53.frame_index, 53);
        assert!(!out53.prior_frames_dropped);
        let out54 = rx.pop_ready().expect("expected buffered P-frame 54");
        assert_eq!(out54.frame_index, 54);
        assert!(!out54.prior_frames_dropped);
        assert!(rx.pop_ready().is_none());
    }

    #[test]
    fn ready_queue_drains_in_order_across_16bit_frame_index_wraparound() {
        let mut p_frame = vec![0, 0, 0, 1, 0x41, 0x9a];
        p_frame.extend((0..120u8).map(|i| i.wrapping_mul(3).wrapping_add(7)));
        let pf = frame_annexb_access_unit(VideoWireCodec::H264, &p_frame).unwrap();
        let half = pf.bytes.len().div_ceil(2);
        let mut pad = pf.bytes.clone();
        pad.resize(2 * half, 0);
        let s0 = &pad[..half];
        let s1 = &pad[half..];

        let mut rx = RsmVideoReassembler::new();
        // Frame 0xfffe (FEC-protected P-frame) starts incomplete.
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                0xfffe,
                1,
                0,
                2,
                1,
                1,
                1,
                3,
                0,
                false,
                0,
                s0
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );
        // Frames 0xffff and 0x0000 complete while 0xfffe is within FEC_P_GRACE_FRAMES (2).
        for fidx in [0xffffu32, 0x0000u32] {
            assert_eq!(
                rx.ingest(&build_2d_rsm_pkt(
                    VideoWireCodec::H264,
                    fidx,
                    1,
                    0,
                    1,
                    0,
                    1,
                    1,
                    1,
                    0,
                    false,
                    0,
                    &pf.bytes
                ))
                .unwrap(),
                RsmIngestOutcome::Pending
            );
        }
        // Second source packet finishes 0xfffe; pop_ready drains 0xffff and 0x0000 in wrap order.
        let RsmIngestOutcome::Frame(first) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                0xfffe,
                1,
                0,
                2,
                1,
                2,
                2,
                3,
                0,
                false,
                0,
                s1,
            ))
            .unwrap()
        else {
            panic!("expected 0xfffe to emit first");
        };
        assert_eq!(first.frame_index, 0xfffe);
        assert!(!first.prior_frames_dropped);
        assert_eq!(rx.pop_ready().unwrap().frame_index, 0xffff);
        assert_eq!(rx.pop_ready().unwrap().frame_index, 0x0000);
        assert!(rx.pop_ready().is_none());
    }

    #[test]
    fn failed_unframe_records_drop_and_rejects_trailing_parity_without_zombie_stall() {
        let mut p_frame = vec![0, 0, 0, 1, 0x41, 0x9a];
        p_frame.extend((0..120u8).map(|i| i.wrapping_mul(3).wrapping_add(7)));
        let pf = frame_annexb_access_unit(VideoWireCodec::H264, &p_frame).unwrap();

        // Malformed H.264 framed payload (truncated length prefix) triggers an unframe error on
        // completion of frame 70 (s=1, r=1).
        let bad_payload = vec![0xffu8; 8];
        let mut rx = RsmVideoReassembler::new();
        assert!(rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                70,
                1,
                0,
                1,
                1,
                1,
                1,
                2,
                0,
                false,
                0,
                &bad_payload,
            ))
            .is_err());

        // Trailing parity packet for frame 70 must be rejected via pending_drops instead of
        // creating a parity-only zombie assembly.
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                70,
                1,
                0,
                1,
                1,
                2,
                2,
                2,
                0,
                false,
                0,
                &bad_payload,
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );

        // Next valid frame 71 emits immediately (not held back by a zombie frame 70) and reports
        // prior_frames_dropped = true.
        let RsmIngestOutcome::Frame(out71) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                71,
                1,
                0,
                1,
                0,
                1,
                1,
                1,
                0,
                false,
                0,
                &pf.bytes,
            ))
            .unwrap()
        else {
            panic!("frame 71 must emit immediately without zombie stall");
        };
        assert_eq!(out71.frame_index, 71);
        assert!(out71.prior_frames_dropped);

        let RsmIngestOutcome::Frame(out72) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                72,
                1,
                0,
                1,
                0,
                1,
                1,
                1,
                0,
                false,
                0,
                &pf.bytes,
            ))
            .unwrap()
        else {
            panic!("frame 72 must emit immediately");
        };
        assert_eq!(out72.frame_index, 72);
        assert!(!out72.prior_frames_dropped);
    }

    #[test]
    fn video_nack_encodes_target_first_28_byte_single_range_and_roundtrips() {
        // Single range: 8-byte header + 4-byte range + 16 zero tail = 28 bytes.
        let pkt = encode_video_nack(3, 7, 1, 42, &[(100, 103)]);
        assert_eq!(pkt.len(), 28);
        assert_eq!(pkt[0], 0x06);
        // Target member (7, stream 1) comes before local member (3, stream 1).
        assert_eq!([pkt[1], pkt[2]], pack_rtcp_member(7, 1));
        assert_eq!([pkt[3], pkt[4]], pack_rtcp_member(3, 1));
        assert_eq!(pkt[5], 0);
        assert_eq!(pkt[6], 42);
        assert_eq!(pkt[7], 1);
        assert_eq!(&pkt[12..28], &[0u8; 16]);

        let classified = classify_video_channel_payload(&pkt).unwrap();
        let VideoChannelPayload::Nack(raw) = classified else {
            panic!("expected Nack classification");
        };
        let parsed = parse_video_nack(raw).unwrap();
        assert_eq!(parsed.target_member, 7);
        assert_eq!(parsed.target_stream_id, 1);
        assert_eq!(parsed.local_member, 3);
        assert_eq!(parsed.local_stream_id, 1);
        assert_eq!(parsed.is_screen, 0);
        assert_eq!(parsed.arq_seq, 42);
        assert_eq!(parsed.ranges, vec![(100, 103)]);
    }

    #[test]
    fn video_nack_filters_empty_ranges_and_caps_at_max() {
        let mut ranges = vec![(10, 10)]; // empty range filtered out
        for i in 0..25u16 {
            ranges.push((100 + i * 2, 101 + i * 2));
        }
        let pkt = encode_video_nack(1, 2, 0, 9, &ranges);
        let parsed = parse_video_nack(&pkt).unwrap();
        assert_eq!(parsed.ranges.len(), MAX_VIDEO_NACK_RANGES);
        assert_eq!(parsed.ranges[0], (100, 101));
        assert_eq!(parsed.ranges[MAX_VIDEO_NACK_RANGES - 1], (130, 131));
        assert_eq!(pkt.len(), 8 + MAX_VIDEO_NACK_RANGES * 4 + 16);
    }

    #[test]
    fn p2s_report_encodes_live_video_and_link_telemetry_fields() {
        let report = VideoP2sReport {
            remote_member_id: 7,
            stream_id: 1,
            conn_id: 42,
            recv_data_interval_ms: 420,
            delay_ms: 65,
            est_bw_kbps: 240,
            send_kbps: 310,
            recv_kbps: 185,
            send_fps: 12,
            recv_fps: 8,
            capt_fps: 12,
            loss_pct: 18,
            codec_support_mask: 0x1d,
            loss_pkts: 14,
            decode_type: 9,
            play_fps: 8,
            play_frames: 123,
            max_size_format: 6,
            light_freeze_cnt: 2,
            heavy_freeze_cnt: 1,
            light_freeze_time_ratio: 35,
            heavy_freeze_time_ratio: 52,
            short_loss_pct: 18,
            long_loss_pct: 11,
            recv_pkgs: 64,
            expected_member_num: 1,
            actual_member_num: 1,
            recv_arq_resp_pkgs: 5,
        };
        let raw = encode_p2s_report(19, 2, 4, &report).unwrap();
        assert_eq!(raw[0], 0x0b);
        let outer = p::decode(&raw[1..]).unwrap();
        assert_eq!(opt_u32(&outer, 1).unwrap(), Some(19));
        assert_eq!(opt_u32(&outer, 4).unwrap(), None);
        let inner = p::decode(p::optional_data(&outer, 3).unwrap().unwrap()).unwrap();
        let head = p::decode(p::optional_data(&inner, 1).unwrap().unwrap()).unwrap();
        assert_eq!(opt_u32(&head, 3).unwrap(), Some(19));
        assert_eq!(opt_u32(&head, 4).unwrap(), Some(4));
        let v_wrap = p::decode(p::optional_data(&inner, 3).unwrap().unwrap()).unwrap();
        let vp2s = p::decode(p::optional_data(&v_wrap, 1).unwrap().unwrap()).unwrap();
        assert_eq!(opt_u32(&vp2s, 1).unwrap(), Some(7));
        assert_eq!(opt_u32(&vp2s, 2).unwrap(), Some(1));
        assert_eq!(opt_u32(&vp2s, 5).unwrap(), Some(420));
        assert_eq!(opt_u32(&vp2s, 6).unwrap(), Some(65));
        assert_eq!(opt_u32(&vp2s, 7).unwrap(), Some(240));
        // Fields 8/10/12 describe the remote sender's stream as received.
        assert_eq!(opt_u32(&vp2s, 8).unwrap(), Some(185));
        assert_eq!(opt_u32(&vp2s, 9).unwrap(), Some(185));
        assert_eq!(opt_u32(&vp2s, 10).unwrap(), Some(8));
        assert_eq!(opt_u32(&vp2s, 11).unwrap(), Some(8));
        assert_eq!(opt_u32(&vp2s, 12).unwrap(), Some(8));
        assert_eq!(opt_u32(&vp2s, 16).unwrap(), Some(6));
        for field in [17, 19, 20] {
            assert_eq!(opt_u32(&vp2s, field).unwrap(), Some(0));
        }
        assert_eq!(opt_u32(&vp2s, 14).unwrap(), Some(18));
        // Bit 0x01 is never valid in u8VCodecSupport.
        assert_eq!(opt_u32(&vp2s, 24).unwrap(), Some(0x1c));
        assert_eq!(opt_u32(&vp2s, 26).unwrap(), Some(14));
        // Wire H.264 (9) is reported as the HW H.264 decoder bit.
        assert_eq!(opt_u32(&vp2s, 29).unwrap(), Some(8));
        assert_eq!(opt_u32(&vp2s, 30).unwrap(), Some(8));
        assert_eq!(opt_u32(&vp2s, 31).unwrap(), Some(123));
        assert_eq!(opt_u32(&vp2s, 32).unwrap(), Some(2));
        assert_eq!(opt_u32(&vp2s, 33).unwrap(), Some(1));
        assert_eq!(opt_u32(&vp2s, 34).unwrap(), Some(35));
        assert_eq!(opt_u32(&vp2s, 35).unwrap(), Some(52));
        assert_eq!(opt_u32(&vp2s, 47).unwrap(), Some(18));
        assert_eq!(opt_u32(&vp2s, 48).unwrap(), Some(11));

        let vn_wrap = p::decode(p::optional_data(&inner, 6).unwrap().unwrap()).unwrap();
        let vlink = p::decode(p::optional_data(&vn_wrap, 1).unwrap().unwrap()).unwrap();
        assert_eq!(opt_u32(&vlink, 1).unwrap(), Some(42));
        assert_eq!(opt_u32(&vlink, 2).unwrap(), Some(64));
        assert_eq!(opt_u32(&vlink, 3).unwrap(), Some(18));
        assert_eq!(opt_u32(&vlink, 4).unwrap(), Some(1));
        assert_eq!(opt_u32(&vlink, 5).unwrap(), Some(1));
        assert_eq!(opt_u32(&vlink, 6).unwrap(), Some(5));

        // Baseline encode_p2s keeps the 15-field video_p2s wire layout and omits
        // inner.f6 (videoLink); the header carries the sequence and local member.
        let baseline = encode_p2s(7, 2, 1, 0x1d, 15).unwrap();
        let expected_client_hdr = p::message(&[
            p::uint(1, 11),
            p::uint(2, 0),
            p::uint(3, 7),
            p::uint(4, 1),
            p::uint(5, 4),
            p::uint(6, 111),
            p::uint(7, 0x050000cb),
        ])
        .unwrap();
        let expected_video_p2s = p::message(&[
            p::uint(1, 0),
            p::uint(2, 1),
            p::uint(4, 0x7e),
            p::uint(8, 500),
            p::uint(9, 500),
            p::uint(10, 15),
            p::uint(11, 15),
            p::uint(12, 15),
            p::uint(15, 15),
            p::uint(16, 8),
            p::uint(17, 0),
            p::uint(19, 0),
            p::uint(20, 0),
            p::uint(24, 0x1c),
            p::uint(29, 4),
        ])
        .unwrap();
        let expected_video_wrap = p::message(&[p::bytes(1, &expected_video_p2s)]).unwrap();
        let expected_inner = p::message(&[
            p::bytes(1, &expected_client_hdr),
            p::bytes(3, &expected_video_wrap),
        ])
        .unwrap();
        let expected_outer =
            p::message(&[p::uint(1, 7), p::uint(2, 2), p::bytes(3, &expected_inner)]).unwrap();
        let mut expected_baseline = Vec::with_capacity(1 + expected_outer.len());
        expected_baseline.push(0x0b);
        expected_baseline.extend_from_slice(&expected_outer);
        assert_eq!(baseline, expected_baseline);

        let baseline_outer = p::decode(&baseline[1..]).unwrap();
        let baseline_inner =
            p::decode(p::optional_data(&baseline_outer, 3).unwrap().unwrap()).unwrap();
        let baseline_inner_tags: Vec<u32> = baseline_inner.iter().map(|f| f.number).collect();
        assert_eq!(baseline_inner_tags, vec![1, 3]);
        let baseline_vwrap =
            p::decode(p::optional_data(&baseline_inner, 3).unwrap().unwrap()).unwrap();
        let baseline_vp2s =
            p::decode(p::optional_data(&baseline_vwrap, 1).unwrap().unwrap()).unwrap();
        let baseline_vp2s_tags: Vec<u32> = baseline_vp2s.iter().map(|f| f.number).collect();
        assert_eq!(
            baseline_vp2s_tags,
            vec![1, 2, 4, 8, 9, 10, 11, 12, 15, 16, 17, 19, 20, 24, 29]
        );
    }

    #[test]
    fn multi_group_uneven_tail_s_per_group_reassembles_and_recovers_fec() {
        let mut annexb = vec![0, 0, 0, 1, 0x41, 0x9a];
        for i in 0..480u16 {
            annexb.push(((i * 43 + 17) & 0xff) as u8);
        }
        let framed = frame_annexb_access_unit(VideoWireCodec::H264, &annexb).unwrap();
        // 5 total source chunks across x_groups = 2: group 0 has s = 3, group 1 has s = 2.
        let s0 = 3usize;
        let s1 = 2usize;
        let r = 1usize;
        let total_src = s0 + s1;
        let chunk_len = framed.bytes.len().div_ceil(total_src);
        let mut padded = framed.bytes.clone();
        padded.resize(total_src * chunk_len, 0);
        let total_pkts = (s0 + r) + (s1 + r);

        let g0_srcs: Vec<Vec<u8>> = (0..s0)
            .map(|c| padded[c * chunk_len..(c + 1) * chunk_len].to_vec())
            .collect();
        let g1_srcs: Vec<Vec<u8>> = (0..s1)
            .map(|c| padded[(s0 + c) * chunk_len..(s0 + c + 1) * chunk_len].to_vec())
            .collect();
        let g0_par = test_rs_encode(&g0_srcs, r);
        let g1_par = test_rs_encode(&g1_srcs, r);

        let g0_pkts: Vec<Vec<u8>> = g0_srcs
            .iter()
            .enumerate()
            .map(|(c, src)| {
                build_2d_rsm_pkt(
                    VideoWireCodec::H264,
                    80,
                    2,
                    0,
                    s0,
                    r,
                    c + 1,
                    c + 1,
                    total_pkts,
                    0,
                    false,
                    0,
                    src,
                )
            })
            .chain(std::iter::once(build_2d_rsm_pkt(
                VideoWireCodec::H264,
                80,
                2,
                0,
                s0,
                r,
                s0 + 1,
                s0 + 1,
                total_pkts,
                0,
                false,
                0,
                &g0_par[0],
            )))
            .collect();
        let g1_pkts: Vec<Vec<u8>> = g1_srcs
            .iter()
            .enumerate()
            .map(|(c, src)| {
                build_2d_rsm_pkt(
                    VideoWireCodec::H264,
                    80,
                    2,
                    1,
                    s1,
                    r,
                    c + 1,
                    s0 + r + c + 1,
                    total_pkts,
                    0,
                    false,
                    0,
                    src,
                )
            })
            .chain(std::iter::once(build_2d_rsm_pkt(
                VideoWireCodec::H264,
                80,
                2,
                1,
                s1,
                r,
                s1 + 1,
                total_pkts,
                total_pkts,
                0,
                false,
                0,
                &g1_par[0],
            )))
            .collect();

        // 1. In-order group 0 (s=3) -> group 1 (s=2) source packets reassemble without drop.
        let mut rx = RsmVideoReassembler::new();
        let mut out = None;
        for pkt in g0_pkts[..s0].iter().chain(g1_pkts[..s1].iter()) {
            if let RsmIngestOutcome::Frame(f) = rx.ingest(pkt).unwrap() {
                out = Some(f);
            }
        }
        let f = out.expect("uneven multi-group P-frame must reassemble in order");
        assert!(!f.fec_recovered);
        assert!(!f.prior_frames_dropped);
        assert_eq!(f.payload, annexb);

        // 2. Out-of-order arrival: group 1 (s=2) arrives completely BEFORE group 0 (s=3);
        // must stay Pending until group 0 arrives, then emit exact payload.
        let mut rx_ooo = RsmVideoReassembler::new();
        for pkt in &g1_pkts[..s1] {
            assert_eq!(rx_ooo.ingest(pkt).unwrap(), RsmIngestOutcome::Pending);
        }
        let mut out_ooo = None;
        for pkt in &g0_pkts[..s0] {
            if let RsmIngestOutcome::Frame(f) = rx_ooo.ingest(pkt).unwrap() {
                out_ooo = Some(f);
            }
        }
        let f_ooo = out_ooo.expect("out-of-order uneven multi-group frame must reassemble");
        assert!(!f_ooo.prior_frames_dropped);
        assert_eq!(f_ooo.payload, annexb);

        // 3. Lose 1 source packet in trailing group 1 and recover via group 1's (s=2, r=1) parity.
        let mut rx_fec = RsmVideoReassembler::new();
        let mut out_fec = None;
        for pkt in [
            &g0_pkts[0],
            &g0_pkts[1],
            &g0_pkts[2],
            &g1_pkts[0], // g1_pkts[1] lost
            &g1_pkts[2], // g1 parity
        ] {
            if let RsmIngestOutcome::Frame(f) = rx_fec.ingest(pkt).unwrap() {
                out_fec = Some(f);
            }
        }
        let f_fec = out_fec.expect("trailing uneven group must recover lost packet via RS parity");
        assert!(f_fec.fec_recovered);
        assert!(!f_fec.prior_frames_dropped);
        assert_eq!(f_fec.payload, annexb);
    }

    #[test]
    fn two_dim_multi_group_full_first_group_loss_recovers_via_vertical_parity() {
        let mut annexb = vec![0, 0, 0, 1, 0x02, 0x01];
        for i in 0..300u16 {
            annexb.push(((i * 19 + 5) & 0xff) as u8);
        }
        let framed = frame_annexb_access_unit(VideoWireCodec::H265, &annexb).unwrap();
        let x_groups = 2usize;
        let s = 3usize;
        let y_groups = 1usize;
        let chunk_len = framed.bytes.len().div_ceil(x_groups * s);
        let mut padded = framed.bytes.clone();
        padded.resize(x_groups * s * chunk_len, 0);
        let total_pkts = x_groups * s + y_groups * s;

        let srcs: Vec<Vec<Vec<u8>>> = (0..x_groups)
            .map(|g| {
                (0..s)
                    .map(|col| {
                        padded[(g * s + col) * chunk_len..(g * s + col + 1) * chunk_len].to_vec()
                    })
                    .collect()
            })
            .collect();
        let v_par: Vec<Vec<u8>> = (0..s)
            .map(|col| {
                test_rs_encode(&[srcs[0][col].clone(), srcs[1][col].clone()], y_groups)
                    .pop()
                    .unwrap()
            })
            .collect();

        // All packets of group 0 are lost; only group 1 + vertical parity arrive.
        let mut rx = RsmVideoReassembler::new();
        let mut out = None;
        for (col, src) in srcs[1].iter().enumerate() {
            assert_eq!(
                rx.ingest(&build_2d_rsm_pkt(
                    VideoWireCodec::H265,
                    85,
                    x_groups,
                    1,
                    s,
                    0,
                    col + 1,
                    s + col + 1,
                    total_pkts,
                    y_groups,
                    false,
                    0,
                    src,
                ))
                .unwrap(),
                RsmIngestOutcome::Pending
            );
        }
        for (col, vp) in v_par.iter().enumerate() {
            if let RsmIngestOutcome::Frame(f) = rx
                .ingest(&build_2d_rsm_pkt(
                    VideoWireCodec::H265,
                    85,
                    x_groups,
                    0,
                    s,
                    0,
                    col + 1,
                    2 * s + col + 1,
                    total_pkts,
                    y_groups,
                    true,
                    0,
                    vp,
                ))
                .unwrap()
            {
                out = Some(f);
            }
        }
        let f = out.expect("2D vertical parity must recover completely missing group 0");
        assert!(f.fec_recovered);
        assert_eq!(f.payload, annexb);
    }

    #[test]
    fn multi_group_fec_p_frame_survives_reorder_across_adjacent_frame_without_false_drop() {
        let mut p1 = vec![0, 0, 0, 1, 0x41, 0x9a];
        p1.extend((0..320u16).map(|i| ((i * 3 + 1) & 0xff) as u8));
        let f1 = frame_annexb_access_unit(VideoWireCodec::H264, &p1).unwrap();
        // 5 source chunks across 2 horizontal groups: group 0 (s=3, r=1), group 1 (s=2, r=1).
        let chunk_len = f1.bytes.len().div_ceil(5);
        let mut pad1 = f1.bytes.clone();
        pad1.resize(5 * chunk_len, 0);

        let mut p2 = vec![0, 0, 0, 1, 0x41, 0x9a];
        p2.extend((0..80u8).map(|i| i.wrapping_mul(7).wrapping_add(5)));
        let f2 = frame_annexb_access_unit(VideoWireCodec::H264, &p2).unwrap();

        let mut rx = RsmVideoReassembler::new();
        for c in 0..3usize {
            assert_eq!(
                rx.ingest(&build_2d_rsm_pkt(
                    VideoWireCodec::H264,
                    60,
                    2,
                    0,
                    3,
                    1,
                    c + 1,
                    c + 1,
                    7,
                    0,
                    false,
                    0,
                    &pad1[c * chunk_len..(c + 1) * chunk_len]
                ))
                .unwrap(),
                RsmIngestOutcome::Pending
            );
        }
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                60,
                2,
                1,
                2,
                1,
                1,
                5,
                7,
                0,
                false,
                0,
                &pad1[3 * chunk_len..4 * chunk_len]
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );
        // Next frame 61 completes ahead of frame 60's trailing group-1 packet.
        assert_eq!(
            rx.ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                61,
                1,
                0,
                1,
                1,
                1,
                1,
                2,
                0,
                false,
                0,
                &f2.bytes
            ))
            .unwrap(),
            RsmIngestOutcome::Pending
        );
        let RsmIngestOutcome::Frame(out60) = rx
            .ingest(&build_2d_rsm_pkt(
                VideoWireCodec::H264,
                60,
                2,
                1,
                2,
                1,
                2,
                6,
                7,
                0,
                false,
                0,
                &pad1[4 * chunk_len..5 * chunk_len],
            ))
            .unwrap()
        else {
            panic!("reordered trailing packet of uneven multi-group FEC frame 60 must complete");
        };
        assert_eq!(out60.frame_index, 60);
        assert!(!out60.prior_frames_dropped);
        assert_eq!(out60.payload, p1);

        let out61 = rx.pop_ready().expect("buffered frame 61 must drain next");
        assert_eq!(out61.frame_index, 61);
        assert!(!out61.prior_frames_dropped);
        assert_eq!(out61.payload, p2);
    }

    #[test]
    fn parse_rsm_packet_header_decodes_media_2d_virtual_and_padding_packets() {
        let mut pktz = RsmVideoPacketizer::new(7, 2)
            .with_orientation(3)
            .with_chunk_bytes(100)
            .unwrap();
        let mut idr = vec![0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0, 0, 0, 1, 0x68, 0xee];
        idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
        idr.extend((0..240u16).map(|i| (i & 0xff) as u8));
        let pkts = pktz
            .packetize(VideoWireCodec::H264, &idr, 1234, 15, 500)
            .unwrap();
        assert!(pkts.len() >= 3);

        let h0 = parse_rsm_packet_header(&pkts[0]).unwrap();
        assert_eq!(h0.codec, VideoWireCodec::H264);
        assert_eq!(h0.member_id, 7);
        assert_eq!(h0.stream_id, 2);
        assert_eq!(h0.frame_index, 1);
        assert_eq!(h0.total_pkg_seq, 1);
        assert_eq!(h0.pkg_seq, Some(1));
        assert_eq!(h0.timestamp_ms, 1234);
        assert_eq!(h0.orientation, 3);
        assert!(h0.is_iframe);
        assert!(!h0.is_virtual_frame);
        assert_eq!(h0.virtual_frame_orientation, None);
        assert_eq!(h0.frame_count, 2);
        assert_eq!(h0.x_groups, 1);
        assert_eq!(h0.s_per_group as usize, pkts.len());
        assert_eq!(h0.r_per_group, 0);
        assert_eq!(h0.pkg_idx_in_group, 1);
        assert_eq!(h0.frame_pkt_idx, 1);
        assert_eq!(h0.frame_total_pkts as usize, pkts.len());

        // Virtual-frame metadata packet sets `is_virtual_frame`, extracts protobuf orientation,
        // advances `total_pkg_seq`, and leaves `pkg_seq = None`.
        let vf = pktz
            .build_orientation_metadata_packet(VideoWireCodec::H264, 1250, 15)
            .unwrap();
        let hvf = parse_rsm_packet_header(&vf).unwrap();
        assert!(hvf.is_virtual_frame);
        assert_eq!(hvf.virtual_frame_orientation, Some(3));
        assert_eq!(hvf.pkg_seq, None);
        assert_eq!(hvf.total_pkg_seq as usize, pkts.len() + 1);

        // Degenerate/padding packet (`f_tot_pkts == 0`) leaves `pkg_seq = None`.
        let mut padding = pkts[0].clone();
        padding[22] = 0;
        let hpad = parse_rsm_packet_header(&padding).unwrap();
        assert!(!hpad.is_virtual_frame);
        assert_eq!(hpad.pkg_seq, None);

        // 2D vertical parity packet parses `y_groups`, `is_vert_parity`, `vert_group_idx`, and `frame_index`.
        let vpkt = build_2d_rsm_pkt(
            VideoWireCodec::H265,
            300,
            2,
            0,
            4,
            1,
            2,
            10,
            12,
            1,
            true,
            0,
            &[0xaa; 32],
        );
        let hv = parse_rsm_packet_header(&vpkt).unwrap();
        assert_eq!(hv.codec, VideoWireCodec::H265);
        assert_eq!(hv.frame_index, 300);
        assert_eq!(hv.x_groups, 2);
        assert_eq!(hv.y_groups, 1);
        assert!(hv.is_vert_parity);
        assert_eq!(hv.vert_group_idx, 0);
        assert_eq!(hv.s_per_group, 4);
        assert_eq!(hv.r_per_group, 1);
        assert_eq!(hv.pkg_idx_in_group, 2);
        assert_eq!(hv.frame_pkt_idx, 10);
        assert_eq!(hv.frame_total_pkts, 12);
        assert_eq!(hv.payload_offset + 32, vpkt.len());

        // Truncated or non-kind-3 packets fail header validation cleanly.
        assert!(parse_rsm_packet_header(&[0u8; 12]).is_err());
        let mut bad_kind = pkts[0].clone();
        bad_kind[3] = (bad_kind[3] & !0x03) | 2;
        assert!(parse_rsm_packet_header(&bad_kind).is_err());
    }

    #[test]
    fn advance_seq16_handles_high_start_wraparound_and_reordering() {
        let seq = advance_seq16(None, 0xfffe);
        assert_eq!(seq, 0xfffe);
        // Out-of-order older sequence does not move backward.
        assert_eq!(advance_seq16(Some(seq), 0xfffc), 0xfffe);
        // Wraparound into 64-bit space advances monotonically.
        let seq = advance_seq16(Some(seq), 0xffff);
        assert_eq!(seq, 0xffff);
        let seq = advance_seq16(Some(seq), 0x0000);
        assert_eq!(seq, 0x1_0000);
        let seq = advance_seq16(Some(seq), 0x0005);
        assert_eq!(seq, 0x1_0005);
        // Late packet from before wraparound is ignored.
        assert_eq!(advance_seq16(Some(seq), 0xfffe), 0x1_0005);
    }

    #[test]
    fn annexb_keyframe_and_nalu_type_helpers_cover_h264_and_h265_irap() {
        let h264_idr = [
            0, 0, 0, 1, 0x67, 0x64, 0, 0, 0, 1, 0x68, 0xee, 0, 0, 1, 0x65, 0x88,
        ];
        let h264_p = [0, 0, 0, 1, 0x41, 0x9a];
        assert!(is_annexb_keyframe(VideoWireCodec::H264, &h264_idr));
        assert!(!is_annexb_keyframe(VideoWireCodec::H264, &h264_p));
        assert_eq!(
            annexb_nalu_types(VideoWireCodec::H264, &h264_idr),
            vec![7, 8, 5]
        );

        // HEVC BLA (NAL type 16) and IDR (19) are keyframes; TRAIL_R (1) is not.
        let h265_bla = [0, 0, 0, 1, 16 << 1, 0x01, 0xaa];
        let h265_idr = [0, 0, 0, 1, 32 << 1, 0x01, 0, 0, 0, 1, 19 << 1, 0x01, 0xbb];
        let h265_p = [0, 0, 0, 1, 1 << 1, 0x01, 0xcc];
        assert!(is_annexb_keyframe(VideoWireCodec::H265, &h265_bla));
        assert!(is_annexb_keyframe(VideoWireCodec::H265, &h265_idr));
        assert!(!is_annexb_keyframe(VideoWireCodec::H265, &h265_p));
        assert_eq!(
            annexb_nalu_types(VideoWireCodec::H265, &h265_idr),
            vec![32, 19]
        );
    }

    #[test]
    fn ingest_with_header_enforces_group_bounds_orientation_mask_and_payload_offset() {
        let mut pktz = RsmVideoPacketizer::new(7, 1).with_orientation(2);
        let idr = vec![0, 0, 0, 1, 0x65, 0x88, 0x11, 0x22, 0x33, 0x44];
        let pkts = pktz
            .packetize(VideoWireCodec::H264, &idr, 100, 15, 500)
            .unwrap();
        let valid_hdr = parse_rsm_packet_header(&pkts[0]).unwrap();
        let mut rx = RsmVideoReassembler::new();

        // Out-of-bounds payload_offset must return an error rather than panicking.
        let mut bad_off = valid_hdr;
        bad_off.payload_offset = pkts[0].len() + 1;
        assert!(rx.ingest_with_header(&pkts[0], &bad_off).is_err());

        // Out-of-range 3-bit group counts (`x_groups = 0/8/255`, `y_groups = 8/253`) must be ignored.
        for (x_groups, y_groups) in [(0, 0), (8, 0), (255, 0), (2, 8), (2, 253)] {
            let mut crafted = valid_hdr;
            crafted.x_groups = x_groups;
            crafted.y_groups = y_groups;
            assert_eq!(
                rx.ingest_with_header(&pkts[0], &crafted).unwrap(),
                RsmIngestOutcome::Pending
            );
        }

        // Unmasked synthetic orientation values are masked to 3 bits (`0..=7`).
        let mut masked_hdr = valid_hdr;
        masked_hdr.orientation = 0xfd; // 0xfd & 0x07 == 5
        let RsmIngestOutcome::Frame(frame) = rx.ingest_with_header(&pkts[0], &masked_hdr).unwrap()
        else {
            panic!("expected valid frame");
        };
        assert_eq!(frame.orientation, 5);
    }
}
