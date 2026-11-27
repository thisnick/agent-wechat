//! Call signaling codecs, authenticated host routes and media packet transforms.
//!
//! Call lifecycle and session state machines live in wechat-calls, which depends
//! on this crate. NativeClient owns authenticated account state. Embedding hosts
//! retain authorization, operation journals and device policy. Decoding a packet
//! or preparing a request starts no call. Media key lifetime and broader peer
//! interoperability still require separate validation.

pub mod app_frame;
pub mod app_session;
pub mod audio_packet;
pub mod conference;
pub mod host_policy;
pub mod host_transport;
pub mod incoming;
pub mod personal;
pub mod relay;
/// In-band reliable `0x50` messages and their acknowledgements.
pub mod rudp;
pub mod signaling;
/// Rewrap of WeChat's Vcodec1 video packaging as standard H.264.
pub mod vcodec1;
pub mod video_arq;
/// Video packetization, reassembly and video RTCP/QoS codecs.
pub mod video_packet;
pub mod video_qos;

pub mod voice_offer;
