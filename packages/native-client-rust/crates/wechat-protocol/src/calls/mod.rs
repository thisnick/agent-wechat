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
pub mod signaling;

pub mod voice_offer;
