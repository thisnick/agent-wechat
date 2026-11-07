//! Call ownership, WebSocket media coordination and native workers.
//!
//! The normal Core registers one native backend for its admitted accounts.
//! Workers start only for current authorized call commands or incoming offers.
//! Video is unavailable. Conference handling is under isolated validation.

pub mod incoming;
pub mod notifications;

pub mod ownership;

pub mod service;

pub mod authority;

pub mod native;

#[doc(hidden)]
pub mod pcm_envelope;
