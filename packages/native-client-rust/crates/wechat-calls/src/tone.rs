//! Synthetic pulsed PCM for an operator-approved audio check.
//!
//! Generating samples has no device, account or network effect. The caller must
//! separately authorize and route test audio to the intended call participant.

use std::{f64::consts::TAU, time::Duration};

use crate::audio::{AudioFormat, MediaError};

pub struct PulseTone {
    format: AudioFormat,
    frequency: f64,
    amplitude: f64,
    on_samples: u64,
    period_samples: u64,
    position: u64,
    phase: f64,
}

impl PulseTone {
    /// `amplitude` is a fraction of full-scale PCM. Pulses include short ramps
    /// to avoid hard transitions; test with conservative playback volume.
    pub fn new(
        format: AudioFormat,
        frequency_hz: f64,
        amplitude: f64,
        on: Duration,
        off: Duration,
    ) -> Result<Self, MediaError> {
        if !frequency_hz.is_finite()
            || frequency_hz <= 0.0
            || frequency_hz >= f64::from(format.sample_rate()) / 2.0
            || !amplitude.is_finite()
            || !(0.0..=1.0).contains(&amplitude)
        {
            return Err(MediaError::InvalidConfiguration);
        }
        let samples = |duration: Duration| {
            duration
                .as_nanos()
                .checked_mul(u128::from(format.sample_rate()))
                .and_then(|n| u64::try_from(n / 1_000_000_000).ok())
                .ok_or(MediaError::InvalidConfiguration)
        };
        let on_samples = samples(on)?;
        let off_samples = samples(off)?;
        let period_samples = on_samples
            .checked_add(off_samples)
            .ok_or(MediaError::Capacity)?;
        if on_samples == 0 || off_samples == 0 {
            return Err(MediaError::InvalidConfiguration);
        }
        Ok(Self {
            format,
            frequency: frequency_hz,
            amplitude,
            on_samples,
            period_samples,
            position: 0,
            phase: 0.0,
        })
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn next_frame(&mut self) -> Result<Vec<i16>, MediaError> {
        let mut output = Vec::with_capacity(self.format.samples_per_frame());
        let step = TAU * self.frequency / f64::from(self.format.sample_rate());
        let ramp = u64::from(self.format.sample_rate() / 200)
            .min(self.on_samples / 2)
            .max(1);
        for _ in 0..self.format.samples_per_channel() {
            let pulse_position = self.position;
            let sample = if pulse_position < self.on_samples {
                let envelope = (pulse_position.min(self.on_samples - 1 - pulse_position) as f64
                    / ramp as f64)
                    .min(1.0);
                (self.phase.sin() * self.amplitude * envelope * f64::from(i16::MAX)).round() as i16
            } else {
                0
            };
            output.extend(std::iter::repeat_n(sample, self.format.channels() as usize));
            self.phase = (self.phase + step) % TAU;
            // The bounded period position cannot wrap a lifetime counter.
            self.position = if self.position == self.period_samples - 1 {
                0
            } else {
                self.position + 1
            };
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulses_have_real_silence_and_identical_stereo_channels() {
        let format = AudioFormat::new(16000, 2, 20).unwrap();
        let mut tone = PulseTone::new(
            format,
            880.0,
            0.2,
            Duration::from_millis(40),
            Duration::from_millis(20),
        )
        .unwrap();
        let a = tone.next_frame().unwrap();
        assert_eq!(a.len(), 640);
        assert!(a.iter().any(|&sample| sample.abs() > 1000));
        assert!(a.iter().all(|&sample| sample.abs() <= 6554));
        assert!(a.chunks_exact(2).all(|p| p[0] == p[1]));
        tone.next_frame().unwrap();
        assert!(tone.next_frame().unwrap().iter().all(|&sample| sample == 0));
        assert!(tone.next_frame().unwrap().iter().any(|&sample| sample != 0));
    }

    #[test]
    fn rejects_invalid_frequency_amplitude_and_zero_pulse_periods() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        for (frequency, amplitude) in [
            (f64::NAN, 0.2),
            (880.0, f64::INFINITY),
            (8000.0, 0.2),
            (880.0, 1.1),
        ] {
            assert!(PulseTone::new(
                format,
                frequency,
                amplitude,
                Duration::from_millis(20),
                Duration::from_millis(20)
            )
            .is_err());
        }
        assert!(PulseTone::new(
            format,
            880.0,
            0.2,
            Duration::ZERO,
            Duration::from_millis(20)
        )
        .is_err());
    }
}
