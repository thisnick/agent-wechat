//! Offline building blocks for experimental audio/video call support.
//!
//! Enabling this feature does not enable making or receiving calls. These
//! codecs have no socket, account, capture-device or call-session integration.
//! Live negotiation, key lifecycle and peer interoperability remain unverified.

pub mod channel;
pub mod signaling;
