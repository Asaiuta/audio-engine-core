//! Pure-Rust mono-channel resampler backend built on rubato 4.
//!
//! This backend adapts rubato's fixed-input-chunk interface to the same
//! streaming semantics the SoXR backend provides natively:
//!
//! - **Arbitrary input granularity.** Caller input of any size is staged in a
//!   pre-allocated input FIFO; rubato only ever sees exact `CHUNK_IN`-frame
//!   chunks, so the produced sample sequence is independent of how the caller
//!   splits its input (chunked and single-feed runs are bitwise identical).
//! - **No leading delay frames.** Both rubato 4 engines used here carry a real
//!   leading delay. The adapter discards `output_delay()` produced frames at
//!   stream start (and again after reset), so callers receive an aligned
//!   sequence within one frame of the backend's delay rounding.
//! - **Duration-aligned drain.** At end of stream the total output is padded
//!   (with zero-fed chunks) or truncated to `round(total_input * to / from)`
//!   frames, after which `drain` reports the terminal zero.
//! - **Allocation-free processing.** All buffers are allocated in `new`;
//!   `process`/`drain`/`clear` stay within pre-reserved capacity and rubato's
//!   `process_into_buffer` is itself allocation-free.
//!
//! Differences from the SoXR backend that callers should know about:
//! rubato's FFT and windowed-sinc engines are **linear phase only**, so the
//! requested [`PhaseResponse`] is accepted but not applied. Common reduced
//! sample-rate ratios use the much faster FFT engine through High quality;
//! UltraHigh and ratios that would create pathological FFT blocks use sinc,
//! where the quality mapping selects sinc length / oversampling rather than a
//! SoX recipe.

use crate::config::{PhaseResponse, ResampleQuality};
use rubato::{
    audioadapter_buffers::direct::InterleavedSlice, Async, Fft, FixedAsync, FixedSync, Resampler,
    SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

use super::BackendProgress;

pub(super) const BACKEND_NAME: &str = "rubato";

/// Fixed rubato input chunk size in frames. The FIFO adaptation makes this an
/// internal detail; changing it changes latency smoothing granularity only.
const CHUNK_IN: usize = 1024;

/// Maximum numerator and denominator of the reduced sample-rate ratio routed
/// through rubato's synchronous FFT engine. Larger components make its FFT
/// block, delay, and per-call output grow with the raw rate pair; for example,
/// 44_100 -> 44_101 would otherwise create a 44_101-frame output block and a
/// 22_050-frame delay. All conventional audio-rate conversions fit this bound.
/// UltraHigh intentionally bypasses this route to preserve the strongest sinc
/// quality tier while the default High tier retains FFT throughput.
const MAX_FFT_REDUCED_RATE: u32 = 1024;

/// FFT sub-chunks selected by the same quality/performance probe that chose
/// the routing threshold. Two preserves the strongest measured stopband while
/// keeping common-ratio throughput close to the native backend.
const FFT_SUB_CHUNKS: usize = 2;

/// Consecutive zero-output flush rounds tolerated during drain before the
/// backend reports a stall instead of looping forever.
const MAX_DRAIN_STALL_ROUNDS: usize = 64;

fn sinc_parameters(quality: ResampleQuality) -> SincInterpolationParameters {
    let (sinc_len, oversampling_factor, interpolation) = match quality {
        ResampleQuality::Low => (64, 128, SincInterpolationType::Linear),
        ResampleQuality::Standard => (128, 256, SincInterpolationType::Cubic),
        ResampleQuality::High => (256, 256, SincInterpolationType::Cubic),
        ResampleQuality::UltraHigh => (256, 512, SincInterpolationType::Cubic),
    };
    SincInterpolationParameters::new(sinc_len, WindowFunction::BlackmanHarris2)
        .oversampling_factor(oversampling_factor)
        .interpolation(interpolation)
}

fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn should_use_fft(from_rate: u32, to_rate: u32, quality: ResampleQuality) -> bool {
    if from_rate == 0 || to_rate == 0 {
        return false;
    }
    let divisor = greatest_common_divisor(from_rate, to_rate);
    !matches!(quality, ResampleQuality::UltraHigh)
        && from_rate / divisor <= MAX_FFT_REDUCED_RATE
        && to_rate / divisor <= MAX_FFT_REDUCED_RATE
}

/// Total output frames a stream of `total_input` frames must produce to be
/// duration-aligned, rounding half up.
pub(super) fn expected_output_frames(total_input: u64, from_rate: u32, to_rate: u32) -> u64 {
    ((total_input as u128 * to_rate as u128 * 2 + from_rate as u128) / (from_rate as u128 * 2))
        as u64
}

enum RubatoEngine {
    Sinc(Async<f64>),
    Fft(Box<Fft<f64>>),
}

impl RubatoEngine {
    fn new(from_rate: u32, to_rate: u32, quality: ResampleQuality) -> Result<Self, String> {
        if should_use_fft(from_rate, to_rate, quality) {
            Fft::<f64>::new_custom(
                from_rate as usize,
                to_rate as usize,
                CHUNK_IN,
                FFT_SUB_CHUNKS,
                1,
                WindowFunction::BlackmanHarris2,
                FixedSync::Input,
            )
            .map(Box::new)
            .map(Self::Fft)
            .map_err(|error| format!("{error}"))
        } else {
            let ratio = to_rate as f64 / from_rate as f64;
            let parameters = sinc_parameters(quality);
            Async::<f64>::new_sinc(ratio, 1.0, &parameters, CHUNK_IN, 1, FixedAsync::Input)
                .map(Self::Sinc)
                .map_err(|error| format!("{error}"))
        }
    }

    fn output_frames_max(&self) -> usize {
        match self {
            Self::Sinc(resampler) => resampler.output_frames_max(),
            Self::Fft(resampler) => resampler.output_frames_max(),
        }
    }

    fn output_delay(&self) -> usize {
        match self {
            Self::Sinc(resampler) => resampler.output_delay(),
            Self::Fft(resampler) => resampler.output_delay(),
        }
    }

    fn process_into_buffer(
        &mut self,
        input: &InterleavedSlice<&[f64]>,
        output: &mut InterleavedSlice<&mut [f64]>,
    ) -> Result<(usize, usize), &'static str> {
        match self {
            Self::Sinc(resampler) => resampler.process_into_buffer(input, output, None),
            Self::Fft(resampler) => resampler.process_into_buffer(input, output, None),
        }
        .map_err(|_| "resampler backend process failed")
    }

    fn reset(&mut self) {
        match self {
            Self::Sinc(resampler) => resampler.reset(),
            Self::Fft(resampler) => resampler.reset(),
        }
    }
}

