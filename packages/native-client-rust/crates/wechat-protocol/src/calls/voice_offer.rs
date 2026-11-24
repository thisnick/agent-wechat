//! Local voice advertisements are distinct from returned selected capabilities.
//! A fresh offer advertises only the supported Opus17 bit. The server's selected
//! codec number, packet format and channel flags require separate admission.

use crate::{crypto, protobuf as p};
use anyhow::Result;
use zeroize::Zeroizing;

/// Advertised standard video codec capabilities for an engine capability offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoOfferOptions {
    /// Advertise standard H.265/HEVC (`Vcodec2` bit `0x10` and `HW265` bit `0x04`, with capability fields 21 and 22).
    pub hevc: bool,
    /// Advertise standard H.264/AVC (`SwH264 | HW264`, bits `0x09`).
    pub h264: bool,
    /// Display aspect ratio hint encoded in engine capability field 4 (for example `178` for 16:9 direct or `100` for conference).
    pub video_flags: u32,
}

impl Default for VideoOfferOptions {
    fn default() -> Self {
        Self {
            hevc: true,
            h264: true,
            video_flags: 178,
        }
    }
}

/// Owned request metadata for one call generation. It intentionally has no
/// `Clone` or `Debug`: create a fresh identity for each new call. The embedding
/// owner must supply new random bytes and keep this value until that call ends.
pub struct VoiceOffer {
    sdk: Vec<u8>,
    engine: Vec<u8>,
    channel_internal: Zeroizing<[u8; 12]>,
    channel_advertised: [u8; 12],
}
impl VoiceOffer {
    /// `owner_version` is this embedding client's version, not a captured peer's
    /// version. Signed int32 representation is preserved in engine field27.
    /// MD5 reproduces the channel identity transform; it is not authentication.
    pub fn from_owned_random(owner_version: i32, random32: [u8; 32]) -> Result<Self> {
        Self::build(owner_version, random32, None)
    }

    /// Build a fresh offer advertising Opus audio together with the selected
    /// standard video codecs. Proprietary video codec bits remain unset.
    pub fn from_owned_random_video(
        owner_version: i32,
        random32: [u8; 32],
        video: VideoOfferOptions,
    ) -> Result<Self> {
        Self::build(owner_version, random32, Some(video))
    }

