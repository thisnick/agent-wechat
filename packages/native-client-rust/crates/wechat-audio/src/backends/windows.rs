// WASAPI setup adapted from sys-voice; see THIRD-PARTY.md.
use super::{DeviceBackend, NATIVE_RATE};
use crate::{AecStatus, AudioConfig, AudioError, PlaybackHandle};
use std::sync::mpsc::SyncSender;
use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

struct ComApartment;
impl Drop for ComApartment {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

pub(super) struct Backend {
    input: wasapi::AudioClient,
    output: wasapi::AudioClient,
    reader: wasapi::AudioCaptureClient,
    writer: wasapi::AudioRenderClient,
    input_format: WaveFormat,
    output_format: WaveFormat,
    playback: PlaybackHandle,
    capture: SyncSender<Vec<f32>>,
    generation: u64,
    input_buffer: Vec<u8>,
    aec: AecStatus,
    _com: ComApartment,
}
fn error(e: impl std::fmt::Debug) -> AudioError {
    AudioError::Backend(format!("WASAPI: {e:?}"))
}

impl Backend {
    pub(super) fn new(
        config: &AudioConfig,
        playback: PlaybackHandle,
        capture: SyncSender<Vec<f32>>,
    ) -> Result<Self, AudioError> {
        let hr = wasapi::initialize_mta();
        if hr.0 < 0 {
            return Err(error(hr));
        }
        let com = ComApartment;
        let enumerator = DeviceEnumerator::new().map_err(error)?;
        let input_device = match &config.input_device {
            Some(id) => enumerator.get_device(id).map_err(error)?,
            None => enumerator
                .get_default_device(&Direction::Capture)
                .map_err(error)?,
        };
        let output_device = match &config.output_device {
            Some(id) => enumerator.get_device(id).map_err(error)?,
            None => enumerator
                .get_default_device(&Direction::Render)
                .map_err(error)?,
        };
        let mut input = input_device.get_iaudioclient().map_err(error)?;
        let mut output = output_device.get_iaudioclient().map_err(error)?;
        let desired = WaveFormat::new(32, 32, &SampleType::Float, NATIVE_RATE as usize, 1, None);
        // Require the shared engine conversion to our fixed f32/mono/48k contract.
        // Never reinterpret an unsupported device's native format as this one.
        let input_format = desired.clone();
        let output_format = desired;
        let mode = StreamMode::PollingShared {
            autoconvert: true,
            buffer_duration_hns: 200_000,
        };
        input
            .set_properties(
                wasapi::AudioClientProperties::new()
                    .set_category(wasapi::StreamCategory::Communications),
            )
            .map_err(error)?;
        output
            .set_properties(
                wasapi::AudioClientProperties::new()
                    .set_category(wasapi::StreamCategory::Communications),
            )
            .map_err(error)?;
        input
            .initialize_client(&input_format, &Direction::Capture, &mode)
            .map_err(error)?;
        output
            .initialize_client(&output_format, &Direction::Render, &mode)
            .map_err(error)?;
        let aec = match input.get_aec_control() {
            Ok(control) => match output_device.get_id() {
                Ok(id) => {
                    if control
                        .set_echo_cancellation_render_endpoint(Some(id))
                        .is_ok()
                    {
                        AecStatus::Requested
                    } else {
                        AecStatus::Unknown
                    }
                }
                Err(_) => AecStatus::Unknown,
            },
            Err(_) => AecStatus::Unavailable,
        };
        let reader = input.get_audiocaptureclient().map_err(error)?;
        let writer = output.get_audiorenderclient().map_err(error)?;
        let input_frames = input.get_buffer_size().map_err(error)? as usize;
        if input_frames == 0 || input_frames > 48_000 {
            return Err(error("invalid capture buffer size"));
        }
        let generation = playback.generation()?;
        input.start_stream().map_err(error)?;
        if let Err(e) = output.start_stream() {
            let _ = input.stop_stream();
            return Err(error(e));
        }
        Ok(Self {
            input,
            output,
            reader,
            writer,
            input_format,
            output_format,
            playback,
            capture,
            generation,
            input_buffer: vec![0; input_frames * 4],
            aec,
            _com: com,
        })
    }
}

impl DeviceBackend for Backend {
    fn aec_status(&self) -> AecStatus {
        self.aec
    }
    fn poll(&mut self) -> Result<(), AudioError> {
        let generation = self.playback.generation()?;
        if generation != self.generation {
            self.output.stop_stream().map_err(error)?;
            self.output.reset_stream().map_err(error)?;
            self.output.start_stream().map_err(error)?;
            self.generation = generation;
        }
        if self
            .reader
            .get_next_packet_size()
            .map_err(error)?
            .unwrap_or(0)
            > 0
        {
            let (frames, _) = self
                .reader
                .read_from_device(&mut self.input_buffer)
                .map_err(error)?;
            let bytes = frames as usize * self.input_format.get_blockalign() as usize;
            if bytes > self.input_buffer.len() {
                return Err(error("capture buffer extent"));
            }
            let samples = self.input_buffer[..bytes]
                .chunks_exact(4)
                .map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
                .collect();
            let _ = self.capture.try_send(samples);
        }
        let frames = self
            .output
            .get_available_space_in_frames()
            .map_err(error)?
            .min(4_800) as usize;
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
            debug_assert_eq!(self.output_format.get_blockalign(), 4);
            self.writer
                .write_to_device(frames, &bytes[..frames * 4], None)
                .map_err(error)?;
        }
        Ok(())
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.input.stop_stream();
        let _ = self.output.stop_stream();
    }
}
