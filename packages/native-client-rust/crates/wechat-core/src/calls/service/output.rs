use super::*;
use tokio::time::{interval, MissedTickBehavior};

impl CallService {
    /// Start an application send generation for an audio (`1`) or video (`65537`) track.
    /// This does not reset transport counters.
    pub async fn start_media_track(&self, id: &str, track: u32, generation: u32) -> Result<()> {
        let mut state = self.lock()?;
        let call = self.media_transport(&state, id)?;
        if track == AUDIO_SEND_TRACK_ID {
            let current = call.send_generation;
            return state
                .media
                .get_mut(id)
                .expect("current media")
                .output
                .start(generation, current)
                .map_err(|_| error(CallErrorCode::Stale));
        }
        if track == VIDEO_SEND_TRACK_ID {
            if call.video_send_format.is_none() {
                return Err(error(CallErrorCode::InvalidRequest));
            }
            let current = call.video_send_generation;
            if generation == 0 || generation != current {
                return Err(error(CallErrorCode::Stale));
            }
            state
                .media
                .get_mut(id)
                .expect("current media")
                .video_started_generation = Some(generation);
            return Ok(());
        }
        Err(error(CallErrorCode::InvalidRequest))
    }

    /// Backwards-compatible helper for starting the audio send track.
    pub async fn start_media_audio(&self, id: &str, track: u32, generation: u32) -> Result<()> {
        if track != AUDIO_SEND_TRACK_ID {
            return Err(error(CallErrorCode::InvalidRequest));
        }
        self.start_media_track(id, track, generation).await
    }

    /// Inspect the active call's negotiated video send track, if any.
    pub async fn video_send_track(&self, id: &str) -> Result<Option<(u32, CallVideoFormat, bool)>> {
        let state = self.lock()?;
        let call = self.media_transport(&state, id)?;
        Ok(call
            .video_send_format
            .map(|format| (call.video_send_generation, format, call.camera_enabled)))
    }

    /// Accept a whole variable-size PCM chunk or return backpressure without
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
        if track != AUDIO_SEND_TRACK_ID || bytes.len() > frame::MAX_AUDIO_PAYLOAD_BYTES {
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

    /// Admit and forward one client-sent video frame on the outgoing video track.
    #[allow(clippy::too_many_arguments)]
    pub async fn submit_video(
        &self,
        id: &str,
        track: u32,
        generation: u32,
        kind: frame::MediaPayloadKind,
        flags: u16,
        timestamp_micros: u64,
        payload: &[u8],
    ) -> Result<()> {
        self.maintenance().await;
        let request = {
            let state = self.lock()?;
            let call = self.media_transport(&state, id)?;
            if track != VIDEO_SEND_TRACK_ID {
                return Err(error(CallErrorCode::InvalidRequest));
            }
            let Some(format) = call.video_send_format else {
                return Err(error(CallErrorCode::InvalidRequest));
            };
            if kind != frame::MediaPayloadKind::from_video_codec(format.codec) {
                return Err(error(CallErrorCode::InvalidRequest));
            }
            if payload.is_empty() || payload.len() > frame::MAX_VIDEO_PAYLOAD_BYTES {
                return Err(error(CallErrorCode::InvalidRequest));
            }
            if generation == 0 || generation > call.video_send_generation {
                return Err(error(CallErrorCode::Stale));
            }
            if generation < call.video_send_generation {
                // Frame from a superseded camera generation still in flight on the media socket.
                return Ok(());
            }
            let media = state.media.get(id).expect("current media");
            if media.video_started_generation != Some(generation) {
                return Err(error(CallErrorCode::Stale));
            }
            if call.state == CallState::Ending || !call.camera_enabled {
                return Ok(());
            }
            BackendVideoRequest {
                identity: call.identity.clone(),
                codec: format.codec,
                generation,
                flags,
                timestamp_micros,
                payload: payload.to_vec(),
                lease: call.video_lease.clone(),
            }
        };
        let generation = request.generation;
        let identity = request.identity.clone();
        let result = self.backend.send_video(request).await;
        if result.is_err() {
            let state = self.lock()?;
            if let Ok(call) = self.media_transport(&state, id) {
                if call.identity == identity
                    && (call.state == CallState::Ending
                        || !call.camera_enabled
                        || (matches!(result, Err(CallServiceError(CallErrorCode::Stale)))
                            && call.video_send_generation > generation))
                {
                    return Ok(());
                }
            }
        }
        // Video backpressure drops the frame rather than tearing down the call.
        if matches!(result, Err(CallServiceError(CallErrorCode::Capacity))) {
            return Ok(());
        }
        result
    }

    /// Ask the backend to send an `IRequest` keyframe trigger for an active receive video track.
    pub async fn request_video_keyframe(&self, id: &str, track_id: u32) -> Result<()> {
        self.maintenance().await;
        let identity = {
            let state = self.lock()?;
            let call = self.media_transport(&state, id)?;
            let Some(track) = call.receive_tracks.get(&track_id) else {
                return Err(error(CallErrorCode::Stale));
            };
            if !track.is_video() {
                return Err(error(CallErrorCode::InvalidRequest));
            }
            if call.state != CallState::Connected {
                return Ok(());
            }
            call.identity.clone()
        };
        let _ = self
            .backend
            .request_video_keyframe(identity, track_id)
            .await;
        Ok(())
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
                        track_id: AUDIO_SEND_TRACK_ID,
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
