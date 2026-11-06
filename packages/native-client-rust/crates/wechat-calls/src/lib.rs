//! Portable call-media building blocks.
//!
//! Native call sessions consume the protocol crate's codecs and authenticated
//! transport. Hosts supply account authority and operation journals. This crate
//! does not open microphone/speaker devices or persist account credentials.
//! Without default features, portable PCM, queue, pacing and mixing utilities
//! remain available without the protocol or native codec dependencies.

pub mod audio;
pub mod mixer;
pub mod ordering;
pub mod pcm_output;
pub mod playout;
pub mod tone;
pub mod track_queue;

#[cfg(all(feature = "native-opus", not(target_arch = "wasm32")))]
pub mod opus;

#[cfg(all(feature = "native-runtime", not(target_arch = "wasm32")))]
#[doc(hidden)]
pub mod session;

#[cfg(all(feature = "native-runtime", not(target_arch = "wasm32")))]
pub mod conference_media;

#[cfg(all(feature = "native-runtime", not(target_arch = "wasm32")))]
pub mod conference_send;

#[cfg(all(feature = "native-runtime", not(target_arch = "wasm32")))]
#[doc(hidden)]
pub mod native_media;
