//! One bounded stream adapter for the owned fixed-block kernels.
use super::super::{
    contiguous_polyphase_backend::ContiguousPolyphaseResampler,
    halfband_backend::Halfband2xResampler, spectral_backend::SpectralNonlinearResampler,
};
use super::{
    fft::{reference_filter, FftKernel, Geometry},
    gcd, zeroed, BackendProgress, Config, InitError, ProcessError, Quality,
};
use crate::config::PhaseResponse;

const CHUNK: usize = 1024;

enum Kernel {
    Fft(Box<FftKernel>),
    Halfband(Box<Halfband2xResampler>),
    Spectral(Box<SpectralNonlinearResampler>),
    Polyphase(Box<ContiguousPolyphaseResampler>),
}

impl Kernel {
    fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<(usize, usize), ProcessError> {
        match self {
            Self::Fft(kernel) => {
                kernel.block(input, output)?;
                Ok((kernel.geometry.input, kernel.geometry.output))
            }
            Self::Halfband(kernel) => kernel.process_chunk(input, output),
            Self::Spectral(kernel) => kernel.process_chunk(input, output),
            Self::Polyphase(kernel) => kernel.process_chunk(input, output),
        }
    }
    fn reset(&mut self) {
        match self {
            Self::Fft(kernel) => kernel.reset(),
            Self::Halfband(kernel) => kernel.reset(),
            Self::Spectral(kernel) => kernel.reset(),
            Self::Polyphase(kernel) => kernel.reset(),
        }
    }
}

pub(super) struct BlockStream {
    config: Config,
    kernel: Kernel,
    input: Vec<f64>,
    filled: usize,
    pending: Vec<f64>,
    start: usize,
    end: usize,
    skip: usize,
    initial_skip: usize,
    total_input: u64,
    authorized_output: u64,
    emitted: u64,
    ending: bool,
    pub(super) latency: usize,
    pub(super) extension: usize,
}

impl BlockStream {
    pub(super) fn new(config: Config, phase: PhaseResponse) -> Result<Self, InitError> {
        let (kernel, ni, no, skip, latency, extension) = if phase != PhaseResponse::Linear {
            if config.to / gcd(config.from, config.to) <= 16 {
                let k = SpectralNonlinearResampler::new(
                    config.from,
                    config.to,
                    phase,
                    config.quality,
                    config.channels,
                    CHUNK,
                )?;
                let result = (
                    k.output_frames_max(),
                    k.output_delay(),
                    k.latency_frames(),
                    k.finish_extension_frames(),
                );
                (
                    Kernel::Spectral(Box::new(k)),
                    CHUNK,
                    result.0,
                    result.1,
                    result.2,
                    result.3,
                )
            } else {
                let k = ContiguousPolyphaseResampler::new(
                    config.from,
                    config.to,
                    phase,
                    config.quality,
                    config.channels,
                    CHUNK,
                )?;
                let result = (
                    k.output_frames_max(),
                    k.output_delay(),
                    k.latency_frames(),
                    k.finish_extension_frames(),
                );
                (
                    Kernel::Polyphase(Box::new(k)),
                    CHUNK,
                    result.0,
                    result.1,
                    result.2,
                    result.3,
                )
            }
        } else if config.quality == Quality::High && config.from.checked_mul(2) == Some(config.to) {
            let k = Halfband2xResampler::new(config.channels, CHUNK)?;
            let result = (k.output_frames_max(), k.output_delay());
            (
                Kernel::Halfband(Box::new(k)),
                CHUNK,
                result.0,
                result.1,
                0,
                0,
            )
        } else {
            let g = Geometry::reference(config)?;
            let k = FftKernel::new(config, g, &reference_filter(g))?;
            (
                Kernel::Fft(Box::new(k)),
                g.input,
                g.output,
                g.output / 2,
                0,
                0,
            )
        };
        Ok(Self {
            config,
            kernel,
            input: zeroed(ni, config.channels, "block input")?,
            filled: 0,
            pending: zeroed(no, config.channels, "block output")?,
            start: 0,
            end: 0,
            skip,
            initial_skip: skip,
            total_input: 0,
            authorized_output: 0,
            emitted: 0,
            ending: false,
            latency,
            extension,
        })
    }

    fn validate(&self, input: &[f64], output: &[f64]) -> Result<(), ProcessError> {
        if !input.len().is_multiple_of(self.config.channels)
            || !output.len().is_multiple_of(self.config.channels)
        {
            return Err("incomplete resampler block frame".into());
        }
        Ok(())
    }

    fn budget(&self) -> Result<u64, ProcessError> {
        self.authorized_output
            .checked_add(if self.ending {
                self.extension as u64
            } else {
                0
            })
            .ok_or_else(|| ProcessError::new("resampler finite output duration overflow"))
    }

