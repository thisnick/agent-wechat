use super::*;
use tokio::time::{interval, MissedTickBehavior};

impl CallService {
    /// Start an application generation. This does not reset transport counters.
    pub async fn start_media_audio(&self, id: &str, track: u32, generation: u32) -> Result<()> {
        let mut state = self.lock()?;
        let current = self.media_transport(&state, id)?.send_generation;
        if track != 1 {
            return Err(error(CallErrorCode::InvalidRequest));
        }
        state
            .media
            .get_mut(id)
            .expect("current media")
            .output
            .start(generation, current)
            .map_err(|_| error(CallErrorCode::Stale))
    }

    /// Accept a whole variable-size chunk or return backpressure without
    /// accepting any samples. Framing and scheduling belong to this runtime.
    pub async fn submit_pcm(
        &self,
        id: &str,
        track: u32,
        generation: u32,
        bytes: &[u8],
    ) -> Result<usize> {
        let mut state = self.lock()?;
        let call = self.media_transport(&state, id)?;
        let current = call.send_generation;
        let ending = call.state == CallState::Ending;
        if track != 1 || bytes.len() > 64 * 1024 {
            return Err(error(CallErrorCode::InvalidRequest));
        }
        let output = &mut state.media.get_mut(id).expect("current media").output;
        output
            .push(generation, current, bytes)
            .map_err(|error_value| match error_value {
                wechat_calls::audio::MediaError::QueueFull => error(CallErrorCode::Capacity),
                _ => error(CallErrorCode::InvalidRequest),
            })?;
        if ending {
            return Ok(0);
        }
        Ok(output.pending_bytes())
    }

    fn next_output(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let mut state = self.lock()?;
        let call = self.media_transport(&state, id)?;
        let generation = call.send_generation;
        if call.state != CallState::Connected {
            return Ok(None);
        }
        let frame = state
            .media
            .get_mut(id)
            .expect("current media")
            .output
            .pop(generation)
            .map_err(|_| error(CallErrorCode::Capacity))?;
        frame
            .map(|frame| {
                frame::encode_pcm(
                    MediaHeader {
                        track_id: 1,
                        generation,
                        sequence: frame.sequence,
                        timestamp_micros: frame.timestamp * 1_000_000
                            / u64::from(Self::format().sample_rate),
                    },
                    Self::media_format(),
                    &frame.samples,
                )
                .map_err(|_| error(CallErrorCode::InvalidRequest))
            })
            .transpose()
    }

    pub(super) fn start_output(&self, id: String) {
        let Some(service) = self.self_weak.get().cloned() else {
            return;
        };
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_millis(u64::from(
                CallService::format().frame_ms,
            )));
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(service) = service.upgrade() else {
                    return;
                };
                let result = match service.next_output(&id) {
                    Ok(Some(frame)) => service.media_frame(&id, &frame).await,
                    Ok(None) => continue,
                    Err(_) => return,
                };
                if result.is_err() {
                    service.detach_media(&id).await;
                    return;
                }
            }
        });
    }
}
