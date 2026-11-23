use super::*;
use std::collections::BTreeSet;
use tokio::sync::Notify;
use wechat_calls::track_queue::{Delivery, TrackQueue};

pub type MediaDelivery = Delivery<ReceivedCallAudio, CallEvent>;

/// Independent JPEG frames keep a shallow drop-oldest queue so display stays fresh.
const VIDEO_JPEG_RECEIVE_CAPACITY: usize = 2;
/// Inter-frame H.264/H.265 tracks keep deeper per-track buffering so worker bursts
/// and pre-attach GOP replays never evict keyframes.
const VIDEO_INTERFRAME_RECEIVE_CAPACITY: usize = 32;

fn track_is_jpeg_video(track: &CallTrack) -> bool {
    track
        .video_format()
        .is_some_and(|fmt| fmt.codec == CallVideoCodec::Jpeg)
}

pub(super) struct MediaQueue {
    data: Mutex<QueueData>,
    changed: Notify,
}
struct QueueData {
    audio: TrackQueue<ReceivedCallAudio, CallEvent>,
    video_jpeg: TrackQueue<ReceivedCallAudio, ()>,
    video_interframe: TrackQueue<ReceivedCallAudio, ()>,
    jpeg_video_tracks: BTreeSet<u32>,
    closed: bool,
}

impl MediaQueue {
    pub(super) fn new(tracks: &[CallTrack]) -> Result<Arc<Self>> {
        let mut audio = TrackQueue::new(128, MEDIA_RECEIVE_CAPACITY, 512)
            .map_err(|_| error(CallErrorCode::Capacity))?;
        let mut video_jpeg = TrackQueue::new(128, VIDEO_JPEG_RECEIVE_CAPACITY, 512)
            .map_err(|_| error(CallErrorCode::Capacity))?;
        let mut video_interframe = TrackQueue::new(128, VIDEO_INTERFRAME_RECEIVE_CAPACITY, 512)
            .map_err(|_| error(CallErrorCode::Capacity))?;
        let mut jpeg_video_tracks = BTreeSet::new();
        for track in tracks {
            if track.is_video() {
                if track_is_jpeg_video(track) {
                    jpeg_video_tracks.insert(track.id());
                    video_jpeg
                        .initial_track(track.id(), track.generation())
                        .map_err(|_| error(CallErrorCode::Stale))?;
                } else {
                    video_interframe
                        .initial_track(track.id(), track.generation())
                        .map_err(|_| error(CallErrorCode::Stale))?;
                }
            } else {
                audio
                    .initial_track(track.id(), track.generation())
                    .map_err(|_| error(CallErrorCode::Stale))?;
            }
        }
        Ok(Arc::new(Self {
            data: Mutex::new(QueueData {
                audio,
                video_jpeg,
                video_interframe,
                jpeg_video_tracks,
                closed: false,
            }),
            changed: Notify::new(),
        }))
    }

    fn change(
        &self,
        change: impl FnOnce(&mut QueueData) -> std::result::Result<(), wechat_calls::audio::MediaError>,
    ) -> Result<()> {
        let mut data = self.data.lock().map_err(|_| error(CallErrorCode::Closed))?;
        if data.closed {
            return Err(error(CallErrorCode::Closed));
        }
        let result = change(&mut data);
        if result.is_err() {
            data.closed = true;
        }
        drop(data);
        self.changed.notify_one();
        result.map_err(|_| error(CallErrorCode::Capacity))
    }

    pub(super) fn audio(
        &self,
        track: u32,
        generation: u32,
        audio: ReceivedCallAudio,
    ) -> Result<()> {
        self.change(|data| data.audio.push(track, generation, audio))
    }

    pub(super) fn video(
        &self,
        track: u32,
        generation: u32,
        frame: ReceivedCallAudio,
    ) -> Result<()> {
        self.change(|data| {
            if data.jpeg_video_tracks.contains(&track) {
                let _ = data.video_jpeg.push(track, generation, frame);
            } else {
                let _ = data.video_interframe.push(track, generation, frame);
            }
            Ok(())
        })
    }

    pub(super) fn control(&self, event: CallEvent) -> Result<()> {
        self.change(|data| match &event {
            CallEvent::TrackAdded { track, .. } if track.is_video() => {
                if track_is_jpeg_video(track) {
                    let _ = data
                        .video_interframe
                        .remove(track.id(), track.generation(), ());
                    data.jpeg_video_tracks.insert(track.id());
                    data.video_jpeg.add(track.id(), track.generation(), ())?;
                } else {
                    if data.jpeg_video_tracks.remove(&track.id()) {
                        let _ = data.video_jpeg.remove(track.id(), track.generation(), ());
                    }
                    data.video_interframe
                        .add(track.id(), track.generation(), ())?;
                }
                data.audio.control(event)
            }
            CallEvent::TrackAdded { track, .. } => {
                data.audio.add(track.id(), track.generation(), event)
            }
            CallEvent::TrackUpdated { track, .. } if track.is_video() => {
                if track.direction() == CallTrackDirection::Receive {
                    if track_is_jpeg_video(track) {
                        let _ = data
                            .video_interframe
                            .remove(track.id(), track.generation(), ());
                        data.jpeg_video_tracks.insert(track.id());
                        let _ = data.video_jpeg.add(track.id(), track.generation(), ());
                    } else {
                        if data.jpeg_video_tracks.remove(&track.id()) {
                            let _ = data.video_jpeg.remove(track.id(), track.generation(), ());
                        }
                        let _ = data
                            .video_interframe
                            .add(track.id(), track.generation(), ());
                    }
                }
                data.audio.control(event)
            }
            CallEvent::TrackRemoved {
                track_id,
                generation,
                ..
            } if *track_id >= VIDEO_SEND_TRACK_ID => {
                data.jpeg_video_tracks.remove(track_id);
                let _ = data.video_jpeg.remove(*track_id, *generation, ());
                let _ = data.video_interframe.remove(*track_id, *generation, ());
                data.audio.control(event)
            }
            CallEvent::TrackRemoved {
                track_id,
                generation,
                ..
            } => data.audio.remove(*track_id, *generation, event),
            _ => data.audio.control(event),
        })
    }