    fn accept(&mut self, frames: usize) -> Result<(), ProcessError> {
        let total = self
            .total_input
            .checked_add(frames as u64)
            .ok_or("resampler input duration overflow")?;
        let authorized = self.config.output_frames(total)?;
        authorized
            .checked_add(self.extension as u64)
            .ok_or("resampler output duration overflow")?;
        self.total_input = total;
        self.authorized_output = authorized;
        Ok(())
    }

    fn generated(&mut self, consumed: usize, produced: usize) -> Result<(), ProcessError> {
        if consumed != self.input.len() / self.config.channels
            || produced > self.pending.len() / self.config.channels
        {
            return Err("owned resampler kernel returned invalid progress".into());
        }
        self.end = produced;
        self.start = self.skip.min(produced);
        self.skip -= self.start;
        Ok(())
    }

    fn flush(&mut self, output: &mut [f64]) -> Result<usize, ProcessError> {
        if self.start == self.end {
            return Ok(0);
        }
        let c = self.config.channels;
        let budget = self.budget()?.saturating_sub(self.emitted);
        let n = (self.end - self.start)
            .min(output.len() / c)
            .min(budget.min(usize::MAX as u64) as usize);
        output[..n * c].copy_from_slice(&self.pending[self.start * c..(self.start + n) * c]);
        self.start += n;
        self.emitted += n as u64;
        Ok(n)
    }

    pub(super) fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<BackendProgress, ProcessError> {
        self.validate(input, output)?;
        if self.ending {
            return Err("process after resampler finish".into());
        }
        let c = self.config.channels;
        let ni = self.input.len() / c;
        let no = self.pending.len() / c;
        let (mut used, mut produced) = (0, 0);
        while produced < output.len() / c {
            produced += self.flush(&mut output[produced * c..])?;
            if produced == output.len() / c {
                break;
            }
            if self.filled == ni && self.start == self.end {
                let (read, wrote) = self.kernel.process(&self.input, &mut self.pending)?;
                self.generated(read, wrote)?;
                self.filled = 0;
            } else if used == input.len() / c {
                break;
            } else if self.filled == 0 && self.start == self.end && input.len() / c - used >= ni {
                self.accept(ni)?;
                if self.skip == 0
                    && output.len() / c - produced >= no
                    && self.budget()?.saturating_sub(self.emitted) >= no as u64
                {
                    let (read, wrote) = self.kernel.process(
                        &input[used * c..(used + ni) * c],
                        &mut output[produced * c..(produced + no) * c],
                    )?;
                    if read != ni || wrote > no {
                        return Err("owned resampler direct progress overflow".into());
                    }
                    self.emitted += wrote as u64;
                    produced += wrote;
                } else {
                    let (read, wrote) = self
                        .kernel
                        .process(&input[used * c..(used + ni) * c], &mut self.pending)?;
                    self.generated(read, wrote)?;
                }
                used += ni;
            } else {
                let n = (ni - self.filled).min(input.len() / c - used);
                if n == 0 {
                    return Err("resampler pending output exceeded input budget".into());
                }
                self.accept(n)?;
                self.input[self.filled * c..(self.filled + n) * c]
                    .copy_from_slice(&input[used * c..(used + n) * c]);
                self.filled += n;
                used += n;
            }
        }
        // Preserve one input block of lookahead when the caller's output fills.
        // This is bounded by setup storage and keeps ordinary 512-frame feeds
        // fully consumable despite the rational FFT unit's burst boundary.
        if !output.is_empty() && used < input.len() / c {
            let n = (ni - self.filled).min(input.len() / c - used);
            self.accept(n)?;
            self.input[self.filled * c..(self.filled + n) * c]
                .copy_from_slice(&input[used * c..(used + n) * c]);
            self.filled += n;
            used += n;
        }
        Ok(BackendProgress {
            input_frames: used,
            output_frames: produced,
        })
    }

    pub(super) fn finish(&mut self, output: &mut [f64]) -> Result<usize, ProcessError> {
        self.validate(&[], output)?;
        self.ending = true;
        let c = self.config.channels;
        let target = self.budget()?;
        let mut produced = 0;
        let mut stalled = 0;
        while self.emitted < target && produced < output.len() / c {
            let n = self.flush(&mut output[produced * c..])?;
            produced += n;
            if self.emitted == target || produced == output.len() / c {
                break;
            }
            self.input[self.filled * c..].fill(0.0);
            let (read, wrote) = self.kernel.process(&self.input, &mut self.pending)?;
            self.filled = 0;
            self.generated(read, wrote)?;
            stalled = if n == 0 { stalled + 1 } else { 0 };
            if stalled > 64 {
                return Err("owned resampler finite drain stalled".into());
            }
        }
        Ok(produced)
    }

    pub(super) fn reset(&mut self) {
        self.kernel.reset();
        self.filled = 0;
        self.start = 0;
        self.end = 0;
        self.skip = self.initial_skip;
        self.total_input = 0;
        self.authorized_output = 0;
        self.emitted = 0;
        self.ending = false;
    }
}
