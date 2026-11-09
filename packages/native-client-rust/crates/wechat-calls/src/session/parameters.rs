//! Call request advertisements for this implementation.
use super::*;
use wechat_protocol::{calls::voice_offer::VoiceOffer, crypto};

/// Independent Linux SDK metadata for the selected direct voice subset. It
/// advertises our supported codec/layout, not another client's copied identity.
/// Only returned current capabilities determine whether media may start.
pub fn independent_voice_parameters() -> Result<VoiceParameters> {
    let major = env!("CARGO_PKG_VERSION_MAJOR").parse::<u8>()?;
    let minor = env!("CARGO_PKG_VERSION_MINOR").parse::<u8>()?;
    let patch = env!("CARGO_PKG_VERSION_PATCH").parse::<u8>()?;
    let version = i32::from_be_bytes([major, minor, patch, 0]);
    let offer = VoiceOffer::from_owned_random(version, crypto::random::<32>()?)?;
    let sdk = offer.sdk_info().to_vec();
    let group = p::message(&[
        p::bytes(1, b"wechat"),
        p::uint(2, 0),
        p::bytes(3, b"voip-2p"),
        p::bytes(4, &[]),
        p::uint(5, 1),
        p::bytes(6, &[]),
        p::uint(7, 0),
    ])?;
    let device = p::message(&[
        p::bytes(1, b"agent-wechat"),
        p::bytes(2, b"native"),
        p::bytes(3, env!("CARGO_PKG_VERSION").as_bytes()),
        p::bytes(4, &[]),
        p::bytes(5, b"agent-wechat"),
        p::uint(
            7,
            std::thread::available_parallelism().map_or(1, usize::from) as u64,
        ),
        p::uint(8, 0),
        p::bytes(9, std::env::consts::ARCH.as_bytes()),
    ])?;
    Ok(VoiceParameters {
        sdk,
        group,
        device,
        engine_capability: offer.engine_advertisement().to_vec(),
        channel_capability: offer.advertised_channel_identity().to_vec(),
        network_type: 0,
        sim_type: 0,
        video_length: 0,
        video_ratio: 0,
        nics: vec![],
        cloud_device_id: b"Ldeviceid".to_vec(),
        app_config_version: 1,
    })
}
