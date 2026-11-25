//! Rewrap WeChat "Vcodec1" video (RSM `codecType = 0x00`) as standard H.264 Annex-B.
//!
//! Vcodec1 is H.264 (CABAC or CAVLC, 4x4 transform) in WeChat's own packaging. Official
//! senders switch to it when the server's video QoS selects codec type 1. A frame is
//! WeChat's 4-byte framing prefix `[len_be16, 0, 0]` followed by units, each introduced by
//! `00 01 00 01` and a one-byte header `(ref_idc << 6) | (unit_type << 1)`:
//!
//! - unit type 1: a compact merged sequence/picture parameter header;
//! - unit type 2: an intra slice (an IDR picture);
//! - unit type 3: an inter (P) slice, optionally naming its reference by frame distance.
//!
//! Units carry no `0x03` emulation-prevention bytes. Slice data uses standard H.264
//! entropy coding, so this module only rebuilds the parameter sets and slice headers and
//! re-escapes the payload; the picture data is copied unchanged.
//!
//! Two reconstruction details differ from H.264 (one luma sub-pel interpolation position
//! and the strongest luma deblocking filter), so a stock decoder's P-frames can drift
//! slightly from the sender's until the next intra frame. Mixed-resolution "MixVcodec1"
//! (`codecType = 0x11`) is not handled.

use anyhow::{bail, ensure, Context, Result};
use std::collections::BTreeMap;

/// Separator before each Vcodec1 unit.
const UNIT_SEPARATOR: [u8; 4] = [0x00, 0x01, 0x00, 0x01];
/// Vcodec1 frame numbers wrap at 252.
const VCODEC1_FRAME_NUM_MODULO: u32 = 252;
/// Reference frames declared in the synthesized SPS; covers hierarchical-P distances.
const MAX_NUM_REF_FRAMES: u32 = 16;
/// Recent reference pictures remembered for distance-based references.
const MAX_TRACKED_REFS: usize = 64;
/// Largest picture accepted from a Vcodec1 header (in macroblocks per side).
const MAX_MBS_PER_SIDE: u32 = 256;

/// One Vcodec1 frame rewrapped as an H.264 Annex-B access unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConvertedFrame {
    /// Standard H.264 Annex-B bytes (SPS/PPS when present, then slice NAL units).
    pub annexb: Vec<u8>,
    /// True when the frame is an IDR picture.
    pub is_keyframe: bool,
    /// True when the frame carried a parameter header (SPS/PPS were emitted).
    pub has_codec_config: bool,
}

/// Fields of the Vcodec1 parameter header that the H.264 rewrap needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    log2_max_frame_num_minus4: u32,
    width_mbs_minus1: u32,
    height_mbs_minus1: u32,
    cabac: bool,
    pic_init_qp_minus26: i32,
    chroma_qp_index_offset: i32,
    disable_deblocking: bool,
}

/// One parsed Vcodec1 slice unit.
#[derive(Debug)]
struct Slice<'a> {
    nal_ref_idc: u8,
    first_mb: u32,
    intra: bool,
    frame_num: u32,
    ref_delta: Option<u32>,
    cabac_init_idc: u32,
    slice_qp_delta: i32,
    /// Slice data after the Vcodec1 slice header.
    data: SliceData<'a>,
}

