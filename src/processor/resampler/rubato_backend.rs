//! Pure-Rust mono-channel resampler backend built on `rubato::SincFixedIn`.
//!
//! This backend adapts rubato's fixed-input-chunk interface to the same
//! streaming semantics the SoXR backend provides natively:
//!
//! - **Arbitrary input granularity.** Caller input of any size is staged in a
//!   pre-allocated input FIFO; rubato only ever sees exact `CHUNK_IN`-frame
//!   chunks, so the produced sample sequence is independent of how the caller
//!   splits its input (chunked and single-feed runs are bitwise identical).
//! - **No leading delay frames.** `SincFixedIn` initializes its interpolation
//!   index at `-sinc_len / 2`, pre-compensating the kernel group delay, so its
//!   output is already positionally aligned with the input (within one frame
//!   of rounding). No frames are skipped here; `output_delay()` reports the
//!   theoretical group delay, and honoring it would double-compensate.
//! - **Duration-aligned drain.** At end of stream the total output is padded
//!   (with zero-fed chunks) or truncated to `round(total_input * to / from)`
//!   frames, after which `drain` reports the terminal zero.
//! - **Allocation-free processing.** All buffers are allocated in `new`;
//!   `process`/`drain`/`clear` stay within pre-reserved capacity and rubato's
//!   `process_into_buffer` is itself allocation-free.
//!
//! Differences from the SoXR backend that callers should know about:
//! rubato's windowed-sinc kernel is **linear phase only**, so the requested
//! [`PhaseResponse`] is accepted but not applied, and the quality mapping
//! selects sinc length / oversampling rather than a SoX recipe.

use crate::config::{PhaseResponse, ResampleQuality};
use rubato::{
    calculate_cutoff, Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use super::BackendProgress;

pub(super) const BACKEND_NAME: &str = "rubato";

/// Fixed rubato input chunk size in frames. The FIFO adaptation makes this an
/// internal detail; changing it changes latency smoothing granularity only.
const CHUNK_IN: usize = 1024;

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
    SincInterpolationParameters {
        sinc_len,
        f_cutoff: calculate_cutoff::<f32>(sinc_len, WindowFunction::BlackmanHarris2),
        oversampling_factor,
        interpolation,
        window: WindowFunction::BlackmanHarris2,
    }
}

/// Total output frames a stream of `total_input` frames must produce to be
/// duration-aligned, rounding half up.
fn expected_output_frames(total_input: u64, from_rate: u32, to_rate: u32) -> u64 {
    ((total_input as u128 * to_rate as u128 * 2 + from_rate as u128) / (from_rate as u128 * 2))
        as u64
}

pub(super) struct MonoBackend {
    resampler: SincFixedIn<f64>,
    /// Staged caller input; rubato consumes exact CHUNK_IN prefixes of this.
    in_fifo: Vec<f64>,
    /// rubato per-call output stage (single channel, len == output_frames_max).
    out_stage: Vec<Vec<f64>>,
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
        let ratio = to_rate as f64 / from_rate as f64;
        let resampler = SincFixedIn::<f64>::new(ratio, 1.0, sinc_parameters(quality), CHUNK_IN, 1)
            .map_err(|error| format!("{error}"))?;
        let out_max = resampler.output_frames_max();
        let out_fifo_capacity = out_max * 2;
        Ok(Self {
            resampler,
            in_fifo: Vec::with_capacity(CHUNK_IN * 2),
            out_stage: vec![vec![0.0; out_max]; 1],
            out_fifo: Vec::with_capacity(out_fifo_capacity),
            out_fifo_capacity,
            zero_chunk: vec![0.0; CHUNK_IN],
            total_input: 0,
            emitted: 0,
            expected_total: 0,
            draining: false,
            from_rate,
            to_rate,
        })
    }

    /// Run rubato over the first CHUNK_IN frames of `in_fifo` and append the
    /// produced frames to `out_fifo`.
    fn run_chunk(&mut self) -> Result<(), &'static str> {
        debug_assert!(self.in_fifo.len() >= CHUNK_IN);
        debug_assert!(self.out_fifo.len() + self.out_stage[0].len() <= self.out_fifo_capacity);
        let (input_used, output_written) = self
            .resampler
            .process_into_buffer(&[&self.in_fifo[..CHUNK_IN]], &mut self.out_stage, None)
            .map_err(|_| "resampler backend process failed")?;
        if input_used != CHUNK_IN || output_written > self.out_stage[0].len() {
            return Err("resampler backend reported out-of-bounds progress");
        }
        let remaining = self.in_fifo.len() - CHUNK_IN;
        self.in_fifo.copy_within(CHUNK_IN.., 0);
        self.in_fifo.truncate(remaining);
        self.out_fifo
            .extend_from_slice(&self.out_stage[0][..output_written]);
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
                && self.out_fifo.len() + self.out_stage[0].len() <= self.out_fifo_capacity
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
        self.resampler.reset();
        self.in_fifo.clear();
        self.out_fifo.clear();
        self.total_input = 0;
        self.emitted = 0;
        self.expected_total = 0;
        self.draining = false;
        Ok(())
    }
}
