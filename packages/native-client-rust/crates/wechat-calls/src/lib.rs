//! Portable call-media building blocks.
//!
//! Hosts supply authenticated packets, clocks and PCM. This crate has no sockets,
//! credentials, microphone, speaker or account storage. Native builds include a
//! bundled libopus adapter. The portable media interfaces also compile for
//! WebAssembly without the native codec. These primitives alone do not establish
//! a WeChat call.

pub mod audio;
pub mod frame;
pub mod playout;
pub mod tone;

#[cfg(not(target_arch = "wasm32"))]
pub mod opus;