#[derive(Debug)]
enum SliceData<'a> {
    /// CABAC data starts on a byte boundary.
    Aligned(&'a [u8]),
    /// CAVLC data continues from `bit_offset` within `bytes`.
    Unaligned { bytes: &'a [u8], bit_offset: usize },
}

/// Stateful per-stream converter: parameter header, IDR numbering and the mapping from
/// Vcodec1 reference frame numbers to the synthesized H.264 frame numbers.
#[derive(Debug, Default)]
pub struct Vcodec1Converter {
    header: Option<Header>,
    idr_pic_id: u32,
    prev_ref_frame_num: u32,
    refs: BTreeMap<u32, u32>,
    ref_order: Vec<u32>,
}

impl Vcodec1Converter {
    /// Create a converter with no parameter header yet; P-frames fail until a keyframe
    /// carrying the header arrives.
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert one reassembled Vcodec1 frame (including WeChat's 4-byte framing prefix).
    pub fn convert_frame(&mut self, framed: &[u8]) -> Result<ConvertedFrame> {
        let units = split_units(framed)?;
        ensure!(!units.is_empty(), "Vcodec1 frame has no units");
        let mut out = Vec::with_capacity(framed.len() + 64);
        let mut has_codec_config = false;
        let mut picture: Option<PictureNumbers> = None;
        let mut is_keyframe = false;
        for unit in units {
            ensure!(!unit.is_empty(), "Empty Vcodec1 unit");
            match (unit[0] >> 1) & 0x1f {
                1 => {
                    let header = parse_header(&unit[1..])?;
                    self.header = Some(header);
                    out.extend(build_sps(&header));
                    out.extend(build_pps(&header));
                    has_codec_config = true;
                }
                2 | 3 => {
                    let header = self
                        .header
                        .context("Vcodec1 slice before any parameter header")?;
                    let slice = parse_slice(unit, &header)?;
                    let numbers = match picture {
                        Some(numbers) => numbers,
                        None => {
                            let numbers = self.number_picture(&slice, &header);
                            picture = Some(numbers);
                            numbers
                        }
                    };
                    is_keyframe |= slice.intra;
                    out.extend(build_slice(&slice, &header, numbers)?);
                }
                other => bail!("Unsupported Vcodec1 unit type {other}"),
            }
        }
        ensure!(
            picture.is_some() || has_codec_config,
            "Vcodec1 frame has no slices"
        );
        Ok(ConvertedFrame {
            annexb: out,
            is_keyframe,
            has_codec_config,
        })
    }

    /// Assign H.264 numbering to the picture whose first slice is `slice`.
    fn number_picture(&mut self, slice: &Slice<'_>, header: &Header) -> PictureNumbers {
        let mask = (1u32 << (header.log2_max_frame_num_minus4 + 4)) - 1;
        if slice.intra {
            self.refs.clear();
            self.ref_order.clear();
            let idr_pic_id = self.idr_pic_id;
            self.idr_pic_id = (self.idr_pic_id + 1) & 0xffff;
            self.prev_ref_frame_num = 0;
            self.remember_ref(slice.frame_num, 0);
            return PictureNumbers {
                frame_num: 0,
                idr_pic_id,
                ref_pic_num_diff: None,
            };
        }
        let frame_num = (self.prev_ref_frame_num + 1) & mask;
        let ref_pic_num_diff = slice.ref_delta.and_then(|delta| {
            let target = (slice.frame_num + VCODEC1_FRAME_NUM_MODULO
                - delta % VCODEC1_FRAME_NUM_MODULO)
                % VCODEC1_FRAME_NUM_MODULO;
            self.refs
                .get(&target)
                .map(|&ref_frame_num| frame_num.wrapping_sub(ref_frame_num) & mask)
        });
        if slice.nal_ref_idc != 0 {
            self.prev_ref_frame_num = frame_num;
            self.remember_ref(slice.frame_num, frame_num);
        }
        PictureNumbers {
            frame_num,
            idr_pic_id: 0,
            ref_pic_num_diff,
        }
    }

    fn remember_ref(&mut self, vcodec1_frame_num: u32, h264_frame_num: u32) {
        if self
            .refs
            .insert(vcodec1_frame_num, h264_frame_num)
            .is_none()
        {
            self.ref_order.push(vcodec1_frame_num);
        }
        while self.ref_order.len() > MAX_TRACKED_REFS {
            let oldest = self.ref_order.remove(0);
            self.refs.remove(&oldest);
        }
    }
}

/// H.264 numbering chosen for one picture (shared by all its slices).
#[derive(Clone, Copy, Debug)]
struct PictureNumbers {
    frame_num: u32,
    idr_pic_id: u32,
    /// `CurrPicNum - PicNum(reference)` when the slice names a reference by distance.
    ref_pic_num_diff: Option<u32>,
}

/// Split a framed Vcodec1 frame into units (each starting with its header byte).
fn split_units(framed: &[u8]) -> Result<Vec<&[u8]>> {
    ensure!(framed.len() >= 4, "Truncated Vcodec1 frame prefix");
    let declared = usize::from(u16::from_be_bytes([framed[0], framed[1]]));
    ensure!(
        4 + declared <= framed.len(),
        "Vcodec1 frame shorter than its declared length"
    );
    let body = &framed[4..4 + declared];
    let starts: Vec<usize> = body
        .windows(UNIT_SEPARATOR.len())
        .enumerate()
        .filter(|(_, window)| *window == UNIT_SEPARATOR)
        .map(|(index, _)| index)
        .collect();
    ensure!(
        starts.first() == Some(&0),
        "Vcodec1 frame does not start with a unit separator"
    );
    let mut units = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let begin = start + UNIT_SEPARATOR.len();
        let end = starts.get(n + 1).copied().unwrap_or(body.len());
        if begin < end {
            units.push(&body[begin..end]);
        }
    }
    Ok(units)
}

fn parse_header(bits: &[u8]) -> Result<Header> {
    let mut r = BitReader::new(bits);
    let _seq_id = r.ue()?;
    r.skip(4)?;
    let log2_max_frame_num_minus4 = r.ue()?;
    ensure!(
        log2_max_frame_num_minus4 <= 12,
        "Invalid Vcodec1 frame number width"
    );
    if r.bit()? {
        r.ue()?;
    }
    r.skip(1)?;
    let width_mbs_minus1 = r.ue()?;
    let height_mbs_minus1 = r.ue()?;
    ensure!(
        width_mbs_minus1 < MAX_MBS_PER_SIDE && height_mbs_minus1 < MAX_MBS_PER_SIDE,
        "Vcodec1 picture size out of range"
    );
    r.skip(1)?;
    let entropy = r.bits(2)?;
    r.skip(6)?;
    let pic_init_qp_minus26 = r.se()?;
    let chroma_qp_index_offset = r.se()?;
    ensure!(
        (-26..=25).contains(&pic_init_qp_minus26) && (-12..=12).contains(&chroma_qp_index_offset),
        "Vcodec1 quantizer settings out of range"
    );
    r.skip(2)?;
    r.se()?;
    r.se()?;
    r.skip(1)?;
    r.ue()?;
    let disable_deblocking = r.bit()?;
    Ok(Header {
        log2_max_frame_num_minus4,
        width_mbs_minus1,
        height_mbs_minus1,
        cabac: entropy == 2,
        pic_init_qp_minus26,
        chroma_qp_index_offset,
        disable_deblocking,
    })
}

fn parse_slice<'a>(unit: &'a [u8], header: &Header) -> Result<Slice<'a>> {
    let nal_ref_idc = unit[0] >> 6;
    let body = &unit[1..];
    let mut r = BitReader::new(body);
    let first_mb = r.ue()?;
    let slice_type = r.ue()?;
    ensure!(
        slice_type <= 1,
        "Unsupported Vcodec1 slice type {slice_type}"
    );
    let intra = slice_type == 1;
    let frame_num = r.bits(header.log2_max_frame_num_minus4 + 4)?;
    r.ue()?;
    if intra && r.ue()? == 1 {
        r.skip(1)?;
    }
    let mut ref_delta = None;
    if !intra && r.ue()? == 0 {
        ref_delta = Some(r.ue()? + 1);
    }
    let cabac_init_idc = if header.cabac && !intra { r.ue()? } else { 0 };
    ensure!(cabac_init_idc <= 2, "Invalid Vcodec1 CABAC init index");
    let slice_qp_delta = r.se()?;
    r.skip(1)?;
    let data = if header.cabac {
        SliceData::Aligned(&body[r.position().div_ceil(8)..])
    } else {
        SliceData::Unaligned {
            bytes: body,
            bit_offset: r.position(),
        }
    };
    Ok(Slice {
        nal_ref_idc,
        first_mb,
        intra,
        frame_num,
        ref_delta,
        cabac_init_idc,
        slice_qp_delta,
        data,
    })
}

