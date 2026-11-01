//! Experimental call signaling and media building blocks.
//!
//! Explicit native session methods can submit authenticated direct-voice host
//! operations. Enabling this feature alone starts no daemon call worker. The
//! caller owns account/logout fences, intent persistence, current negotiation,
//! socket/device lifetimes and separate live-test authorization. Media key
//! lifecycle and broader peer interoperability still need validation.

pub mod app_frame;
pub mod app_session;
pub mod audio_packet;
pub mod client;
pub mod host_policy;
pub mod host_transport;
pub mod incoming;
pub mod personal;
pub mod relay;
pub mod signaling;

pub mod voice_offer;
