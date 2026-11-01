// Adapted from sys-voice 0.2.0 and the acpfx fork; see THIRD-PARTY.md.
use crate::AudioError;
use rubato::{FftFixedIn, Resampler as _};

pub(crate) fn valid_rate(rate: u32) -> bool {
    matches!(
        rate,
        8_000
            | 11_025
            | 12_000
            | 16_000
            | 22_050
            | 24_000
            | 32_000
            | 44_100
            | 48_000
            | 88_200
            | 96_000
            | 176_400
            | 192_000
    )
}

pub(crate) struct Resampler {
    inner: FftFixedIn<f32>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    pending: Vec<f32>,
    chunk: usize,
}

impl Resampler {
    pub(crate) fn new(from: u32, to: u32) -> Result<Self, AudioError> {
        if !valid_rate(from) || !valid_rate(to) {
            return Err(AudioError::InvalidConfig);
        }
        let chunk = from as usize / 100;
        let inner = FftFixedIn::new(from as usize, to as usize, chunk, 1, 1)
            .map_err(|e| AudioError::Backend(e.to_string()))?;
        Ok(Self {
            input: inner.input_buffer_allocate(true),
            output: inner.output_buffer_allocate(true),
            inner,
            pending: Vec::with_capacity(chunk * 2),
            chunk,
        })
    }

    pub(crate) fn process(&mut self, samples: &[f32]) -> Result<Vec<f32>, AudioError> {
        // Limit one submission independently of queue length.
        if samples.len() > 192_000 || samples.iter().any(|s| !s.is_finite()) {
            return Err(AudioError::InvalidConfig);
        }
        self.pending.extend_from_slice(samples);
        let mut output = Vec::new();
        while self.pending.len() >= self.chunk {
            self.input[0].copy_from_slice(&self.pending[..self.chunk]);
            let (_, n) = self
                .inner
                .process_into_buffer(&self.input, &mut self.output, None)
                .map_err(|e| AudioError::Backend(e.to_string()))?;
            output.extend_from_slice(&self.output[0][..n]);
            self.pending.drain(..self.chunk);
        }
        Ok(output)
    }
}