fn build_sps(header: &Header) -> Vec<u8> {
    let mut w = BitWriter::default();
    // Main profile for CABAC, constrained Baseline for CAVLC; level 4.1 leaves room for
    // the declared reference frames at up to 720p.
    let (profile_idc, constraints) = if header.cabac { (77, 0x40) } else { (66, 0xc0) };
    w.bits(profile_idc, 8);
    w.bits(constraints, 8);
    w.bits(41, 8);
    w.ue(0);
    w.ue(header.log2_max_frame_num_minus4);
    w.ue(2); // pic_order_cnt_type: output order follows decoding order
    w.ue(MAX_NUM_REF_FRAMES);
    w.bit(true); // gaps_in_frame_num_value_allowed_flag
    w.ue(header.width_mbs_minus1);
    w.ue(header.height_mbs_minus1);
    w.bit(true); // frame_mbs_only_flag
    w.bit(true); // direct_8x8_inference_flag
    w.bit(false); // frame_cropping_flag
    w.bit(false); // vui_parameters_present_flag
    w.rbsp_trailing();
    nal(0x67, &w.into_bytes())
}

fn build_pps(header: &Header) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.ue(0);
    w.ue(0);
    w.bit(header.cabac);
    w.bit(false); // bottom_field_pic_order_in_frame_present_flag
    w.ue(0); // num_slice_groups_minus1
    w.ue(0); // num_ref_idx_l0_default_active_minus1
    w.ue(0); // num_ref_idx_l1_default_active_minus1
    w.bit(false); // weighted_pred_flag
    w.bits(0, 2); // weighted_bipred_idc
    w.se(header.pic_init_qp_minus26);
    w.se(header.pic_init_qp_minus26); // pic_init_qs_minus26
    w.se(header.chroma_qp_index_offset);
    w.bit(true); // deblocking_filter_control_present_flag
    w.bit(false); // constrained_intra_pred_flag
    w.bit(false); // redundant_pic_cnt_present_flag
    w.rbsp_trailing();
    nal(0x68, &w.into_bytes())
}

