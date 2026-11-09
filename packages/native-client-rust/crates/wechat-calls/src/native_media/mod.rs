//! Native media workers independent of daemon authorization, persistence and
//! sockets exposed to consumers. These integration hooks are internal to the
//! account-owning runtime migration, not the final public calling facade.
mod conference;
mod conference_sources;
mod direct;
mod relay;
mod submission;
pub use conference_sources::{reconcile_roster, BoundConferenceReceiver};
pub use direct::run_direct_media;
pub use relay::connect_relay;
pub use submission::SubmissionGate;

use anyhow::Result;
use std::{
    net::{SocketAddrV4, UdpSocket},
    time::Duration,
};
use wechat_protocol::calls::audio_packet::MediaEpoch;

#[derive(Clone, Copy)]
pub struct DirectMediaOptions {
    pub epoch: MediaEpoch,
    pub peer_idle_timeout: Duration,
}

pub trait DirectAudioFrame {
    fn generation(&self) -> u32;
    fn samples(&self) -> &[i16];
}

#[derive(Clone, Copy)]
pub enum MediaMetric {
    InvalidOrder,
    SkippedFrames,
    Gaps,
    DeliveredFrames,
    StaleFrames,
    InputUnderruns,
}

/// Implemented by the owning runtime. Notification/audio delivery must enqueue
/// without waiting for a consumer to drain it. `submit` must serialize its final
/// authority and generation check with interruption and physical UDP handoff.
/// It must perform no filesystem work, codec work, or blocking socket wait while
/// that admission gate is held. `false` means no datagram was handed off.
pub trait DirectMediaHost {
    type Frame: DirectAudioFrame;
    fn current(&self) -> bool;
    fn generation(&self) -> u32;
    fn frame_current(&self, frame: &Self::Frame) -> bool;
    fn connected(&self) -> Result<()>;
    fn receive_pcm(&self, samples: &[i16]) -> Result<()>;
    fn record(&self, metric: MediaMetric, count: u64);
    fn submit(
        &self,
        socket: &UdpSocket,
        endpoint: SocketAddrV4,
        packet: &[u8],
        frames: &[&Self::Frame],
    ) -> Result<bool>;
}

pub use conference::{
    run_conference_media, ConferenceMediaHost, ConferenceMediaOptions, ConferenceMediaReport,
    ConferenceReceiveTrack, ConferenceRoster,
};