pub(super) struct MonoBackend {
    engine: RubatoEngine,
    /// Staged caller input; rubato consumes exact CHUNK_IN prefixes of this.
    in_fifo: Vec<f64>,
    /// rubato per-call output stage (single channel, len == output_frames_max).
    out_stage: Vec<f64>,
    /// Produced frames not yet handed to the caller.
    out_fifo: Vec<f64>,
    out_fifo_capacity: usize,
    /// Zero frames used to pad the final partial chunk and to flush the tail.
    zero_chunk: Vec<f64>,
    /// Frames consumed from the caller (pad zeros are not counted).
    total_input: u64,
    /// Frames handed to the caller.
    emitted: u64,
    /// Set at drain start: the duration-aligned total output length.
    expected_total: u64,
    /// Backend output frames still to discard from the initial leading delay.
    delay_remaining: usize,
    initial_delay: usize,
    draining: bool,
    from_rate: u32,
    to_rate: u32,
}

impl MonoBackend {
    pub(super) fn new(
        from_rate: u32,
        to_rate: u32,
        _phase: PhaseResponse,
        quality: ResampleQuality,
    ) -> Result<Self, String> {
        let engine = RubatoEngine::new(from_rate, to_rate, quality)?;
        let out_max = engine.output_frames_max();
        let initial_delay = engine.output_delay();
        let out_fifo_capacity = out_max * 2;
        Ok(Self {
            engine,
            in_fifo: Vec::with_capacity(CHUNK_IN * 2),
            out_stage: vec![0.0; out_max],
            out_fifo: Vec::with_capacity(out_fifo_capacity),
            out_fifo_capacity,
            zero_chunk: vec![0.0; CHUNK_IN],
            total_input: 0,
            emitted: 0,
            expected_total: 0,
            delay_remaining: initial_delay,
            initial_delay,
            draining: false,
            from_rate,
            to_rate,
        })
    }