fn build_slice(slice: &Slice<'_>, header: &Header, numbers: PictureNumbers) -> Result<Vec<u8>> {
    let mut w = BitWriter::default();
    w.ue(slice.first_mb);
    w.ue(if slice.intra { 7 } else { 5 });
    w.ue(0); // pic_parameter_set_id
    w.bits(numbers.frame_num, header.log2_max_frame_num_minus4 + 4);
    if slice.intra {
        w.ue(numbers.idr_pic_id);
    } else {
        w.bit(false); // num_ref_idx_active_override_flag
        match numbers.ref_pic_num_diff {
            Some(diff) if diff > 1 => {
                w.bit(true); // ref_pic_list_modification_flag_l0
                w.ue(0); // subtract from the predicted picture number
                w.ue(diff - 1);
                w.ue(3); // end of modifications
            }
            _ => w.bit(false),
        }
    }
    let nal_ref_idc = if slice.intra {
        3
    } else {
        slice.nal_ref_idc.min(3)
    };
    if nal_ref_idc != 0 {
        if slice.intra {
            w.bit(false); // no_output_of_prior_pics_flag
            w.bit(false); // long_term_reference_flag
        } else {
            w.bit(false); // adaptive_ref_pic_marking_mode_flag: sliding window
        }
    }
    if header.cabac && !slice.intra {
        w.ue(slice.cabac_init_idc);
    }
    w.se(slice.slice_qp_delta);
    w.ue(u32::from(header.disable_deblocking));
    if !header.disable_deblocking {
        w.se(0); // slice_alpha_c0_offset_div2
        w.se(0); // slice_beta_offset_div2
    }
    match slice.data {
        SliceData::Aligned(data) => {
            while !w.is_aligned() {
                w.bit(true); // cabac_alignment_one_bit
            }
            let mut rbsp = w.into_bytes();
            rbsp.extend_from_slice(data);
            let nal_type = if slice.intra { 5 } else { 1 };
            Ok(nal((nal_ref_idc << 5) | nal_type, &rbsp))
        }
        SliceData::Unaligned { bytes, bit_offset } => {
            // Copy the slice data bits up to and including the RBSP stop bit, then pad.
            let total_bits = bytes.len() * 8;
            let stop_bit = (bit_offset..total_bits)
                .rev()
                .find(|&bit| bytes[bit / 8] & (0x80 >> (bit % 8)) != 0)
                .context("Vcodec1 CAVLC slice has no stop bit")?;
            for bit in bit_offset..=stop_bit {
                w.bit(bytes[bit / 8] & (0x80 >> (bit % 8)) != 0);
            }
            while !w.is_aligned() {
                w.bit(false);
            }
            let nal_type = if slice.intra { 5 } else { 1 };
            Ok(nal((nal_ref_idc << 5) | nal_type, &w.into_bytes()))
        }
    }
}

