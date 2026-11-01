//! Local voice advertisements are distinct from returned selected capabilities.
//! A fresh offer advertises only the supported Opus17 bit. The server's selected
//! codec number, packet format and channel flags require separate admission.

use super::client::VoiceParameters;
use crate::{crypto, protobuf as p};
use anyhow::Result;
use zeroize::Zeroizing;

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
                let value = match field {
                    1 => 0x457,
                    5 => 0x40000,
                    6 => 0x100,
                    25 => 5,
                    26 => 2,
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
    /// Replace only request-side fields. Negotiated response buffers must never
    /// be fed back through these fields as the next call's advertised support.
    pub fn apply_to(&self, parameters: &mut VoiceParameters) {
        parameters.sdk = self.sdk.clone();
        parameters.engine_capability = self.engine.clone();
        parameters.channel_capability = self.channel_advertised.to_vec();
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
    }
    #[test]
    fn own_client_version_keeps_signed_engine_and_raw_hex_sdk_forms() {
        let offer = VoiceOffer::from_owned_random(i32::MIN, [1; 32]).unwrap();
        let fields = p::decode(offer.engine_advertisement()).unwrap();
        assert_eq!(p::scalar(&fields, 27).unwrap() as i64, i64::from(i32::MIN));
        let sdk = p::decode(offer.sdk_info()).unwrap();
        assert_eq!(p::data(&sdk, 2).unwrap(), b"0x80000000");
    }
}
