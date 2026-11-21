//! Binary `WXCM` v3 media envelopes re-exported from [`wechat_call_contracts`],
//! plus the WeChat wire-rotation flag helper used by the native call daemon.

pub use wechat_call_contracts::pcm_envelope::*;

/// Encode a clockwise [`wechat_protocol::calls::video_packet::VideoRotation`] into `WXCM` video flags.
pub const fn video_rotation_flags(
    rotation: wechat_protocol::calls::video_packet::VideoRotation,
) -> u16 {
    use wechat_protocol::calls::video_packet::VideoRotation;
    match rotation {
        VideoRotation::Deg0 => 0,
        VideoRotation::Deg90 => FLAG_ROTATE_90,
        VideoRotation::Deg180 => FLAG_ROTATE_180,
        VideoRotation::Deg270 => FLAG_ROTATE_270,
    }
}
