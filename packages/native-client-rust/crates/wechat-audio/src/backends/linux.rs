// PulseAudio backend derived from sys-voice; see THIRD-PARTY.md.
// The nonblocking mainloop replaces pa_simple's uninterruptible reads.
use super::{wait_ready, DeviceBackend, NATIVE_RATE};
use crate::{AecStatus, AudioConfig, AudioError, PlaybackHandle};
use libpulse_binding::{
    context::{Context, FlagSet as ContextFlags, State as ContextState},
    def::BufferAttr,
    mainloop::standard::{IterateResult, Mainloop},
    sample::{Format, Spec},
    stream::{FlagSet, PeekResult, SeekMode, State, Stream},
};
use std::sync::mpsc::SyncSender;

pub(super) struct Backend {
    record: Stream,
    output: Stream,
    context: Context,
    mainloop: Mainloop,
    playback: PlaybackHandle,
    capture: SyncSender<Vec<f32>>,
    generation: u64,
}

fn error(e: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(e.to_string())
}

impl Backend {
    pub(super) fn new(
        config: &AudioConfig,
        playback: PlaybackHandle,
        capture: SyncSender<Vec<f32>>,
    ) -> Result<Self, AudioError> {
        let mut mainloop =
            Mainloop::new().ok_or_else(|| error("cannot create PulseAudio mainloop"))?;
        let mut context = Context::new(&mainloop, "wechat-audio")
            .ok_or_else(|| error("cannot create PulseAudio context"))?;
        context
            .connect(None, ContextFlags::NOAUTOSPAWN, None)
            .map_err(error)?;
        wait_ready(|| {
            iterate(&mut mainloop)?;
            match context.get_state() {
                ContextState::Ready => Ok(true),
                ContextState::Failed | ContextState::Terminated => {
                    Err(error("PulseAudio connection failed"))
                }
                _ => Ok(false),
            }
        })?;
        let spec = Spec {
            format: Format::F32le,
            channels: 1,
            rate: NATIVE_RATE,
        };
        let mut record = Stream::new(&mut context, "WeChat microphone", &spec, None)
            .ok_or_else(|| error("cannot create capture stream"))?;
        let mut output = Stream::new(&mut context, "WeChat speaker", &spec, None)
            .ok_or_else(|| error("cannot create playback stream"))?;
        let attr = BufferAttr {
            maxlength: 48_000 * 4 / 10,
            tlength: 48_000 * 4 / 25,
            prebuf: 0,
            minreq: 48_000 * 4 / 100,
            fragsize: 48_000 * 4 / 100,
        };
        record
            .connect_record(
                config.input_device.as_deref(),
                Some(&attr),
                FlagSet::ADJUST_LATENCY,
            )
            .map_err(error)?;
        output
            .connect_playback(
                config.output_device.as_deref(),
                Some(&attr),
                FlagSet::ADJUST_LATENCY,
                None,
                None,
            )
            .map_err(error)?;
        wait_ready(|| {
            iterate(&mut mainloop)?;
            for stream in [&record, &output] {
                if matches!(stream.get_state(), State::Failed | State::Terminated) {
                    return Err(error("PulseAudio stream failed"));
                }
            }
            Ok(record.get_state() == State::Ready && output.get_state() == State::Ready)
        })?;
        let generation = playback.generation()?;
        Ok(Self {
            record,
            output,
            context,
            mainloop,
            playback,
            capture,
            generation,
        })
    }
}

fn iterate(mainloop: &mut Mainloop) -> Result<(), AudioError> {
    match mainloop.iterate(false) {
        IterateResult::Success(_) => Ok(()),
        _ => Err(error("PulseAudio mainloop stopped")),
    }
}

impl DeviceBackend for Backend {
    fn aec_status(&self) -> AecStatus {
        AecStatus::Unknown
    }
    fn poll(&mut self) -> Result<(), AudioError> {
        iterate(&mut self.mainloop)?;
        if self.record.get_state() != State::Ready || self.output.get_state() != State::Ready {
            return Err(error("PulseAudio device disconnected"));
        }
        // Clear is checked before reading any queued output. No old producer can
        // enqueue after the generation change, and the server buffer is flushed.
        let generation = self.playback.generation()?;
        if generation != self.generation {
            self.output.flush(None);
            self.generation = generation;
        }
        for _ in 0..8 {
            let samples = match self.record.peek().map_err(error)? {
                PeekResult::Empty => break,
                PeekResult::Hole(_) => None,
                PeekResult::Data(bytes) => {
                    if bytes.len() > 48_000 * 4 || bytes.len() % 4 != 0 {
                        return Err(error("invalid PulseAudio capture extent"));
                    }
                    Some(
                        bytes
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect(),
                    )
                }
            };
            self.record.discard().map_err(error)?;
            if let Some(samples) = samples {
                let _ = self.capture.try_send(samples);
            }
        }
        // Fill the whole server request. Writing only one 10ms chunk per poll
        // adds the polling delay to every request and eventually underruns even
        // when the producer and device have the same nominal sample rate.
        let writable_frames = self.output.writable_size().unwrap_or(0) / 4;
        let frames = writable_frames.min(4_800);
        if frames > 0 {
            let mut samples = [0.0f32; 4_800];
            self.playback.drain(&mut samples[..frames])?;
            let mut bytes = [0u8; 4_800 * 4];
            for (sample, out) in samples[..frames]
                .iter()
                .zip(bytes[..frames * 4].chunks_exact_mut(4))
            {
                out.copy_from_slice(&sample.to_le_bytes());
            }
            self.output
                .write(&bytes[..frames * 4], None, 0, SeekMode::Relative)
                .map_err(error)?;
        }
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.record.disconnect();
        let _ = self.output.disconnect();
        self.context.disconnect();
    }
}
