//! Shared Calls Protocol v3 JSON contracts and binary `WXCM` v3 media framing.
//!
//! Kept free of database, OpenSSL, and wire-protocol dependencies so both the
//! daemon (`wechat-core`) and client applications (`wx`, desktop Tauri) share
//! one contract definition.

pub mod call_types;
pub mod pcm_envelope;
