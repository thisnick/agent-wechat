//! Native media workers independent of daemon authorization, persistence and
//! sockets exposed to consumers. These integration hooks are internal to the
//! account-owning runtime migration, not the final public calling facade.
mod conference;
mod conference_sources;
mod direct;
mod relay;
mod submission;
pub use conference_sources::{reconcile_roster, BoundConferenceReceiver};
pub use direct::{run_direct_media, run_direct_media_with_video};
pub use relay::connect_relay;
pub use submission::SubmissionGate;

use anyhow::Result;
use std::{
    collections::VecDeque,
    net::{SocketAddrV4, UdpSocket},
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant},
};
use wechat_protocol::calls::{
    audio_packet::MediaEpoch,
    video_packet::{ReassembledVideoFrame, VideoWireCodec},
};

#[derive(Clone, Copy)]
pub struct DirectMediaOptions {
    pub epoch: MediaEpoch,
    pub peer_idle_timeout: Duration,
}

/// Outbound standard Annex-B video access unit queued for WeChat RSM packetization.
pub struct DirectVideoTx {
    /// Video send track generation (stale generations are dropped).
    pub generation: u32,
    /// Target WeChat wire codec (`H265` or `H264`).
    pub codec: VideoWireCodec,
    /// Monotonic stream timestamp in milliseconds.
    pub timestamp_ms: u32,
    /// Target frame rate hint carried in the RSM header.
    pub fps: u8,
    /// Target bitrate hint in kbps carried in the RSM header.
    pub kbps: u16,
    /// Standard Annex-B access unit bytes.
    pub annexb: Vec<u8>,
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
    /// Deliver one decoded PCM frame at its aligned 16 kHz presentation sample offset.
    fn receive_pcm_at(&self, _sample_offset: u64, samples: &[i16]) -> Result<()> {
        self.receive_pcm(samples)
    }
    fn record(&self, metric: MediaMetric, count: u64);
    fn submit(
        &self,
        socket: &UdpSocket,
        endpoint: SocketAddrV4,
        packet: &[u8],
        frames: &[&Self::Frame],
    ) -> Result<bool>;
    /// Current video send generation; `0` when local camera video is disabled.
    fn video_generation(&self) -> u32 {
        0
    }
    /// Deliver one reassembled inbound video frame.
    fn receive_video(&self, _frame: ReassembledVideoFrame) -> Result<()> {
        Ok(())
    }
    /// Called when the remote peer or relay QoS requests a fresh keyframe.
    fn on_peer_keyframe_requested(&self) {}
    /// Poll whether the local client asked for a keyframe from a remote member (returning `Some(member_id)`).
    fn take_local_keyframe_request(&self) -> Option<u16> {
        None
    }
    /// Whether outbound media packets should prefer `Live98` (`0x98`) framing.
    fn prefer_live98_tx(&self) -> bool {
        false
    }
}

pub use conference::{
    run_conference_media, run_conference_media_with_video, ConferenceMediaHost,
    ConferenceMediaOptions, ConferenceMediaReport, ConferenceReceiveTrack, ConferenceRoster,
};

/// Bounded TX audio pre-buffer that absorbs timer and WebSocket arrival jitter
/// before the 20 ms `FramePacer` pops frames for Opus encoding, while capping
/// single-frame startup wait so short synthetic bursts never stall.
pub(super) struct TxAudioJitterBuffer<T> {
    frames: VecDeque<(Instant, T)>,
    primed: bool,
    min_frames: usize,
    max_frames: usize,
    max_prime_wait: Duration,
}

impl<T> TxAudioJitterBuffer<T> {
    pub(super) fn new() -> Self {
        Self {
            frames: VecDeque::with_capacity(8),
            primed: false,
            min_frames: 2,
            max_frames: 6,
            max_prime_wait: Duration::from_millis(30),
        }
    }

    pub(super) fn is_priming(&self) -> bool {
        !self.primed && !self.frames.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.frames.clear();
        self.primed = false;
    }

    pub(super) fn drain_from(
        &mut self,
        rx: &Receiver<T>,
        now: Instant,
        mut is_current: impl FnMut(&T) -> bool,
        mut on_stale: impl FnMut(),
    ) {
        loop {
            match rx.try_recv() {
                Ok(item) if is_current(&item) => {
                    if self.frames.len() >= self.max_frames {
                        self.frames.pop_front();
                    }
                    self.frames.push_back((now, item));
                    if self.frames.len() >= self.min_frames {
                        self.primed = true;
                    }
                }
                Ok(_) => on_stale(),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    pub(super) fn pop(
        &mut self,
        now: Instant,
        mut is_current: impl FnMut(&T) -> bool,
        mut on_stale: impl FnMut(),
    ) -> Option<T> {
        while let Some((_, front)) = self.frames.front() {
            if is_current(front) {
                break;
            }
            self.frames.pop_front();
            on_stale();
        }
        if self.frames.is_empty() {
            self.primed = false;
            return None;
        }
        if !self.primed {
            let ready_by_age = self.frames.front().is_some_and(|(queued_at, _)| {
                now.saturating_duration_since(*queued_at) >= self.max_prime_wait
            });
            if self.frames.len() < self.min_frames && !ready_by_age {
                return None;
            }
            self.primed = true;
        }
        let (_, item) = self.frames.pop_front()?;
        if self.frames.is_empty() {
            self.primed = false;
        }
        Some(item)
    }
}

#[cfg(test)]
mod jitter_tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn tx_jitter_buffer_absorbs_arrival_jitter_without_underruns() {
        let (tx, rx) = mpsc::channel::<u32>();
        let mut buf = TxAudioJitterBuffer::new();
        let t0 = Instant::now();
        // Frame 0 arrives at t=0: waits for second frame to build 2-frame cushion.
        tx.send(0).unwrap();
        buf.drain_from(&rx, t0, |_| true, || {});
        assert_eq!(buf.pop(t0, |_| true, || {}), None);

        // Frame 1 arrives at t=20ms -> primed! Pops frame 0 while retaining frame 1.
        let t1 = t0 + Duration::from_millis(20);
        tx.send(1).unwrap();
        buf.drain_from(&rx, t1, |_| true, || {});
        assert_eq!(buf.pop(t1, |_| true, || {}), Some(0));

        // Frame 2 arrives 8ms late (after t=40ms tick): buffered frame 1 still plays!
        let t2 = t0 + Duration::from_millis(40);
        buf.drain_from(&rx, t2, |_| true, || {});
        assert_eq!(buf.pop(t2, |_| true, || {}), Some(1));

        // Frame 2 + 3 arrive before t=60ms tick: continuous output, zero drops.
        let t3 = t0 + Duration::from_millis(60);
        tx.send(2).unwrap();
        tx.send(3).unwrap();
        buf.drain_from(&rx, t3, |_| true, || {});
        assert_eq!(buf.pop(t3, |_| true, || {}), Some(2));
        assert_eq!(
            buf.pop(t3 + Duration::from_millis(20), |_| true, || {}),
            Some(3)
        );
    }

    #[test]
    fn tx_jitter_buffer_flushes_lone_frame_after_prime_timeout() {
        let (tx, rx) = mpsc::channel::<u32>();
        let mut buf = TxAudioJitterBuffer::new();
        let t0 = Instant::now();
        tx.send(7).unwrap();
        buf.drain_from(&rx, t0, |_| true, || {});
        assert_eq!(
            buf.pop(t0 + Duration::from_millis(10), |_| true, || {}),
            None
        );
        assert_eq!(
            buf.pop(t0 + Duration::from_millis(35), |_| true, || {}),
            Some(7)
        );
    }
}
