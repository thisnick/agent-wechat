//! Daemon authority and event adapter for the shared call-media runtime.
use super::*;
use wechat_calls::native_media::{
    self, ConferenceMediaHost, ConferenceReceiveTrack, DirectAudioFrame, DirectMediaHost,
    DirectMediaOptions, MediaMetric,
};

impl DirectAudioFrame for QueuedAudio {
    fn generation(&self) -> u32 {
        self.request.generation
    }
    fn samples(&self) -> &[i16] {
        &self.request.samples
    }
}

pub(super) struct Host<'a> {
    pub(super) worker: &'a Worker,
    pub(super) owner: Option<(&'a BackendSink, &'a tokio::runtime::Handle)>,
}
impl DirectMediaHost for Host<'_> {
    type Frame = QueuedAudio;
    fn current(&self) -> bool {
        self.worker.current() && !self.worker.stop.load(Ordering::Acquire)
    }
    fn generation(&self) -> u32 {
        self.worker.generation.load(Ordering::Acquire)
    }
    fn frame_current(&self, frame: &QueuedAudio) -> bool {
        audio_current(self.worker, frame)
    }
    fn connected(&self) -> Result<()> {
        let (sink, runtime) = self.owner.context("Call owner unavailable")?;
        runtime
            .block_on(sink.connected())
            .map_err(|_| anyhow::anyhow!("Call owner unavailable"))
    }
    fn receive_pcm(&self, samples: &[i16]) -> Result<()> {
        let (sink, runtime) = self.owner.context("Call owner unavailable")?;
        runtime
            .block_on(sink.audio(samples))
            .map_err(|_| anyhow::anyhow!("Call audio receiver unavailable"))
    }
    fn record(&self, metric: MediaMetric, count: u64) {
        let metrics = &self.worker.metrics;
        let target = match metric {
            MediaMetric::InvalidOrder => &metrics.rx_invalid_order,
            MediaMetric::SkippedFrames => &metrics.rx_skipped_frames,
            MediaMetric::Gaps => &metrics.rx_gaps,
            MediaMetric::DeliveredFrames => &metrics.rx_delivered_frames,
            MediaMetric::StaleFrames => &metrics.tx_stale_frames,
            MediaMetric::InputUnderruns => &metrics.tx_input_underruns,
        };
        target.fetch_add(count, Ordering::Relaxed);
    }
    fn submit(
        &self,
        socket: &UdpSocket,
        endpoint: SocketAddrV4,
        packet: &[u8],
        frames: &[&QueuedAudio],
    ) -> Result<bool> {
        send_datagram(socket, endpoint, packet, self.worker, frames)
    }
}

pub(super) fn run(
    config: &NativeCallConfig,
    worker: &Arc<Worker>,
    media: wechat_calls::session::NegotiatedMedia,
    audio: Receiver<QueuedAudio>,
    sink: &BackendSink,
    runtime: &tokio::runtime::Handle,
) -> Result<()> {
    let options = DirectMediaOptions {
        epoch: MediaEpoch {
            account_generation: nonzero64(worker.lease.authority().generation.as_bytes()),
            logout_epoch: 0,
            call_generation: nonzero64(worker.identity.call_id().as_bytes()),
            key_generation: 1,
        },
        peer_idle_timeout: config.peer_idle_timeout,
    };
    let result = native_media::run_direct_media(
        &Host {
            worker,
            owner: Some((sink, runtime)),
        },
        options,
        media,
        audio,
    );
    worker.media_stop.store(true, Ordering::Release);
    result
}

impl ConferenceMediaHost for Host<'_> {
    fn receive_tracks(&self, tracks: Vec<ConferenceReceiveTrack>) -> Result<()> {
        let (sink, runtime) = self.owner.context("Call owner unavailable")?;
        runtime.block_on(
            sink.receive_tracks(
                tracks
                    .into_iter()
                    .map(|track| crate::call_types::CallAudioTrack {
                        id: u32::from(track.member) + 2,
                        generation: track.generation,
                        direction: crate::call_types::CallTrackDirection::Receive,
                        format: CallAudioFormat::default(),
                        participant_id: Some(format!("member-{}", track.member)),
                    })
                    .collect(),
            ),
        )?;
        Ok(())
    }
    fn receive_track_pcm(
        &self,
        member: u16,
        generation: u32,
        frame: wechat_calls::conference_media::DecodedAudio,
    ) -> Result<()> {
        let (sink, runtime) = self.owner.context("Call owner unavailable")?;
        runtime.block_on(sink.track_audio(
            super::super::pcm_envelope::MediaHeader {
                track_id: u32::from(member) + 2,
                generation,
                sequence: frame.sequence,
                timestamp_micros: frame.sample_offset * 1_000_000 / 16000,
            },
            &frame.samples,
        ))?;
        Ok(())
    }
    fn submit_generation(
        &self,
        socket: &UdpSocket,
        endpoint: SocketAddrV4,
        packet: &[u8],
        frames: &[&QueuedAudio],
        generation: u32,
    ) -> Result<bool> {
        send_datagram_generation(
            socket,
            endpoint,
            packet,
            self.worker,
            frames,
            Some(generation),
        )
    }
}
