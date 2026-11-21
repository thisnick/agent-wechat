// VoiceProcessingIO and HAL audio device setup adapted from sys-voice; see THIRD-PARTY.md.
use super::{parse_macos_device_id, DeviceBackend, NATIVE_RATE};
use crate::{AecStatus, AudioConfig, AudioDevice, AudioError, PlaybackHandle};
use coreaudio::audio_unit::macos_helpers::{
    get_audio_device_ids, get_default_device_id, get_device_name,
};
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::types::IOType;
use coreaudio::audio_unit::{AudioUnit, Element, SampleFormat, Scope, StreamFormat};
use coreaudio::sys::{
    kAudioDevicePropertyStreamConfiguration, kAudioHardwareNoError,
    kAudioObjectPropertyElementWildcard, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput, kAudioOutputUnitProperty_CurrentDevice,
    kAudioOutputUnitProperty_EnableIO, AudioBuffer, AudioBufferList, AudioDeviceID,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectPropertyAddress,
};
use std::ptr::null;
use std::sync::mpsc::SyncSender;

pub(super) struct Backend {
    units: Vec<AudioUnit>,
    aec: AecStatus,
    capture_rate: u32,
}

fn error(e: impl std::fmt::Debug) -> AudioError {
    AudioError::Backend(format!("CoreAudio: {e:?}"))
}

fn stream_format(sample_rate: u32) -> StreamFormat {
    StreamFormat {
        sample_rate: f64::from(sample_rate),
        sample_format: SampleFormat::F32,
        flags: coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_FLOAT
            | coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_PACKED
            | coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_NON_INTERLEAVED,
        channels: 1,
    }
}

