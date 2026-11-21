//! Call request advertisements for this implementation.
use super::*;
use wechat_protocol::{
    calls::voice_offer::{VideoOfferOptions, VoiceOffer},
    crypto,
};

/// Independent Linux SDK metadata for the selected direct voice subset. It
/// advertises our supported codec/layout, not another client's copied identity.
/// Only returned current capabilities determine whether media may start.
pub fn independent_voice_parameters() -> Result<VoiceParameters> {
    build_parameters(None, false)
}

/// Independent Linux SDK metadata advertising Opus audio together with standard
/// H.265/HEVC and H.264/AVC video codecs (`Vcodec1 = 0`).
pub fn independent_video_parameters(conference: bool) -> Result<VoiceParameters> {
    build_parameters(
        Some(VideoOfferOptions {
            hevc: true,
            h264: true,
            video_flags: if conference { 100 } else { 178 },
        }),
        conference,
    )
}

fn build_parameters(video: Option<VideoOfferOptions>, conference: bool) -> Result<VoiceParameters> {
    let major = env!("CARGO_PKG_VERSION_MAJOR").parse::<u8>()?;
    let minor = env!("CARGO_PKG_VERSION_MINOR").parse::<u8>()?;
    let patch = env!("CARGO_PKG_VERSION_PATCH").parse::<u8>()?;
    let version = i32::from_be_bytes([major, minor, patch, 0]);
    let offer = match video {
        Some(options) => {
            VoiceOffer::from_owned_random_video(version, crypto::random::<32>()?, options)?
        }
        None => VoiceOffer::from_owned_random(version, crypto::random::<32>()?)?,
    };
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
        network_type: if video.is_some() { 4 } else { 0 },
        sim_type: if video.is_some() { 65_536 } else { 0 },
        video_length: if video.is_some() { 640 } else { 0 },
        video_ratio: if video.is_some() {
            if conference {
                2
            } else {
                178
            }
        } else {
            0
        },
        nics: vec![],
        cloud_device_id: b"Ldeviceid".to_vec(),
        app_config_version: 1,
    })
}
