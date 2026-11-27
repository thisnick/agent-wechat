//! Calls Protocol v3 `/media` WebSocket attachment and binary `WXCM` v3 send/receive client.

use crate::{
    audio::ReceiveAudio,
    transport::{
        debug_call, encode_segment, map_ws_send_error, open_socket_with_limit, receive,
        receive_event, send_json, CallEndpoint, ControlClient, Socket, MEDIA_SOCKET_LIMIT,
        STARTUP_TIMEOUT, WRITE_TIMEOUT,
    },
    video::{CapturedVideoFrame, ReceiveVideo},
};
use anyhow::{bail, ensure, Context, Result};
use futures_util::SinkExt;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use wechat_call_contracts::{
    call_types::{
        CallAudioFormat, CallEvent, CallMediaCommand, CallTrack, CallTrackDirection,
        CALL_PROTOCOL_VERSION,
    },
    pcm_envelope::{encode_pcm_chunk, encode_video, MediaHeader, MediaPayloadKind},
};
use wechat_calls::audio::AudioFormat;

/// Owns the `/calls/{callId}/media` WebSocket alongside its audio mixer and video tracker.
pub struct MediaClient {
    pub socket: Socket,
    pub send_track: CallTrack,
    pub video_send_track: Option<CallTrack>,
    pub receive: ReceiveAudio,
    pub receive_video: ReceiveVideo,
    pub attached: Instant,
    started_generation: Option<u32>,
    video_started_generation: Option<u32>,
    pub pcm_writes: u64,
    pub video_writes: u64,
}

impl MediaClient {
    /// Attach to `/calls/{callId}/media` with background RGB24 JPEG decoding (used by CLI).
    pub async fn attach(
        endpoint: &CallEndpoint,
        control: &ControlClient,
        call_id: &str,
        owner_token: &str,
    ) -> Result<Self> {
        Self::attach_inner(
            endpoint,
            &control.client_id,
            control.format,
            call_id,
            owner_token,
            false,
        )
        .await
    }

    /// Attach to `/calls/{callId}/media` with background RGB24 JPEG decoding, taking
    /// `client_id` and `audio_format` directly so the returned future is `Send`.
    pub async fn attach_parts(
        endpoint: &CallEndpoint,
        client_id: &str,
        audio_format: CallAudioFormat,
        call_id: &str,
        owner_token: &str,
    ) -> Result<Self> {
        Self::attach_inner(
            endpoint,
            client_id,
            audio_format,
            call_id,
            owner_token,
            false,
        )
        .await
    }

    /// Attach to `/calls/{callId}/media` with raw JPEG passthrough (used by Desktop WebView).
    /// Takes `client_id` and `audio_format` directly so the returned future is `Send` for
    /// spawned multi-threaded tasks (holding `&ControlClient` across `.await` is `!Send`).
    pub async fn attach_passthrough_parts(
        endpoint: &CallEndpoint,
        client_id: &str,
        audio_format: CallAudioFormat,
        call_id: &str,
        owner_token: &str,
    ) -> Result<Self> {
        Self::attach_inner(
            endpoint,
            client_id,
            audio_format,
            call_id,
            owner_token,
            true,
        )
        .await
    }

    /// Attach to `/calls/{callId}/media` with raw JPEG passthrough (used by Desktop WebView).
    pub async fn attach_passthrough(
        endpoint: &CallEndpoint,
        control: &ControlClient,
        call_id: &str,
        owner_token: &str,
    ) -> Result<Self> {
        Self::attach_inner(
            endpoint,
            &control.client_id,
            control.format,
            call_id,
            owner_token,
            true,
        )
        .await
    }

    async fn attach_inner(
        endpoint: &CallEndpoint,
        client_id: &str,
        audio_format: CallAudioFormat,
        call_id: &str,
        owner_token: &str,
        passthrough_video: bool,
    ) -> Result<Self> {
        let suffix = format!("calls/{}/media", encode_segment(call_id));
        let mut socket = open_socket_with_limit(endpoint, &suffix, MEDIA_SOCKET_LIMIT).await?;
        send_json(
            &mut socket,
            &CallMediaCommand::Attach {
                supported_protocol_versions: vec![CALL_PROTOCOL_VERSION],
                client_id: client_id.to_owned(),
                owner_token: owner_token.into(),
            },
        )
        .await?;
        let event = timeout(STARTUP_TIMEOUT, receive_event(&mut socket, "media"))
            .await
            .context("media attachment timed out")??;
        if let CallEvent::Error { code } = &event {
            bail!("media attachment rejected: {}", serde_json::to_value(code)?);
        }
        let CallEvent::MediaReady {
            protocol_version,
            send_tracks,
            receive_tracks,
        } = event
        else {
            bail!("media attachment was not accepted");
        };
        ensure!(
            protocol_version == CALL_PROTOCOL_VERSION,
            "unsupported call media version"
        );
        let send_track = send_tracks
            .iter()
            .find(|track| !track.is_video() && track.direction() == CallTrackDirection::Send)
            .cloned()
            .context("missing send audio track")?;
        let video_send_track = send_tracks
            .into_iter()
            .find(|track| track.is_video() && track.direction() == CallTrackDirection::Send);
        ensure!(
            send_track.audio_format() == Some(audio_format)
                && send_track.direction() == CallTrackDirection::Send
                && send_track.id() != 0
                && send_track.generation() != 0,
            "unsupported send audio track"
        );
        ensure!(
            receive_tracks
                .iter()
                .all(|track| track.id() != send_track.id()),
            "overlapping send and receive tracks"
        );
        let format = AudioFormat::new(
            audio_format.sample_rate,
            audio_format.channels,
            audio_format.frame_ms,
        )?;
        let receive_video = if passthrough_video {
            ReceiveVideo::new_passthrough(receive_tracks.clone())
        } else {
            ReceiveVideo::new(receive_tracks.clone())
        };
        let receive = ReceiveAudio::new(format, receive_tracks)?;
        debug_call(
            "media",
            format_args!(
                "media.ready call={call_id} audio_gen={} video_send={}",
                send_track.generation(),
                video_send_track.is_some()
            ),
        );
        Ok(Self {
            socket,
            receive,
            receive_video,
            attached: Instant::now(),
            send_track,
            video_send_track,
            started_generation: None,
            video_started_generation: None,
            pcm_writes: 0,
            video_writes: 0,
        })
    }

