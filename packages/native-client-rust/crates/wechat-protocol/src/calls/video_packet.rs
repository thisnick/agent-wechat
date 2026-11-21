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
/// Default source payload chunk size per UDP video packet (leaves room for Live96/Live98 + RSM headers inside MTU).
pub const DEFAULT_RSM_CHUNK_BYTES: usize = 1100;
/// Default wire orientation code (`2` = upright 0° in `EncFecHeader` byte 13 bits 7..5).
pub const DEFAULT_RSM_ORIENTATION: u8 = 2;
const MAX_ACTIVE_ASSEMBLIES: usize = 8;
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
    let mut out = vec![0u8; 58];
    let local_packed = pack_rtcp_member(local_member, stream_id);
    let target_packed = pack_rtcp_member(target_member, stream_id);
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
    out[6 + 0x24..6 + 0x28].copy_from_slice(&sequence.to_le_bytes());
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

/// Encode a minimal `0x0b` MP P2S QoS heartbeat payload for `packed_kind = 0x30`.
pub fn encode_p2s(
    sequence: u16,
    cap_seq: u32,
    codec_support_mask: u32,
    decode_type: u32,
) -> Result<Vec<u8>> {
    let video_p2s = p::message(&[
        p::uint(24, u64::from(codec_support_mask)),
        p::uint(29, u64::from(decode_type)),
    ])?;
    let inner = p::message(&[p::bytes(3, &video_p2s)])?;
    let outer = p::message(&[
        p::uint(1, u64::from(sequence)),
        p::uint(2, u64::from(cap_seq)),
        p::bytes(3, &inner),
        p::uint(4, 2),
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
        self.pkg_seq = self.pkg_seq.wrapping_add(1);
        let bw_seq = self.bw_est_seq;
        self.bw_est_seq = if bw_seq == 255 { 1 } else { bw_seq + 1 };

        let frame_idx_lo = ((self.frame_idx & 0x1f) ^ 0x10) as u8;
        let mp_member_stream =
            (self.local_member & 0x7ff) | (u16::from(self.stream_id & 0x0f) << 11);
        let proto = p::message(&[
            p::uint(1, u64::from(fps.max(1))),
            p::uint(2, 10),
            p::uint(3, u64::from(self.orientation & 0x07)),
            p::uint(4, u64::from(self.frame_idx)),
        ])?;

        // 5-byte TLV extension matching official virtual-frame header (`ext_mask = 0x004c`).
        let ext: [u8; 5] = [0x00, 0x4c, 0x00, 0x08, bw_seq];
        let mut pkt = Vec::with_capacity(3 + 21 + ext.len() + 0x80 + proto.len());
        pkt.push(codec.wire());
        pkt.extend_from_slice(&mp_member_stream.to_le_bytes());

        pkt.push(3 | (frame_idx_lo << 3)); // hkind = 3, is_iframe = 0
        pkt.push(3 << 2); // pkg_idx_lo6 = 3, r_lo2 = 0 (virtual frame marker)
        pkt.extend_from_slice(&self.total_pkg_seq.to_be_bytes());
        pkt.extend_from_slice(&self.pkg_seq.to_be_bytes());
        pkt.extend_from_slice(&timestamp_ms.to_be_bytes());
        pkt.push(0);
        pkt.push(0);
        pkt.push(1 << 2); // s_lo6 = 1, frame_count - 1 = 0
        pkt.push(5 << 5); // workMode = 3 virtual-frame header code
        pkt.push(0x00);
        pkt.push(1); // x = 1
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
    s_per_group: usize,
    r_per_group: usize,
    y_groups: usize,
    xps_len_hi: u8,
    slice_len_hi: u8,
    /// Per-horizontal-group packets keyed by `slot_in_group` (`0..s` source, `s..s+r` parity).
    groups: Vec<BTreeMap<usize, Vec<u8>>>,
    /// 2D vertical parity packets keyed by `(vert_group_idx, col)` where `col in 0..s`.
    vert_parity: BTreeMap<(usize, usize), Vec<u8>>,
    arrival_order: u64,
}

impl PendingAssembly {
    fn source_chunks_complete(&self) -> bool {
        self.groups
            .iter()
            .all(|g| (0..self.s_per_group).all(|col| g.contains_key(&col)))
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
        if self.r_per_group == 0
            || self.s_per_group == 0
            || self.s_per_group + self.r_per_group > 255
        {
            return false;
        }
        let mut recovered = false;
        for group in &mut self.groups {
            let missing_src = (0..self.s_per_group)
                .filter(|col| !group.contains_key(col))
                .count();
            if missing_src == 0 || group.len() < self.s_per_group {
                continue;
            }
            if rs_recover_group(self.s_per_group, self.r_per_group, group) {
                recovered = true;
            }
        }
        recovered
    }

    fn recover_vertical_columns(&mut self) -> bool {
        if self.y_groups == 0 || self.x_groups < 2 || self.x_groups + self.y_groups > 255 {
            return false;
        }
        let mut recovered = false;
        for col in 0..self.s_per_group {
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
                            recovered = true;
                        }
                    }
                }
            }
        }
        recovered
    }
}