    /// Run rubato over the first CHUNK_IN frames of `in_fifo` and append the
    /// produced frames to `out_fifo`.
    fn run_chunk(&mut self) -> Result<(), &'static str> {
        debug_assert!(self.in_fifo.len() >= CHUNK_IN);
        debug_assert!(self.out_fifo.len() + self.out_stage.len() <= self.out_fifo_capacity);
        let (input_used, output_written) = {
            let input = InterleavedSlice::new(&self.in_fifo[..CHUNK_IN], 1, CHUNK_IN)
                .map_err(|_| "resampler backend input view failed")?;
            let output_frames = self.out_stage.len();
            let mut output = InterleavedSlice::new_mut(&mut self.out_stage, 1, output_frames)
                .map_err(|_| "resampler backend output view failed")?;
            self.engine.process_into_buffer(&input, &mut output)?
        };
        if input_used != CHUNK_IN || output_written > self.out_stage.len() {
            return Err("resampler backend reported out-of-bounds progress");
        }
        let remaining = self.in_fifo.len() - CHUNK_IN;
        self.in_fifo.copy_within(CHUNK_IN.., 0);
        self.in_fifo.truncate(remaining);
        let skip = self.delay_remaining.min(output_written);
        self.delay_remaining -= skip;
        self.out_fifo
            .extend_from_slice(&self.out_stage[skip..output_written]);
        Ok(())
    }

    /// Move up to `max` pending frames into `output`, returning the count.
    fn emit_up_to(&mut self, output: &mut [f64], max: usize) -> usize {
        let count = self.out_fifo.len().min(output.len()).min(max);
        if count > 0 {
            output[..count].copy_from_slice(&self.out_fifo[..count]);
            let remaining = self.out_fifo.len() - count;
            self.out_fifo.copy_within(count.., 0);
            self.out_fifo.truncate(remaining);
            self.emitted += count as u64;
        }
        count
    }

    pub(super) fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<BackendProgress, &'static str> {
        if self.draining {
            return Err("resampler backend already draining");
        }
        let mut consumed = 0usize;
        let mut produced = 0usize;
        loop {
            let before = (consumed, produced);
            produced += self.emit_up_to(&mut output[produced..], usize::MAX);
            let free = self.in_fifo.capacity() - self.in_fifo.len();
            let take = free.min(input.len() - consumed);
            if take > 0 {
                self.in_fifo
                    .extend_from_slice(&input[consumed..consumed + take]);
                consumed += take;
                self.total_input += take as u64;
            }
            while self.in_fifo.len() >= CHUNK_IN
                && self.out_fifo.len() + self.out_stage.len() <= self.out_fifo_capacity
            {
                self.run_chunk()?;
            }
            produced += self.emit_up_to(&mut output[produced..], usize::MAX);
            if (consumed, produced) == before {
                break;
            }
        }
        Ok(BackendProgress {
            input_frames: consumed,
            output_frames: produced,
        })
    }

    pub(super) fn drain(&mut self, output: &mut [f64]) -> Result<usize, &'static str> {
        if !self.draining {
            self.draining = true;
            // Per-chunk output counts can jitter by a frame around the exact
            // ratio; never let already-emitted frames underflow `remaining`.
            self.expected_total =
                expected_output_frames(self.total_input, self.from_rate, self.to_rate)
                    .max(self.emitted);
        }
        let mut produced = 0usize;
        let mut stall_rounds = 0usize;
        loop {
            let remaining = (self.expected_total - self.emitted) as usize;
            produced += self.emit_up_to(&mut output[produced..], remaining);
            if self.emitted == self.expected_total {
                self.out_fifo.clear();
                self.in_fifo.clear();
                return Ok(produced);
            }
            if produced == output.len() {
                return Ok(produced);
            }
            // Reaching this point implies out_fifo is empty (emit was bounded
            // only by its length), so one full chunk of output always fits.
            // Flush staged real input first, then pad with zeros for the tail.
            if self.in_fifo.len() < CHUNK_IN {
                let pad = CHUNK_IN - self.in_fifo.len();
                self.in_fifo.extend_from_slice(&self.zero_chunk[..pad]);
            }
            let before_fifo = self.out_fifo.len();
            self.run_chunk()?;
            if self.out_fifo.len() == before_fifo {
                stall_rounds += 1;
                if stall_rounds > MAX_DRAIN_STALL_ROUNDS {
                    return Err("resampler backend drain stalled");
                }
            } else {
                stall_rounds = 0;
            }
        }
    }

    pub(super) fn clear(&mut self) -> Result<(), &'static str> {
        self.engine.reset();
        self.in_fifo.clear();
        self.out_fifo.clear();
        self.total_input = 0;
        self.emitted = 0;
        self.expected_total = 0;
        self.delay_remaining = self.initial_delay;
        self.draining = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_routing_accepts_common_audio_ratios_and_rejects_pathological_ones() {
        for quality in [
            ResampleQuality::Low,
            ResampleQuality::Standard,
            ResampleQuality::High,
        ] {
            for (from_rate, to_rate) in [
                (44_100, 48_000),
                (48_000, 96_000),
                (96_000, 44_100),
                (44_100, 192_000),
            ] {
                assert!(should_use_fft(from_rate, to_rate, quality));
            }
        }
        assert!(!should_use_fft(44_100, 44_101, ResampleQuality::High));
        assert!(!should_use_fft(0, 48_000, ResampleQuality::High));
    }

    #[test]
    fn ultra_high_preserves_the_sinc_quality_path_for_common_ratios() {
        assert!(matches!(
            RubatoEngine::new(44_100, 48_000, ResampleQuality::High).unwrap(),
            RubatoEngine::Fft(_)
        ));
        assert!(matches!(
            RubatoEngine::new(44_100, 48_000, ResampleQuality::UltraHigh).unwrap(),
            RubatoEngine::Sinc(_)
        ));
        assert!(matches!(
            RubatoEngine::new(44_100, 44_101, ResampleQuality::High).unwrap(),
            RubatoEngine::Sinc(_)
        ));
    }

    #[test]
    fn clear_restores_the_selected_engines_leading_delay() {
        for (from_rate, to_rate, quality) in [
            (44_100, 48_000, ResampleQuality::High),
            (44_100, 48_000, ResampleQuality::UltraHigh),
            (44_100, 44_101, ResampleQuality::High),
        ] {
            let mut backend =
                MonoBackend::new(from_rate, to_rate, PhaseResponse::Linear, quality).unwrap();
            assert!(backend.initial_delay > 0);
            backend.delay_remaining = 0;
            backend.clear().unwrap();
            assert_eq!(backend.delay_remaining, backend.initial_delay);
        }
    }
}