/// Wrap an RBSP as an Annex-B NAL unit, inserting emulation-prevention bytes.
fn nal(header: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + rbsp.len() / 64 + 5);
    out.extend_from_slice(&[0, 0, 0, 1, header]);
    let mut zeros = 0;
    for &byte in rbsp {
        if zeros == 2 && byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    out
}

struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn position(&self) -> usize {
        self.position
    }

    fn bit(&mut self) -> Result<bool> {
        let byte = *self
            .data
            .get(self.position / 8)
            .context("Vcodec1 header truncated")?;
        let bit = byte & (0x80 >> (self.position % 8)) != 0;
        self.position += 1;
        Ok(bit)
    }

    fn bits(&mut self, count: u32) -> Result<u32> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.bit()?);
        }
        Ok(value)
    }

    fn skip(&mut self, count: u32) -> Result<()> {
        self.bits(count).map(|_| ())
    }

    fn ue(&mut self) -> Result<u32> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            ensure!(zeros <= 31, "Invalid Vcodec1 Exp-Golomb code");
        }
        if zeros == 0 {
            return Ok(0);
        }
        Ok((1u32 << zeros) - 1 + self.bits(zeros)?)
    }

    fn se(&mut self) -> Result<i32> {
        let code = self.ue()?;
        let magnitude = i32::try_from(code.div_ceil(2))?;
        Ok(if code % 2 == 1 { magnitude } else { -magnitude })
    }
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bit_count: usize,
}

impl BitWriter {
    fn bit(&mut self, value: bool) {
        if self.bit_count.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if value {
            let last = self.bytes.len() - 1;
            self.bytes[last] |= 0x80 >> (self.bit_count % 8);
        }
        self.bit_count += 1;
    }

    fn bits(&mut self, value: u32, count: u32) {
        for shift in (0..count).rev() {
            self.bit((value >> shift) & 1 == 1);
        }
    }

    fn ue(&mut self, value: u32) {
        let code = u64::from(value) + 1;
        let length = 64 - code.leading_zeros();
        for _ in 1..length {
            self.bit(false);
        }
        for shift in (0..length).rev() {
            self.bit((code >> shift) & 1 == 1);
        }
    }

    fn se(&mut self, value: i32) {
        let code = if value > 0 {
            (value as u32) * 2 - 1
        } else {
            value.unsigned_abs() * 2
        };
        self.ue(code);
    }

    fn is_aligned(&self) -> bool {
        self.bit_count.is_multiple_of(8)
    }

