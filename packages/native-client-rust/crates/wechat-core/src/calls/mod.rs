//! Call ownership, WebSocket media coordination and optional native workers.
//!
//! The default backend is disabled. The native backend currently implements the
//! outgoing direct-voice path for an expiring isolated test; incoming calls,
//! video and conferences are not enabled by this module alone.

pub mod incoming;
pub mod notifications;

pub mod ownership;

pub mod service;

pub mod authority;

#[cfg(feature = "native-calls")]
pub mod native;
