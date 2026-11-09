//! Voice-session and media building blocks without devices or persistence.
//!
//! Native call sessions consume the protocol crate's codecs and authenticated
//! transport. Hosts supply account authority and operation journals. This crate
//! does not open microphone/speaker devices or persist account credentials.
//! Without default features, portable PCM, queue, pacing and mixing utilities
//! remain available without the protocol or native codec dependencies.
//!
//! # Feature selection
//!
//! - Default `native-runtime`: native session/media integration, protocol and Opus.
//! - `native-opus` alone: codec and portable utilities, without protocol.
//! - No default features: portable utilities, including WebAssembly support.
//!
//! Native modules are only available outside WebAssembly. The host supplies
//! execution; no Tokio runtime is created. The high-level account-bound calling
//! service is not part of this release. Use the daemon Calls API for complete
//! dial/join/invite/leave operations; see the crate README for integration limits.
//!
//! # Mix one source on a shared sample clock
//!
//! ```
//! use wechat_calls::{audio::{AudioFormat, AudioFrame}, mixer::PcmMixer};
//! let format = AudioFormat::new(16000, 1, 20)?;
//! let mut mixer = PcmMixer::new(format, 2, 3, 0)?;
//! mixer.add_track(7, 1)?;
//! mixer.push(7, AudioFrame {
//!     generation: 1, sequence: 0, timestamp: 0,
//!     samples: vec![1000; format.samples_per_frame()],
//! })?;
//! assert_eq!(mixer.render(0)?.samples, vec![1000; 320]);
//! # Ok::<(), wechat_calls::audio::MediaError>(())
//! ```

pub mod audio;
pub mod mixer;
pub mod ordering;
pub mod pcm_output;
pub mod playout;
pub mod tone;
pub mod track_queue;

#[cfg(all(feature = "native-runtime", not(target_arch = "wasm32")))]
#[doc(hidden)]
pub mod admission;

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