/// Safe aligned replacement for `macos_helpers::get_audio_device_supports_scope`
/// that handles multi-buffer (`mNumberBuffers > 1`) CoreAudio devices without
/// indexing past the `[AudioBuffer; 1]` trailing array declaration.
fn device_supports_scope(dev_id: AudioDeviceID, input: bool) -> Result<bool, AudioError> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: if input {
            kAudioObjectPropertyScopeInput
        } else {
            kAudioObjectPropertyScopeOutput
        },
        mElement: kAudioObjectPropertyElementWildcard,
    };
    let mut data_size: u32 = 0;
    let status =
        unsafe { AudioObjectGetPropertyDataSize(dev_id, &address, 0, null(), &mut data_size) };
    if status != kAudioHardwareNoError as i32 {
        return Err(error(status));
    }
    let byte_len = data_size as usize;
    if byte_len < std::mem::size_of::<u32>() {
        return Ok(false);
    }
    // Allocate as u64 words so AudioBufferList alignment requirements are always met.
    let mut storage = vec![0u64; byte_len.div_ceil(std::mem::size_of::<u64>())];
    let capacity_bytes = storage.len() * std::mem::size_of::<u64>();
    let list_ptr = storage.as_mut_ptr().cast::<AudioBufferList>();
    let status = unsafe {
        AudioObjectGetPropertyData(dev_id, &address, 0, null(), &mut data_size, list_ptr.cast())
    };
    if status != kAudioHardwareNoError as i32 {
        return Err(error(status));
    }
    let written = (data_size as usize).min(capacity_bytes);
    let header_offset = std::mem::offset_of!(AudioBufferList, mBuffers);
    if written < header_offset {
        return Ok(false);
    }
    let buffer_size = std::mem::size_of::<AudioBuffer>();
    let count = unsafe { (*list_ptr).mNumberBuffers } as usize;
    let max_count = (written - header_offset) / buffer_size;
    let buffers_ptr = unsafe { std::ptr::addr_of!((*list_ptr).mBuffers).cast::<AudioBuffer>() };
    for index in 0..count.min(max_count) {
        let channels = unsafe { (*buffers_ptr.add(index)).mNumberChannels };
        if channels > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn devices() -> Result<Vec<AudioDevice>, AudioError> {
    let ids = get_audio_device_ids().map_err(error)?;
    let default_input = get_default_device_id(true);
    let default_output = get_default_device_id(false);
    let mut result = Vec::new();
    for dev_id in ids.into_iter().take(1024) {
        let name = match get_device_name(dev_id) {
            Ok(name) if !name.trim().is_empty() => name,
            _ => format!("Audio device {dev_id}"),
        };
        if device_supports_scope(dev_id, true).unwrap_or(false) {
            result.push(AudioDevice {
                input: true,
                id: Some(dev_id.to_string()),
                name: name.clone(),
                is_default: default_input == Some(dev_id),
            });
        }
        if device_supports_scope(dev_id, false).unwrap_or(false) {
            result.push(AudioDevice {
                input: false,
                id: Some(dev_id.to_string()),
                name,
                is_default: default_output == Some(dev_id),
            });
        }
    }
    Ok(result)
}

fn resolve_device(
    requested: Option<&str>,
    default_id: Option<AudioDeviceID>,
    input: bool,
) -> Result<(AudioDeviceID, bool), AudioError> {
    let Some(raw) = requested else {
        let id = default_id.ok_or_else(|| {
            AudioError::Backend(format!(
                "CoreAudio: no default {} device available",
                if input { "input" } else { "output" }
            ))
        })?;
        return Ok((id, false));
    };
    let dev_id = parse_macos_device_id(raw)?;
    if !device_supports_scope(dev_id, input)? {
        return Err(AudioError::Backend(format!(
            "CoreAudio: device {dev_id} does not support {}",
            if input { "capture" } else { "playback" }
        )));
    }
    Ok((dev_id, default_id != Some(dev_id)))
}

fn hal_input_unit(device_id: AudioDeviceID) -> Result<(AudioUnit, u32), AudioError> {
    let mut unit = AudioUnit::new(IOType::HalOutput).map_err(error)?;
    let _ = unit.uninitialize();
    unit.set_property(
        kAudioOutputUnitProperty_EnableIO,
        Scope::Input,
        Element::Input,
        Some(&1u32),
    )
    .map_err(error)?;
    unit.set_property(
        kAudioOutputUnitProperty_EnableIO,
        Scope::Output,
        Element::Output,
        Some(&0u32),
    )
    .map_err(error)?;
    unit.set_property(
        kAudioOutputUnitProperty_CurrentDevice,
        Scope::Global,
        Element::Output,
        Some(&device_id),
    )
    .map_err(error)?;
    // AUHAL input requires the client-side output scope sample rate to match the
    // selected hardware device's native rate (e.g. 16 kHz Bluetooth HFP or 44.1 kHz USB).
    let capture_rate = unit
        .stream_format(Scope::Input, Element::Input)
        .ok()
        .map(|fmt| fmt.sample_rate.round() as u32)
        .filter(|rate| (8_000..=192_000).contains(rate))
        .unwrap_or(NATIVE_RATE);
    unit.set_stream_format(stream_format(capture_rate), Scope::Output, Element::Input)
        .map_err(error)?;
    Ok((unit, capture_rate))
}

fn hal_output_unit(device_id: AudioDeviceID) -> Result<AudioUnit, AudioError> {
    let mut unit = AudioUnit::new(IOType::HalOutput).map_err(error)?;
    let _ = unit.uninitialize();
    unit.set_property(
        kAudioOutputUnitProperty_CurrentDevice,
        Scope::Global,
        Element::Output,
        Some(&device_id),
    )
    .map_err(error)?;
    unit.set_stream_format(stream_format(NATIVE_RATE), Scope::Input, Element::Output)
        .map_err(error)?;
    Ok(unit)
}

impl Backend {
    pub(super) fn new(
        config: &AudioConfig,
        playback: PlaybackHandle,
        capture: SyncSender<Vec<f32>>,
    ) -> Result<Self, AudioError> {
        let default_input = get_default_device_id(true);
        let default_output = get_default_device_id(false);
        let (input_id, custom_input) =
            resolve_device(config.input_device.as_deref(), default_input, true)?;
        let (output_id, custom_output) =
            resolve_device(config.output_device.as_deref(), default_output, false)?;

        if !custom_input && !custom_output {
            return Self::open_voice_processing(playback, capture);
        }
        Self::open_custom_hal(input_id, output_id, playback, capture)
    }

    fn open_voice_processing(
        playback: PlaybackHandle,
        capture: SyncSender<Vec<f32>>,
    ) -> Result<Self, AudioError> {
        let mut unit = AudioUnit::new(IOType::VoiceProcessingIO).map_err(error)?;
        let _ = unit.uninitialize();
        unit.set_property(
            kAudioOutputUnitProperty_EnableIO,
            Scope::Input,
            Element::Input,
            Some(&1u32),
        )
        .map_err(error)?;
        let format = stream_format(NATIVE_RATE);
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
        Ok(Self {
            units: vec![unit],
            aec: AecStatus::Requested,
            capture_rate: NATIVE_RATE,
        })
    }

    fn open_custom_hal(
        input_id: AudioDeviceID,
        output_id: AudioDeviceID,
        playback: PlaybackHandle,
        capture: SyncSender<Vec<f32>>,
    ) -> Result<Self, AudioError> {
        let (mut input_unit, capture_rate) = hal_input_unit(input_id)?;
        let max_callback_samples = (capture_rate as usize / 10).max(4_800);
        input_unit
            .set_input_callback(
                move |args: render_callback::Args<data::NonInterleaved<f32>>| {
                    if let Some(samples) = args.data.channels().next() {
                        if samples.len() <= max_callback_samples {
                            let _ = capture.try_send(samples.to_vec());
                        }
                    }
                    Ok(())
                },
            )
            .map_err(error)?;

        let mut output_unit = hal_output_unit(output_id)?;
        output_unit
            .set_render_callback(
                move |mut args: render_callback::Args<data::NonInterleaved<f32>>| {
                    if let Some(samples) = args.data.channels_mut().next() {
                        playback.render_callback(samples);
                    }
                    Ok(())
                },
            )
            .map_err(error)?;

        input_unit.initialize().map_err(error)?;
        if let Err(err) = output_unit.initialize() {
            let _ = input_unit.uninitialize();
            return Err(error(err));
        }
        if let Err(err) = input_unit.start() {
            let _ = input_unit.uninitialize();
            let _ = output_unit.uninitialize();
            return Err(error(err));
        }
        if let Err(err) = output_unit.start() {
            let _ = input_unit.stop();
            let _ = input_unit.uninitialize();
            let _ = output_unit.uninitialize();
            return Err(error(err));
        }
        Ok(Self {
            units: vec![input_unit, output_unit],
            aec: AecStatus::Unavailable,
            capture_rate,
        })
    }
}

impl DeviceBackend for Backend {
    fn aec_status(&self) -> AecStatus {
        self.aec
    }
    fn capture_rate(&self) -> u32 {
        self.capture_rate
    }
    fn poll(&mut self) -> Result<(), AudioError> {
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        for unit in &mut self.units {
            let _ = unit.stop();
            let _ = unit.uninitialize();
        }
    }
}
