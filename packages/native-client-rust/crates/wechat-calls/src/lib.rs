//! Portable call-media building blocks.
//!
//! Hosts supply authenticated packets, clocks and PCM. This crate has no sockets,
//! credentials, microphone, speaker or account storage. The optional `native-opus`
//! feature adds a bundled libopus adapter; the default build has no native codec
//! dependency. These primitives alone do not establish a WeChat call.

pub mod audio;
pub mod frame;
pub mod playout;
pub mod tone;

#[cfg(feature = "native-opus")]
pub mod opus;