    fn rbsp_trailing(&mut self) {
        self.bit(true);
        while !self.is_aligned() {
            self.bit(false);
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parameter-header unit seen from an official Android sender (320x176, CABAC, QP 27).
    const OFFICIAL_HEADER: [u8; 8] = [0xc2, 0x81, 0x40, 0xa0, 0xbc, 0x02, 0xda, 0x10];

    fn frame(units: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        for unit in units {
            body.extend_from_slice(&UNIT_SEPARATOR);
            body.extend_from_slice(unit);
        }
        let mut out = (body.len() as u16).to_be_bytes().to_vec();
        out.extend_from_slice(&[0, 0]);
        out.extend(body);
        out
    }

    /// Build a Vcodec1 CABAC slice unit per the documented slice-header syntax.
    fn slice_unit(
        ref_idc: u8,
        intra: bool,
        frame_num: u32,
        ref_delta: Option<u32>,
        qp_delta: i32,
        data: &[u8],
    ) -> Vec<u8> {
        let unit_type = if intra { 2 } else { 3 };
        let mut w = BitWriter::default();
        w.ue(0); // first_mb
        w.ue(u32::from(intra)); // inverted slice type: 1 = I, 0 = P
        w.bits(frame_num, 8);
        w.ue(0);
        if intra {
            w.ue(0);
        } else {
            match ref_delta {
                Some(delta) => {
                    w.ue(0);
                    w.ue(delta - 1);
                }
                None => w.ue(1),
            }
            w.ue(0); // cabac_init_idc
        }
        w.se(qp_delta);
        w.bit(false);
        while !w.is_aligned() {
            w.bit(false);
        }
        let mut unit = vec![(ref_idc << 6) | (unit_type << 1)];
        unit.extend(w.into_bytes());
        unit.extend_from_slice(data);
        unit
    }

    fn header_unit() -> Vec<u8> {
        OFFICIAL_HEADER.to_vec()
    }

    /// Split Annex-B output into NAL units (header byte first, escapes removed).
    fn nal_units(annexb: &[u8]) -> Vec<Vec<u8>> {
        let mut units = Vec::new();
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 4 <= annexb.len() {
            if annexb[i..i + 4] == [0, 0, 0, 1] {
                starts.push(i + 4);
                i += 4;
            } else {
                i += 1;
            }
        }
        for (n, &start) in starts.iter().enumerate() {
            let end = starts.get(n + 1).map_or(annexb.len(), |next| next - 4);
            let mut unit = Vec::new();
            let mut zeros = 0;
            for &byte in &annexb[start..end] {
                if zeros == 2 && byte == 3 {
                    zeros = 0;
                    continue;
                }
                unit.push(byte);
                zeros = if byte == 0 { zeros + 1 } else { 0 };
            }
            units.push(unit);
        }
        units
    }

    #[test]
    fn official_header_parses_as_320x176_cabac() {
        let header = parse_header(&OFFICIAL_HEADER[1..]).unwrap();
        assert_eq!((header.width_mbs_minus1 + 1) * 16, 320);
        assert_eq!((header.height_mbs_minus1 + 1) * 16, 176);
        assert!(header.cabac);
        assert_eq!(header.pic_init_qp_minus26, 1);
        assert_eq!(header.log2_max_frame_num_minus4, 4);
    }

    #[test]
    fn keyframe_becomes_sps_pps_and_idr_with_payload_escaped() {
        let mut converter = Vcodec1Converter::new();
        // Payload containing a start-code pattern that must be escaped.
        let payload = [0xab, 0x00, 0x00, 0x01, 0xcd];
        let converted = converter
            .convert_frame(&frame(&[
                header_unit(),
                slice_unit(3, true, 1, None, 2, &payload),
            ]))
            .unwrap();
        assert!(converted.is_keyframe);
        assert!(converted.has_codec_config);
        let nals = nal_units(&converted.annexb);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0][0], 0x67);
        assert_eq!(nals[0][1], 77, "Main profile for CABAC");
        assert_eq!(nals[1][0], 0x68);
        assert_eq!(nals[2][0], 0x65, "IDR slice");
        let mut r = BitReader::new(&nals[2][1..]);
        assert_eq!(r.ue().unwrap(), 0, "first_mb_in_slice");
        assert_eq!(r.ue().unwrap(), 7, "I slice");
        assert_eq!(r.ue().unwrap(), 0, "pps id");
        assert_eq!(r.bits(8).unwrap(), 0, "IDR frame_num");
        assert_eq!(r.ue().unwrap(), 0, "idr_pic_id");
        assert!(!r.bit().unwrap(), "no_output_of_prior_pics");
        assert!(!r.bit().unwrap(), "long_term_reference");
        assert_eq!(r.se().unwrap(), 2, "slice_qp_delta");
        // The escaped payload survives byte-for-byte at the end of the slice.
        assert!(nals[2].ends_with(&payload));
        assert!(converted
            .annexb
            .windows(4)
            .any(|w| w == [0x00, 0x00, 0x03, 0x01]));
    }