    pub(super) fn close(&self) {
        if let Ok(mut data) = self.data.lock() {
            data.closed = true;
        }
        self.changed.notify_one();
    }
}

pub struct MediaReceiveQueue(pub(super) Arc<MediaQueue>);
impl Drop for MediaReceiveQueue {
    fn drop(&mut self) {
        self.0.close();
    }
}
impl MediaReceiveQueue {
    pub fn pending_frames(&self) -> usize {
        self.0.data.lock().map_or(0, |data| {
            data.audio.pending_audio()
                + data.video_jpeg.pending_audio()
                + data.video_interframe.pending_audio()
        })
    }
    fn poll_delivery(
        &mut self,
    ) -> std::result::Result<MediaDelivery, broadcast::error::TryRecvError> {
        let mut data = self
            .0
            .data
            .lock()
            .map_err(|_| broadcast::error::TryRecvError::Closed)?;
        if data.closed {
            return Err(broadcast::error::TryRecvError::Closed);
        }
        let dropped = data.audio.take_dropped();
        if dropped != 0 {
            return Err(broadcast::error::TryRecvError::Lagged(dropped));
        }
        // Controls and audio always drain ahead of video.
        if let Some(delivery) = data.audio.pop() {
            return Ok(delivery);
        }
        // Discard silent video drops and internal video control sentinels.
        let _ = data.video_jpeg.take_dropped();
        while let Some(delivery) = data.video_jpeg.pop() {
            if let Delivery::Audio(frame) = delivery {
                return Ok(Delivery::Audio(frame));
            }
        }
        let _ = data.video_interframe.take_dropped();
        while let Some(delivery) = data.video_interframe.pop() {
            if let Delivery::Audio(frame) = delivery {
                return Ok(Delivery::Audio(frame));
            }
        }
        Err(broadcast::error::TryRecvError::Empty)
    }
    pub async fn recv_delivery(
        &mut self,
    ) -> std::result::Result<MediaDelivery, broadcast::error::RecvError> {
        loop {
            let queue = self.0.clone();
            let notified = queue.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.poll_delivery() {
                Ok(value) => return Ok(value),
                Err(broadcast::error::TryRecvError::Closed) => {
                    return Err(broadcast::error::RecvError::Closed)
                }
                Err(broadcast::error::TryRecvError::Lagged(count)) => {
                    return Err(broadcast::error::RecvError::Lagged(count))
                }
                Err(broadcast::error::TryRecvError::Empty) => notified.await,
            }
        }
    }
    pub async fn recv_frame(
        &mut self,
    ) -> std::result::Result<ReceivedCallAudio, broadcast::error::RecvError> {
        loop {
            if let Delivery::Audio(frame) = self.recv_delivery().await? {
                return Ok(frame);
            }
        }
    }
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.recv_frame().await {
                Ok(frame) => return Some(frame.bytes),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
    pub fn try_recv(&mut self) -> std::result::Result<Vec<u8>, mpsc::error::TryRecvError> {
        loop {
            match self.poll_delivery() {
                Ok(Delivery::Audio(frame)) => return Ok(frame.bytes),
                Ok(Delivery::Control(_)) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(broadcast::error::TryRecvError::Closed) => {
                    return Err(mpsc::error::TryRecvError::Disconnected)
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    return Err(mpsc::error::TryRecvError::Empty)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video_track(id: u32, codec: CallVideoCodec) -> CallTrack {
        CallTrack::Video {
            id,
            generation: 1,
            direction: CallTrackDirection::Receive,
            format: CallVideoFormat {
                codec,
                mode: CallVideoMode::Transcoded,
                max_width: 640,
                max_height: 480,
                max_fps: 15,
                bitrate_kbps: None,
            },
            participant_id: None,
            active: true,
        }
    }

    #[test]
    fn jpeg_tracks_cap_at_two_frames_while_interframe_tracks_keep_burst_intact() {
        let jpeg_track = video_track(65_538, CallVideoCodec::Jpeg);
        let h264_track = video_track(65_539, CallVideoCodec::H264);
        let queue = MediaQueue::new(&[jpeg_track, h264_track]).unwrap();
        let mut rx = MediaReceiveQueue(queue.clone());

        for seq in 0..4u8 {
            queue
                .video(
                    65_538,
                    1,
                    ReceivedCallAudio {
                        bytes: vec![seq],
                        enqueued_at: Instant::now(),
                        producer_gap: None,
                    },
                )
                .unwrap();
        }
        let mut jpeg_out = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            jpeg_out.push(bytes[0]);
        }
        assert_eq!(
            jpeg_out,
            vec![2, 3],
            "JPEG video queue should drop stale frames past capacity 2"
        );

        for seq in 0..6u8 {
            queue
                .video(
                    65_539,
                    1,
                    ReceivedCallAudio {
                        bytes: vec![100 + seq],
                        enqueued_at: Instant::now(),
                        producer_gap: None,
                    },
                )
                .unwrap();
        }
        let mut h264_out = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            h264_out.push(bytes[0]);
        }
        assert_eq!(
            h264_out,
            vec![100, 101, 102, 103, 104, 105],
            "inter-frame H.264/H.265 queue must preserve multi-frame bursts without evicting keyframes"
        );
    }
}
