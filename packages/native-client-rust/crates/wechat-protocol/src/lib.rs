//! Native transport and numbered WeChat protocol codecs.
//!
//! This library does not launch or load the official client. All peer responses
//! pass pinned MMTLS signature and record authentication before business decode.
//! Network methods are blocking; async services must use a blocking worker.
pub mod business;
mod client;
pub mod crypto;
pub mod media;
pub mod media_send;
pub mod mmtls;
mod notice;
pub mod protobuf;
pub mod wire;
pub use client::*;

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