    #[test]
    fn p_slices_number_frames_and_name_distant_references() {
        let mut converter = Vcodec1Converter::new();
        converter
            .convert_frame(&frame(&[
                header_unit(),
                slice_unit(3, true, 10, None, 0, &[1]),
            ]))
            .unwrap();
        // Reference P-frame at Vcodec1 frame 11, then a non-reference at 12.
        let p1 = converter
            .convert_frame(&frame(&[slice_unit(2, false, 11, None, 0, &[2])]))
            .unwrap();
        assert!(!p1.is_keyframe && !p1.has_codec_config);
        let nals = nal_units(&p1.annexb);
        assert_eq!(nals[0][0], 0x41, "reference P slice");
        let mut r = BitReader::new(&nals[0][1..]);
        r.ue().unwrap();
        assert_eq!(r.ue().unwrap(), 5, "P slice");
        r.ue().unwrap();
        assert_eq!(r.bits(8).unwrap(), 1, "frame_num follows the IDR");
        assert!(!r.bit().unwrap(), "no ref count override");
        assert!(!r.bit().unwrap(), "default reference list");
        assert!(!r.bit().unwrap(), "sliding-window marking");

        let p2 = converter
            .convert_frame(&frame(&[slice_unit(0, false, 12, None, 0, &[3])]))
            .unwrap();
        let nals = nal_units(&p2.annexb);
        assert_eq!(nals[0][0], 0x01, "non-reference P slice");
        let mut r = BitReader::new(&nals[0][1..]);
        r.ue().unwrap();
        r.ue().unwrap();
        r.ue().unwrap();
        assert_eq!(r.bits(8).unwrap(), 2);
        assert!(!r.bit().unwrap());
        assert!(!r.bit().unwrap());
        assert_eq!(
            r.ue().unwrap(),
            0,
            "cabac_init_idc follows directly: no marking"
        );

        // A reference P-frame at Vcodec1 frame 13 predicting from frame 10 (distance 3).
        let p3 = converter
            .convert_frame(&frame(&[slice_unit(2, false, 13, Some(3), 0, &[4])]))
            .unwrap();
        let nals = nal_units(&p3.annexb);
        let mut r = BitReader::new(&nals[0][1..]);
        r.ue().unwrap();
        r.ue().unwrap();
        r.ue().unwrap();
        assert_eq!(
            r.bits(8).unwrap(),
            2,
            "non-reference frame did not advance frame_num"
        );
        assert!(!r.bit().unwrap());
        assert!(r.bit().unwrap(), "explicit reference");
        assert_eq!(r.ue().unwrap(), 0, "subtract");
        assert_eq!(
            r.ue().unwrap(),
            1,
            "picture-number distance 2 (frame_num 2 -> 0)"
        );
        assert_eq!(r.ue().unwrap(), 3);
    }

    #[test]
    fn rejects_slices_before_a_header_and_malformed_frames() {
        let mut converter = Vcodec1Converter::new();
        assert!(converter
            .convert_frame(&frame(&[slice_unit(2, false, 1, None, 0, &[1])]))
            .is_err());
        assert!(converter.convert_frame(&[0, 9, 0, 0, 1]).is_err());
        assert!(converter
            .convert_frame(&frame(&[vec![0xc8, 0x00]]))
            .is_err());
    }
}