    pub fn synchronize_tracks(
        &mut self,
        control: &ControlClient,
        call_id: &str,
    ) -> Result<(bool, bool)> {
        let mut send_changed = false;
        let mut video_send_changed = false;
        for ((id, _), track) in &control.tracks {
            if id != call_id {
                continue;
            }
            if track.is_video() {
                if track.direction() == CallTrackDirection::Send {
                    let should_update = self
                        .video_send_track
                        .as_ref()
                        .is_none_or(|cur| track.generation() > cur.generation());
                    if should_update {
                        self.video_send_track = Some(track.clone());
                        video_send_changed = true;
                    }
                }
                continue;
            }
            if track.audio_format() != Some(control.format) {
                bail!("unsupported updated audio format");
            }
            if track.direction() == CallTrackDirection::Send
                && track.id() == self.send_track.id()
                && track.generation() > self.send_track.generation()
            {
                self.send_track = track.clone();
                send_changed = true;
            }
        }
        Ok((send_changed, video_send_changed))
    }

    pub async fn send_pcm(&mut self, samples: &[i16], format: AudioFormat) -> Result<()> {
        format.validate_frame(samples)?;
        let generation = self.send_track.generation();
        let track_id = self.send_track.id();
        let first = self.started_generation != Some(generation);
        if first {
            debug_call(
                "media",
                format_args!("track.start write attempted generation={generation}"),
            );
            if let Err(error) = send_json(
                &mut self.socket,
                &CallMediaCommand::TrackStart {
                    track_id,
                    generation,
                },
            )
            .await
            {
                debug_call("media", format_args!("track.start write failed: {error:#}"));
                return Err(error);
            }
            debug_call(
                "media",
                format_args!("track.start write completed generation={generation}"),
            );
            self.started_generation = Some(generation);
        }
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let packet = encode_pcm_chunk(
            MediaHeader {
                track_id,
                generation,
                sequence: self.pcm_writes,
                timestamp_micros: self.pcm_writes * u64::from(format.frame_ms()) * 1_000,
            },
            &bytes,
        )?;
        if let Err(error) = timeout(
            WRITE_TIMEOUT,
            self.socket.send(Message::Binary(packet.into())),
        )
        .await
        .context("call media write timed out")
        .and_then(|result| result.map_err(map_ws_send_error))
        {
            debug_call(
                "media",
                format_args!(
                    "PCM write failed after {} completed writes: {error:#}",
                    self.pcm_writes
                ),
            );
            return Err(error);
        }
        self.pcm_writes += 1;
        Ok(())
    }

    pub async fn send_video(&mut self, frame: &CapturedVideoFrame) -> Result<()> {
        let Some(track) = self.video_send_track.as_ref() else {
            return Ok(());
        };
        let Some(expected) = track.video_format() else {
            return Ok(());
        };
        if expected.codec != frame.codec {
            return Ok(());
        }
        if frame.bytes.is_empty()
            || frame.bytes.len() > wechat_call_contracts::pcm_envelope::MAX_VIDEO_PAYLOAD_BYTES
        {
            debug_call(
                "media",
                format_args!(
                    "dropping out-of-range local video frame ({} bytes)",
                    frame.bytes.len()
                ),
            );
            return Ok(());
        }
        let track_id = track.id();
        let generation = track.generation();
        if self.video_started_generation != Some(generation) {
            send_json(
                &mut self.socket,
                &CallMediaCommand::TrackStart {
                    track_id,
                    generation,
                },
            )
            .await?;
            self.video_started_generation = Some(generation);
        }
        let packet = encode_video(
            MediaPayloadKind::from_video_codec(frame.codec),
            frame.flags,
            MediaHeader {
                track_id,
                generation,
                sequence: self.video_writes,
                timestamp_micros: frame.timestamp_micros,
            },
            &frame.bytes,
        )?;
        timeout(
            WRITE_TIMEOUT,
            self.socket.send(Message::Binary(packet.into())),
        )
        .await
        .context("call video media write timed out")?
        .map_err(map_ws_send_error)?;
        self.video_writes += 1;
        Ok(())
    }

    pub async fn request_keyframe(&mut self, track_id: u32) -> Result<()> {
        send_json(
            &mut self.socket,
            &CallMediaCommand::VideoRequestKeyframe { track_id },
        )
        .await
    }

    pub async fn next_message(&mut self) -> Result<Message> {
        receive(&mut self.socket, "media").await
    }

    pub fn attached_elapsed(&self) -> Duration {
        self.attached.elapsed()
    }
}
