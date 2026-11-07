use super::*;
use tokio::sync::Notify;
use wechat_calls::track_queue::{Delivery, TrackQueue};

pub type MediaDelivery = Delivery<ReceivedCallAudio, CallEvent>;

pub(super) struct MediaQueue {
    data: Mutex<QueueData>,
    changed: Notify,
}
struct QueueData {
    queue: TrackQueue<ReceivedCallAudio, CallEvent>,
    closed: bool,
}

impl MediaQueue {
    pub(super) fn new(tracks: &[CallAudioTrack]) -> Result<Arc<Self>> {
        let mut queue = TrackQueue::new(128, MEDIA_RECEIVE_CAPACITY, 512)
            .map_err(|_| error(CallErrorCode::Capacity))?;
        for track in tracks {
            queue
                .initial_track(track.id, track.generation)
                .map_err(|_| error(CallErrorCode::Stale))?;
        }
        Ok(Arc::new(Self {
            data: Mutex::new(QueueData {
                queue,
                closed: false,
            }),
            changed: Notify::new(),
        }))
    }

    fn change(
        &self,
        change: impl FnOnce(
            &mut TrackQueue<ReceivedCallAudio, CallEvent>,
        ) -> std::result::Result<(), wechat_calls::audio::MediaError>,
    ) -> Result<()> {
        let mut data = self.data.lock().map_err(|_| error(CallErrorCode::Closed))?;
        if data.closed {
            return Err(error(CallErrorCode::Closed));
        }
        let result = change(&mut data.queue);
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
        self.change(|queue| queue.push(track, generation, audio))
    }
    pub(super) fn control(&self, event: CallEvent) -> Result<()> {
        self.change(|queue| match &event {
            CallEvent::TrackAdded { track, .. } => queue.add(track.id, track.generation, event),
            CallEvent::TrackRemoved {
                track_id,
                generation,
                ..
            } => queue.remove(*track_id, *generation, event),
            _ => queue.control(event),
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
        self.0
            .data
            .lock()
            .map_or(0, |data| data.queue.pending_audio())
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
        let dropped = data.queue.take_dropped();
        if dropped != 0 {
            return Err(broadcast::error::TryRecvError::Lagged(dropped));
        }
        data.queue
            .pop()
            .ok_or(broadcast::error::TryRecvError::Empty)
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
