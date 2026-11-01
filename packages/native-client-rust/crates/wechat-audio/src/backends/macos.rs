// VoiceProcessingIO setup adapted from sys-voice; see THIRD-PARTY.md.
use super::{DeviceBackend, NATIVE_RATE};
use crate::{AecStatus, AudioConfig, AudioError, PlaybackHandle};
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::types::IOType;
use coreaudio::audio_unit::{AudioUnit, Element, SampleFormat, Scope, StreamFormat};
use std::sync::mpsc::SyncSender;

pub(super) struct Backend {
    unit: AudioUnit,
}
fn error(e: impl std::fmt::Debug) -> AudioError {
    AudioError::Backend(format!("CoreAudio: {e:?}"))
}

impl Backend {
    pub(super) fn new(
        config: &AudioConfig,
        playback: PlaybackHandle,
        capture: SyncSender<Vec<f32>>,
    ) -> Result<Self, AudioError> {
        if config.input_device.is_some() || config.output_device.is_some() {
            return Err(AudioError::Backend(
                "custom CoreAudio device selection is not supported by this adapter yet".into(),
            ));
        }
        let mut unit = AudioUnit::new(IOType::VoiceProcessingIO).map_err(error)?;
        let _ = unit.uninitialize();
        unit.set_property(
            coreaudio::sys::kAudioOutputUnitProperty_EnableIO,
            Scope::Input,
            Element::Input,
            Some(&1u32),
        )
        .map_err(error)?;
        let format = StreamFormat {
            sample_rate: NATIVE_RATE as f64,
            sample_format: SampleFormat::F32,
            flags: coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_FLOAT
                | coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_PACKED
                | coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_NON_INTERLEAVED,
            channels: 1,
        };
        unit.set_stream_format(format, Scope::Output, Element::Input)
            .map_err(error)?;
        unit.set_stream_format(format, Scope::Input, Element::Output)
            .map_err(error)?;
        unit.set_input_callback(
            move |args: render_callback::Args<data::NonInterleaved<f32>>| {
                if let Some(samples) = args.data.channels().next() {
                    if samples.len() <= 4_800 {
                        let _ = capture.try_send(samples.to_vec());
                    }
                }
                Ok(())
            },
        )
        .map_err(error)?;
        unit.set_render_callback(
            move |mut args: render_callback::Args<data::NonInterleaved<f32>>| {
                if let Some(samples) = args.data.channels_mut().next() {
                    playback.render_callback(samples);
                }
                Ok(())
            },
        )
        .map_err(error)?;
        unit.initialize().map_err(error)?;
        unit.start().map_err(error)?;
        Ok(Self { unit })
    }
}
impl DeviceBackend for Backend {
    fn aec_status(&self) -> AecStatus {
        AecStatus::Requested
    }
    fn poll(&mut self) -> Result<(), AudioError> {
        Ok(())
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.unit.stop();
        let _ = self.unit.uninitialize();
    }
}
