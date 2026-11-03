//! Call ownership, WebSocket media coordination and native workers.
//!
//! The normal Core registers one native backend for its admitted accounts.
//! Workers start only for current authorized call commands or incoming offers.
//! Video and conference calls are not implemented.

pub mod incoming;
pub mod notifications;

pub mod ownership;

pub mod service;

pub mod authority;

pub mod native;