/// Reassembles MP/iLink kind-3 RSM video packets (including multi-group and 2D RS FEC)
/// into complete Annex-B frames.
pub struct RsmVideoReassembler {
    active: BTreeMap<u32, PendingAssembly>,
    last_completed: Option<u32>,
    arrival_counter: u64,
    vf_orientation: Option<u8>,
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
            active: BTreeMap::new(),
            last_completed: None,
            arrival_counter: 0,
            vf_orientation: None,
        }
    }

    /// Clear all pending assemblies (for example after a remote camera toggle).
    pub fn reset(&mut self) {
        self.active.clear();
        self.last_completed = None;
        self.vf_orientation = None;
    }

    /// Feed one decrypted MP/iLink RSM video packet (`[codecType, mid/sid, EncFecHeader, ...]`).
    pub fn ingest(&mut self, packet: &[u8]) -> Result<RsmIngestOutcome> {
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
        let timestamp_ms = u32::from_be_bytes([hdr[6], hdr[7], hdr[8], hdr[9]]);
        let frame_count = (hdr[12] & 0x03) + 1;
        let s_lo6 = usize::from(hdr[12] >> 2);
        let orientation = hdr[13] >> 5;
        let r_mid2 = u32::from(hdr[13] & 0x03);
        let bw_probe = (hdr[13] & 0x10) != 0;
        let r_hi4 = u32::from(hdr[14] & 0x0f);
        let r_full = (r_lo2 | (r_mid2 << 2) | (r_hi4 << 4)) as usize;
        let group_idx_x = usize::from(hdr[14] >> 5);
        let x_groups = usize::from(hdr[15] & 0x07).max(1);

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
        let mut y_groups = 0usize;
        let mut is_vert_parity = false;
        let mut vert_group_idx = 0usize;

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
                y_groups = usize::from(b & 0x07);
                is_vert_parity = (b & 0x08) != 0;
                vert_group_idx = usize::from((b >> 4) & 0x07);
                pos += 1;
            }
            let _ = pos;
        }

        let s_per_group = s_lo6 + s_ovf_lo + (s_ovf_hi << 8);
        let pkg_idx_in_group = pkg_idx_lo6 + pkg_ovf_lo + (pkg_ovf_hi << 8);
        let f_pkt_idx = f_pkt_lo + (f_pkt_hi << 8);
        let f_tot_pkts = f_tot_lo + (f_tot_hi << 8);
        let payload = &hdr[21 + ext_len..];

        // Virtual-frame metadata packets (`s = 1, r = 0, x = 1, pkg_idx = 3`) carry the
        // `workMode = 3` protobuf header (including `field 3 = orientation`) after a
        // 128-byte leading block. Parse orientation before skipping non-media packets.
        if s_per_group == 1 && r_full == 0 && x_groups == 1 && pkg_idx_in_group == 3 {
            if payload.len() > 0x80 {
                if let Ok(fields) = p::decode(&payload[0x80..]) {
                    if let Ok(Some(vf_orient)) = opt_u32(&fields, 3) {
                        self.vf_orientation = Some((vf_orient & 0x07) as u8);
                    }
                }
            }
            return Ok(RsmIngestOutcome::Pending);
        }

        // Skip bandwidth-probe padding packets and degenerate headers.
        if bw_probe || f_tot_pkts == 0 || f_pkt_idx == 0 || s_per_group == 0 {
            return Ok(RsmIngestOutcome::Pending);
        }
        let total_source_pkts = x_groups.saturating_mul(s_per_group);
        if total_source_pkts == 0
            || total_source_pkts > MAX_FRAGMENTS_PER_FRAME
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

        let full_frame_idx = (frame_idx_hi << 5) | frame_idx_lo;
        // Drop trailing FEC parity / late duplicate packets belonging to any already-emitted
        // frame (within a 256-frame modular window) so they never re-emit an older frame
        // out of order via RS recovery or re-insert a zombie assembly that triggers false
        // frame-drop IRequests. Allow a clean encoder restart (IDR with low frame_idx).
        if let Some(last) = self.last_completed {
            let encoder_reset = is_iframe && full_frame_idx <= 4 && last > 32;
            let lag = last.wrapping_sub(full_frame_idx) & 0xffff;
            if !encoder_reset && lag < 256 {
                return Ok(RsmIngestOutcome::Pending);
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
            }
        }

        self.arrival_counter = self.arrival_counter.wrapping_add(1);
        let arrival = self.arrival_counter;
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
                s_per_group,
                r_per_group: r_full,
                y_groups,
                xps_len_hi,
                slice_len_hi,
                groups: (0..x_groups).map(|_| BTreeMap::new()).collect(),
                vert_parity: BTreeMap::new(),
                arrival_order: arrival,
            });
        // Reject inconsistent stream/group geometry for the same frame index.
        if entry.stream_id != stream_id
            || entry.x_groups != x_groups
            || entry.s_per_group != s_per_group
        {
            return Ok(RsmIngestOutcome::Pending);
        }
        entry.r_per_group = entry.r_per_group.max(r_full);
        entry.y_groups = entry.y_groups.max(y_groups);
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

        let mut fec_recovered = false;
        if !entry.source_chunks_complete() {
            fec_recovered = entry.try_fec_recover();
        }
        if !entry.source_chunks_complete() {
            return Ok(if dropped {
                RsmIngestOutcome::FrameDroppedNeedKeyframe
            } else {
                RsmIngestOutcome::Pending
            });
        }

        let completed = self.active.remove(&full_frame_idx).expect("just inserted");
        self.last_completed = Some(full_frame_idx);
        // Drop any strictly older stalled assemblies (16-bit wraparound-safe) so latency
        // never accumulates.
        let before = self.active.len();
        self.active.retain(|&idx, _| {
            let older = {
                let diff = full_frame_idx.wrapping_sub(idx) & 0xffff;
                diff > 0 && diff < 0x8000
            };
            !older
        });
        let evicted_older = self.active.len() < before;

        let total_bytes: usize = completed
            .groups
            .iter()
            .flat_map(|g| (0..completed.s_per_group).filter_map(|col| g.get(&col)))
            .map(Vec::len)
            .sum();
        ensure!(
            total_bytes <= MAX_VIDEO_FRAME_BYTES + 4096,
            "Reassembled video frame exceeds budget"
        );
        let mut assembled = Vec::with_capacity(total_bytes);
        for group in &completed.groups {
            for col in 0..completed.s_per_group {
                if let Some(chunk) = group.get(&col) {
                    assembled.extend_from_slice(chunk);
                }
            }
        }

        let (frame_payload, is_keyframe, has_codec_config) = match completed.codec {
            VideoWireCodec::H264 | VideoWireCodec::H265 => unframe_annexb_access_unit_with_count(
                completed.codec,
                &assembled,
                completed.frame_count,
                completed.xps_len_hi,
                completed.slice_len_hi,
            )?,
            _ => (assembled, completed.is_iframe, false),
        };

        Ok(RsmIngestOutcome::Frame(ReassembledVideoFrame {
            codec: completed.codec,
            member_id: completed.member_id,
            stream_id: completed.stream_id,
            frame_index: full_frame_idx,
            timestamp_ms: completed.timestamp_ms,
            orientation: self.vf_orientation.unwrap_or(completed.orientation),
            prior_frames_dropped: dropped || evicted_older,
            fec_recovered,
            is_keyframe: is_keyframe || completed.is_iframe,
            has_codec_config,
            payload: frame_payload,
        }))
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

        let p2s = encode_p2s(5, 2, 0x10, 15).unwrap();
        assert_eq!(p2s[0], 0x0b);
        let decoded = p::decode(&p2s[1..]).unwrap();
        assert_eq!(opt_u32(&decoded, 1).unwrap(), Some(5));
        assert_eq!(opt_u32(&decoded, 4).unwrap(), Some(2));
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
        let r_lo2 = (r & 0x03) as u8;
        let r_mid2 = ((r >> 2) & 0x03) as u8;
        let r_hi4 = ((r >> 4) & 0x0f) as u8;
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

        // s = 1, r = 4 (r_lo2 = 0, r_mid2 = 1): pkg_idx = 3 is the second RS parity packet,
        // NOT a virtual-frame metadata packet (which requires r_full == 0).
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
}
