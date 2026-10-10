//! Owned fixed-ratio resampling. The `rubato` feature selects this backend.
//!
//! Linear FFT/sinc arithmetic preserves the former Rubato 4.0 response (MIT;
//! see NOTICE). The stream owns fixed storage and an exact duration budget.
//! Half-band and nonlinear filters retain their existing project-owned kernels.

use super::polyphase_backend::gcd;
use super::{BackendInitError as InitError, BackendProcessError as ProcessError, BackendProgress};
use crate::config::{PhaseResponse, ResampleQuality as Quality};

mod block;
mod clock;
mod dot;
mod fft;
mod sinc;

pub(super) const BACKEND_NAME: &str = "rust";

#[derive(Clone, Copy)]
struct Config {
    from: u32,
    to: u32,
    channels: usize,
    quality: Quality,
}

impl Config {
    fn validate(self) -> Result<(), InitError> {
        if self.channels == 0 {
            return Err(InitError::ZeroChannels);
        }
        if self.from == 0 || self.to == 0 {
            return Err(InitError::InvalidGeometry {
                backend: BACKEND_NAME,
            });
        }
        Ok(())
    }

    fn output_frames(self, input: u64) -> Result<u64, ProcessError> {
        // Ordinary streams fit in one machine word. Preserve the exact wide
        // fallback for long streams and extreme public rate/count geometry.
        if let Some(numerator) = input
            .checked_mul(u64::from(self.to) * 2)
            .and_then(|value| value.checked_add(u64::from(self.from)))
        {
            return Ok(numerator / (u64::from(self.from) * 2));
        }
        u64::try_from(
            (input as u128 * self.to as u128 * 2 + self.from as u128) / (self.from as u128 * 2),
        )
        .map_err(|_| ProcessError::new("resampler output duration overflow"))
    }
}

pub(super) fn samples(
    frames: usize,
    channels: usize,
    buffer: &'static str,
) -> Result<usize, InitError> {
    frames
        .checked_mul(channels)
        .filter(|&n| n <= isize::MAX as usize / std::mem::size_of::<f64>())
        .ok_or(InitError::StorageOverflow { buffer })
}

pub(super) fn zeroed(
    frames: usize,
    channels: usize,
    buffer: &'static str,
) -> Result<Vec<f64>, InitError> {
    let n = samples(frames, channels, buffer)?;
    let mut result = Vec::new();
    result
        .try_reserve_exact(n)
        .map_err(|error| InitError::Backend {
            message: format!("cannot reserve {buffer}: {error}"),
        })?;
    result.resize(n, 0.0);
    Ok(result)
}

enum Engine {
    Identity { channels: usize },
    Block(Box<block::BlockStream>),
    Sinc(Box<sinc::SincStream>),
}

pub(super) struct MonoBackend {
    inner: Engine,
}

impl MonoBackend {
    pub(super) fn new(
        from: u32,
        to: u32,
        phase: PhaseResponse,
        quality: Quality,
    ) -> Result<Self, InitError> {
        Self::new_interleaved(from, to, phase, quality, 1)
    }

    pub(super) fn new_interleaved(
        from: u32,
        to: u32,
        phase: PhaseResponse,
        quality: Quality,
        channels: usize,
    ) -> Result<Self, InitError> {
        let config = Config {
            from,
            to,
            channels,
            quality,
        };
        config.validate()?;
        if from == to {
            return Ok(Self {
                inner: Engine::Identity { channels },
            });
        }
        let divisor = gcd(from, to);
        if phase == PhaseResponse::Linear && (from / divisor > 1024 || to / divisor > 1024) {
            Ok(Self {
                inner: Engine::Sinc(Box::new(sinc::SincStream::new(config)?)),
            })
        } else {
            Ok(Self {
                inner: Engine::Block(Box::new(block::BlockStream::new(config, phase)?)),
            })
        }
    }

    pub(super) fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<BackendProgress, ProcessError> {
        match &mut self.inner {
            Engine::Identity { channels } => {
                if !input.len().is_multiple_of(*channels) || !output.len().is_multiple_of(*channels)
                {
                    return Err("incomplete identity frame".into());
                }
                let n = input.len().min(output.len());
                output[..n].copy_from_slice(&input[..n]);
                Ok(BackendProgress {
                    input_frames: n / *channels,
                    output_frames: n / *channels,
                })
            }
            Engine::Block(stream) => stream.process(input, output),
            Engine::Sinc(stream) => stream.process(input, output),
        }
    }

    pub(super) fn drain(&mut self, output: &mut [f64]) -> Result<usize, ProcessError> {
        match &mut self.inner {
            Engine::Identity { .. } => Ok(0),
            Engine::Block(stream) => stream.finish(output),
            Engine::Sinc(stream) => stream.finish(output),
        }
    }

    pub(super) fn clear(&mut self) -> Result<(), ProcessError> {
        match &mut self.inner {
            Engine::Identity { .. } => Ok(()),
            Engine::Block(stream) => {
                stream.reset();
                Ok(())
            }
            Engine::Sinc(stream) => stream.reset(),
        }
    }

    pub(super) fn latency_frames(&self) -> usize {
        match &self.inner {
            Engine::Block(stream) => stream.latency,
            _ => 0,
        }
    }
    pub(super) fn finish_extension_frames(&self) -> usize {
        match &self.inner {
            Engine::Block(stream) => stream.extension,
            _ => 0,
        }
    }
}

#[cfg(test)]
pub(super) fn expected_output_frames(input: u64, from: u32, to: u32) -> u64 {
    ((input as u128 * to as u128 * 2 + from as u128) / (from as u128 * 2)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_fast_path_and_overflow_fallback_match_quotient_remainder_oracle() {
        for (from, to) in [(44100, 48000), (48000, 1001), (1, u32::MAX), (u32::MAX, 1)] {
            let config = Config {
                from,
                to,
                channels: 1,
                quality: Quality::Standard,
            };
            let boundary = (u64::MAX - u64::from(from)) / (u64::from(to) * 2);
            for input in [
                0,
                1,
                4099,
                1 << 53,
                boundary - 1,
                boundary,
                boundary + 1,
                u64::MAX,
            ] {
                let product = u128::from(input) * u128::from(to);
                let quotient = product / u128::from(from);
                let remainder = product % u128::from(from);
                let rounded = quotient + u128::from(remainder * 2 >= u128::from(from));
                match u64::try_from(rounded) {
                    Ok(expected) => assert_eq!(config.output_frames(input).unwrap(), expected),
                    Err(_) => assert!(config.output_frames(input).is_err()),
                }
            }
        }
    }
}
