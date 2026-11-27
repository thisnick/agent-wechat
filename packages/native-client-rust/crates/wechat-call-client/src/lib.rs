//! Shared Calls Protocol v3 client media, device, and WebSocket transport foundation.
//!
//! Used by both the `wx calls` CLI and the desktop Tauri application so native
//! audio capture/playback, conference mixing, FFmpeg camera capture/encoding,
//! camera enumeration, and `WXCM` v3 WebSocket framing are implemented once.

pub mod audio;
pub mod inspect;
pub mod media;
pub mod synthetic_audio;
pub mod transport;
pub mod video;

pub use wechat_audio;
pub use wechat_call_contracts::{call_types, pcm_envelope};
pub use wechat_calls::audio::AudioFormat;