    fn build(
        owner_version: i32,
        random32: [u8; 32],
        video: Option<VideoOfferOptions>,
    ) -> Result<Self> {
        let random32 = Zeroizing::new(random32);
        let digest = Zeroizing::new(crypto::md5(random32.as_ref()));
        let mut internal = Zeroizing::new([0; 12]);
        internal.copy_from_slice(&digest[..12]);
        let mut advertised = *internal;
        advertised[..4].reverse();
        advertised[4..8].reverse();
        advertised[8..10].reverse();
        advertised[10..12].reverse();
        let mut engine = Vec::new();
        for field in (1..=27).chain(36..=40) {
            if field == 27 {
                engine.extend(p::int32(field, owner_version));
            } else {
                let value = match (field, video) {
                    (1, _) => 0x457,
                    (2, Some(v)) => {
                        let mut mask = 0u64;
                        if v.hevc {
                            // 0x10 = Vcodec2 (SW HEVC), 0x04 = HW265 (HW HEVC).
                            // Official ExchangeCabInfo drops Vcodec2 unless HW265 is also present.
                            mask |= 0x14;
                        }
                        if v.h264 {
                            mask |= 0x09;
                        }
                        if mask != 0 {
                            // Standard video feature bits (NewP2S=0x80, CLF=0x100, CABAC=0x200,
                            // HPP=0x400, LDENOISE=0x800, RDENOISE=0x1000, IREQUEST=0x2000).
                            // Proprietary Vcodec1 (0x02) remains unset.
                            mask |= 0x3f80;
                        }
                        mask
                    }
                    (3, Some(_)) => 0x3f,
                    (4, Some(v)) => u64::from(v.video_flags),
                    (5, _) => 0x40000,
                    (6, _) => 0x100,
                    // Advertise the modern transport version bit (`1 << 17`) on video offers so
                    // peers keep video ARQ/NACK and updated P2S feedback active during negotiation.
                    (7, Some(_)) => 0x20000,
                    (8, Some(_)) => 4,
                    (9, Some(_)) => 30,
                    (10, Some(_)) => 0x41,
                    (13, Some(v)) if v.hevc => 6,
                    (14, Some(v)) if v.hevc => 8,
                    (19, Some(v)) if v.h264 => 6,
                    (20, Some(v)) if v.h264 => 8,
                    (21, Some(v)) if v.hevc => 6,
                    (22, Some(v)) if v.hevc => 8,
                    (23, Some(v)) if v.h264 => 6,
                    (24, Some(v)) if v.h264 => 8,
                    (25, _) => 5,
                    (26, _) => 2,
                    (40, Some(_)) => 7,
                    _ => 0,
                };
                engine.extend(p::uint(field, value));
            }
        }
        let sdk = p::message(&[
            p::uint(1, 0x050000cb),
            p::bytes(2, format!("0x{:x}", owner_version as u32).as_bytes()),
        ])?;
        Ok(Self {
            sdk,
            engine,
            channel_internal: internal,
            channel_advertised: advertised,
        })
    }
    pub fn sdk_info(&self) -> &[u8] {
        &self.sdk
    }
    pub fn engine_advertisement(&self) -> &[u8] {
        &self.engine
    }
    pub fn advertised_channel_identity(&self) -> &[u8; 12] {
        &self.channel_advertised
    }
    pub fn internal_channel_identity(&self) -> &[u8; 12] {
        &self.channel_internal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_advertises_a_mask_and_fresh_channel_identity_not_selected_buffers() {
        let seed = std::array::from_fn(|i| i as u8);
        let offer = VoiceOffer::from_owned_random(0x01020300, seed).unwrap();
        let fields = p::decode(offer.engine_advertisement()).unwrap();
        assert_eq!(p::scalar(&fields, 1).unwrap(), 0x457);
        assert_eq!(p::scalar(&fields, 5).unwrap(), 0x40000);
        assert_ne!(p::scalar(&fields, 5).unwrap(), 17);
        assert_eq!(p::scalar(&fields, 6).unwrap(), 0x100);
        assert_eq!(p::scalar(&fields, 7).unwrap(), 0);
        assert_eq!(p::scalar(&fields, 25).unwrap(), 5);
        assert_eq!(p::scalar(&fields, 26).unwrap(), 2);
        assert_eq!(p::scalar(&fields, 27).unwrap(), 0x01020300);
        assert_eq!(fields.len(), 32);
        assert_eq!(
            hex::encode(offer.internal_channel_identity()),
            "b4ffcb23737cec315a4a4d1a"
        );
        assert_eq!(
            hex::encode(offer.advertised_channel_identity()),
            "23cbffb431ec7c734a5a1a4d"
        );
        assert_ne!(
            offer.advertised_channel_identity().as_slice(),
            p::message(&[p::uint(1, 2), p::uint(2, 3)]).unwrap()
        );
        let next = VoiceOffer::from_owned_random(0x01020300, [7; 32]).unwrap();
        assert_ne!(
            next.advertised_channel_identity(),
            offer.advertised_channel_identity()
        );
        let signed = VoiceOffer::from_owned_random(i32::MIN, [1; 32]).unwrap();
        assert_eq!(
            p::scalar(&p::decode(signed.engine_advertisement()).unwrap(), 27).unwrap() as i64,
            i64::from(i32::MIN)
        );
        assert_eq!(
            p::data(&p::decode(signed.sdk_info()).unwrap(), 2).unwrap(),
            b"0x80000000"
        );
    }

    #[test]
    fn video_offer_advertises_standard_hevc_and_h264_without_proprietary_vcodec1() {
        let offer =
            VoiceOffer::from_owned_random_video(0x01020300, [9; 32], VideoOfferOptions::default())
                .unwrap();
        let fields = p::decode(offer.engine_advertisement()).unwrap();
        assert_eq!(p::scalar(&fields, 2).unwrap(), 0x3f9d);
        assert_eq!(p::scalar(&fields, 3).unwrap(), 0x3f);
        assert_eq!(p::scalar(&fields, 4).unwrap(), 178);
        assert_eq!(p::scalar(&fields, 5).unwrap(), 0x40000);
        assert_eq!(p::scalar(&fields, 7).unwrap(), 0x20000);
        assert_eq!(p::scalar(&fields, 11).unwrap(), 0);
        assert_eq!(p::scalar(&fields, 12).unwrap(), 0);
        assert_eq!(p::scalar(&fields, 13).unwrap(), 6);
        assert_eq!(p::scalar(&fields, 14).unwrap(), 8);
        assert_eq!(p::scalar(&fields, 19).unwrap(), 6);
        assert_eq!(p::scalar(&fields, 20).unwrap(), 8);
        assert_eq!(p::scalar(&fields, 21).unwrap(), 6);
        assert_eq!(p::scalar(&fields, 22).unwrap(), 8);
        assert_eq!(p::scalar(&fields, 40).unwrap(), 7);
    }
}
