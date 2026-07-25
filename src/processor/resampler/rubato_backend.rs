//! Pure-Rust interleaved resampler backend built on rubato 4.
//!
//! This backend adapts rubato's fixed-input-chunk interface to the same
//! streaming semantics the SoXR backend provides natively:
//!
//! - **Arbitrary input granularity.** Caller input of any size is staged in a
//!   pre-allocated input FIFO; rubato only ever sees exact `CHUNK_IN`-frame
//!   chunks, so the produced sample sequence is independent of how the caller
//!   splits its input (chunked and single-feed runs are bitwise identical).
//! - **Linear delay compensation.** The rubato FFT and sinc engines used for
//!   [`PhaseResponse::Linear`] carry a real leading delay. The adapter discards
//!   `output_delay()` produced frames at stream start (and again after reset),
//!   so callers receive an aligned duration sequence.
//! - **Nonlinear causal tails.** [`PhaseResponse::Minimum`] and
//!   [`PhaseResponse::Maximum`] use a setup-designed rational polyphase FIR
//!   instead of pretending that a delay shift changes phase. They retain the
//!   actual causal latency and finite tail through the streaming lifecycle.
//! - **Bounded drain.** At end of stream the adapter feeds preallocated zeros
//!   only until the duration sequence (and, for nonlinear phase, its declared
//!   finite tail) is complete, after which `drain` reports the terminal zero.
//! - **Allocation-free processing.** All buffers are allocated in `new`;
//!   `process`/`drain`/`clear` stay within pre-reserved capacity and rubato's
//!   `process_into_buffer` is itself allocation-free.
//!
//! For [`PhaseResponse::Linear`], exact 2:1 High-quality upsampling uses a
//! dedicated symmetric half-band FIR. Other common reduced sample-rate ratios
//! use the much faster rubato FFT engine through High quality; UltraHigh and
//! ratios that would create pathological FFT blocks use sinc, where the quality
//! mapping selects sinc length / oversampling rather than a SoX recipe. For
//! [`PhaseResponse::Minimum`] and [`PhaseResponse::Maximum`], the adapter uses
//! a precomputed real-cepstrum polyphase FIR bank. Nonlinear phase intentionally
//! rejects reduced rate components above 1024 rather than silently falling back
//! to the linear Rubato engines.

use crate::config::{PhaseResponse, ResampleQuality};
use rubato::{
    audioadapter_buffers::direct::InterleavedSlice, Async, Fft, FixedAsync, FixedSync, Resampler,
    SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

use super::{
    halfband_backend::Halfband2xResampler, polyphase_backend::PolyphaseResampler, BackendProgress,
};

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

/// FFT sub-chunks selected by the retained same-machine sweep.
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

fn should_use_halfband_2x(
    from_rate: u32,
    to_rate: u32,
    phase: PhaseResponse,
    quality: ResampleQuality,
) -> bool {
    matches!(phase, PhaseResponse::Linear)
        && matches!(quality, ResampleQuality::High)
        && from_rate.checked_mul(2) == Some(to_rate)
}

/// Total output frames a stream of `total_input` frames must produce to be
/// duration-aligned, rounding half up.
pub(super) fn expected_output_frames(total_input: u64, from_rate: u32, to_rate: u32) -> u64 {
    ((total_input as u128 * to_rate as u128 * 2 + from_rate as u128) / (from_rate as u128 * 2))
        as u64
}

/// Fixed-capacity sample FIFO with bounded two-copy push/pop operations.
///
/// Unlike the public pipeline ring, this adapter-local queue never overwrites
/// unread audio and never logs. Capacity errors remain static hot-path errors.
struct SampleRing {
    data: Box<[f64]>,
    head: usize,
    len: usize,
}

impl SampleRing {
    fn new(capacity: usize) -> Self {
        Self {
            data: vec![0.0; capacity].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn free(&self) -> usize {
        self.data.len() - self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    fn push(&mut self, source: &[f64]) -> Result<(), &'static str> {
        if source.len() > self.free() {
            return Err("resampler backend FIFO capacity exceeded");
        }
        if source.is_empty() {
            return Ok(());
        }

        let capacity = self.data.len();
        let tail = (self.head + self.len) % capacity;
        let first = source.len().min(capacity - tail);
        self.data[tail..tail + first].copy_from_slice(&source[..first]);
        let remaining = source.len() - first;
        if remaining > 0 {
            self.data[..remaining].copy_from_slice(&source[first..]);
        }
        self.len += source.len();
        Ok(())
    }

    fn front_contiguous(&self, samples: usize) -> Option<&[f64]> {
        let end = self.head.checked_add(samples)?;
        if samples > self.len || end > self.data.len() {
            return None;
        }
        Some(&self.data[self.head..end])
    }

    fn consume(&mut self, samples: usize) -> Result<(), &'static str> {
        if samples > self.len {
            return Err("resampler backend FIFO underflow");
        }
        self.len -= samples;
        if self.len == 0 {
            self.head = 0;
        } else {
            self.head = (self.head + samples) % self.data.len();
        }
        Ok(())
    }

    fn pop_into(&mut self, output: &mut [f64]) -> usize {
        let samples = output.len().min(self.len);
        if samples == 0 {
            return 0;
        }

        let first = samples.min(self.data.len() - self.head);
        output[..first].copy_from_slice(&self.data[self.head..self.head + first]);
        let remaining = samples - first;
        if remaining > 0 {
            output[first..samples].copy_from_slice(&self.data[..remaining]);
        }

        self.len -= samples;
        if self.len == 0 {
            self.head = 0;
        } else {
            self.head = (self.head + samples) % self.data.len();
        }
        samples
    }
}

enum RubatoEngine {
    Halfband(Halfband2xResampler),
    Sinc(Async<f64>),
    Fft(Box<Fft<f64>>),
    Polyphase(PolyphaseResampler),
}

impl RubatoEngine {
    fn new(
        from_rate: u32,
        to_rate: u32,
        phase: PhaseResponse,
        quality: ResampleQuality,
        channels: usize,
    ) -> Result<Self, String> {
        if !matches!(phase, PhaseResponse::Linear) {
            return PolyphaseResampler::new(from_rate, to_rate, phase, quality, channels, CHUNK_IN)
                .map(Self::Polyphase);
        }
        if should_use_halfband_2x(from_rate, to_rate, phase, quality) {
            return Halfband2xResampler::new(channels, CHUNK_IN).map(Self::Halfband);
        }
        if should_use_fft(from_rate, to_rate, quality) {
            Fft::<f64>::new_custom(
                from_rate as usize,
                to_rate as usize,
                CHUNK_IN,
                FFT_SUB_CHUNKS,
                channels,
                WindowFunction::BlackmanHarris2,
                FixedSync::Input,
            )
            .map(Box::new)
            .map(Self::Fft)
            .map_err(|error| format!("{error}"))
        } else {
            let ratio = to_rate as f64 / from_rate as f64;
            let parameters = sinc_parameters(quality);
            Async::<f64>::new_sinc(
                ratio,
                1.0,
                &parameters,
                CHUNK_IN,
                channels,
                FixedAsync::Input,
            )
            .map(Self::Sinc)
            .map_err(|error| format!("{error}"))
        }
    }

    fn output_frames_max(&self) -> usize {
        match self {
            Self::Halfband(resampler) => resampler.output_frames_max(),
            Self::Sinc(resampler) => resampler.output_frames_max(),
            Self::Fft(resampler) => resampler.output_frames_max(),
            Self::Polyphase(resampler) => resampler.output_frames_max(),
        }
    }

    fn output_delay(&self) -> usize {
        match self {
            Self::Halfband(resampler) => resampler.output_delay(),
            Self::Sinc(resampler) => resampler.output_delay(),
            Self::Fft(resampler) => resampler.output_delay(),
            Self::Polyphase(resampler) => resampler.output_delay(),
        }
    }

    fn latency_frames(&self) -> usize {
        match self {
            Self::Halfband(_) | Self::Sinc(_) | Self::Fft(_) => 0,
            Self::Polyphase(resampler) => resampler.latency_frames(),
        }
    }

    fn finish_extension_frames(&self) -> usize {
        match self {
            Self::Halfband(_) | Self::Sinc(_) | Self::Fft(_) => 0,
            Self::Polyphase(resampler) => resampler.finish_extension_frames(),
        }
    }

    fn process_chunk(
        &mut self,
        input: &[f64],
        output: &mut [f64],
        channels: usize,
    ) -> Result<(usize, usize), &'static str> {
        match self {
            Self::Halfband(resampler) => return resampler.process_chunk(input, output),
            Self::Polyphase(resampler) => return resampler.process_chunk(input, output),
            Self::Sinc(resampler) => {
                let input_frames = input.len() / channels;
                let output_frames = output.len() / channels;
                let input = InterleavedSlice::new(input, channels, input_frames)
                    .map_err(|_| "resampler backend input view failed")?;
                let mut output = InterleavedSlice::new_mut(output, channels, output_frames)
                    .map_err(|_| "resampler backend output view failed")?;
                resampler.process_into_buffer(&input, &mut output, None)
            }
            Self::Fft(resampler) => {
                let input_frames = input.len() / channels;
                let output_frames = output.len() / channels;
                let input = InterleavedSlice::new(input, channels, input_frames)
                    .map_err(|_| "resampler backend input view failed")?;
                let mut output = InterleavedSlice::new_mut(output, channels, output_frames)
                    .map_err(|_| "resampler backend output view failed")?;
                resampler.process_into_buffer(&input, &mut output, None)
            }
        }
        .map_err(|_| "resampler backend process failed")
    }

    fn reset(&mut self) {
        match self {
            Self::Halfband(resampler) => resampler.reset(),
            Self::Sinc(resampler) => resampler.reset(),
            Self::Fft(resampler) => resampler.reset(),
            Self::Polyphase(resampler) => resampler.reset(),
        }
    }
}

pub(super) struct MonoBackend {
    engine: RubatoEngine,
    channels: usize,
    /// Staged caller input; rubato consumes exact CHUNK_IN prefixes of this.
    in_fifo: SampleRing,
    /// Per-engine interleaved output stage.
    out_stage: Vec<f64>,
    /// Produced frames not yet handed to the caller.
    out_fifo: SampleRing,
    /// Zero frames used to pad the final partial chunk and to flush the tail.
    zero_chunk: Vec<f64>,
    /// Frames consumed from the caller (pad zeros are not counted).
    total_input: u64,
    /// Real caller frames actually consumed by completed backend chunks. This
    /// excludes caller input still queued in `in_fifo` and drain pad zeros; it
    /// bounds the caller-visible prefix on the non-integer direct-output path.
    processed_real_input: u64,
    /// Setup-selected non-integer-ratio direct-output mode: engine output is
    /// written straight into caller memory up to the cumulative rational
    /// real-input budget, and the bounded overflow spills into `out_fifo`.
    prefix_budget_direct: bool,
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

fn process_fifo_chunk_into(
    engine: &mut RubatoEngine,
    in_fifo: &mut SampleRing,
    output: &mut [f64],
    channels: usize,
    delay_remaining: &mut usize,
) -> Result<usize, &'static str> {
    let chunk_samples = CHUNK_IN * channels;
    let input = in_fifo
        .front_contiguous(chunk_samples)
        .ok_or("resampler backend input FIFO lost chunk contiguity")?;
    let (input_used, output_written) = engine.process_chunk(input, output, channels)?;
    let output_capacity_frames = output.len() / channels;
    if input_used != CHUNK_IN || output_written > output_capacity_frames {
        return Err("resampler backend reported out-of-bounds progress");
    }
    in_fifo.consume(chunk_samples)?;

    let skip = (*delay_remaining).min(output_written);
    *delay_remaining -= skip;
    let emitted = output_written - skip;
    if skip > 0 && emitted > 0 {
        output.copy_within(skip * channels..output_written * channels, 0);
    }
    Ok(emitted)
}

impl MonoBackend {
    pub(super) fn new(
        from_rate: u32,
        to_rate: u32,
        phase: PhaseResponse,
        quality: ResampleQuality,
    ) -> Result<Self, String> {
        Self::new_interleaved(from_rate, to_rate, phase, quality, 1)
    }

    pub(super) fn new_interleaved(
        from_rate: u32,
        to_rate: u32,
        phase: PhaseResponse,
        quality: ResampleQuality,
        channels: usize,
    ) -> Result<Self, String> {
        if channels == 0 {
            return Err("channel count must be >= 1".to_string());
        }
        let engine = RubatoEngine::new(from_rate, to_rate, phase, quality, channels)?;
        let out_max = engine.output_frames_max();
        let initial_delay = engine.output_delay();
        let out_fifo_capacity = out_max * 2 * channels;
        let duration_stable =
            (CHUNK_IN as u128 * to_rate as u128).is_multiple_of(from_rate.max(1) as u128);
        // The FFT engine's cumulative post-delay output tracks the exact
        // rational duration within one frame per chunk, so its non-integer
        // ratios can use budget-bounded direct output. The sinc and polyphase
        // engines keep the staged route.
        let prefix_budget_direct = !duration_stable && matches!(engine, RubatoEngine::Fft(_));
        Ok(Self {
            engine,
            channels,
            in_fifo: SampleRing::new(CHUNK_IN * 2 * channels),
            out_stage: vec![0.0; out_max * channels],
            out_fifo: SampleRing::new(out_fifo_capacity),
            zero_chunk: vec![0.0; CHUNK_IN * channels],
            total_input: 0,
            processed_real_input: 0,
            prefix_budget_direct,
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
        let emitted = process_fifo_chunk_into(
            &mut self.engine,
            &mut self.in_fifo,
            &mut self.out_stage,
            self.channels,
            &mut self.delay_remaining,
        )?;
        self.out_fifo
            .push(&self.out_stage[..emitted * self.channels])
    }

    fn direct_output_is_duration_stable(&self) -> bool {
        (CHUNK_IN as u128 * self.to_rate as u128).is_multiple_of(self.from_rate as u128)
    }

    /// Caller-visible frames still authorized by real backend-processed input
    /// on the prefix-budget direct route. Frames beyond this stay staged in
    /// `out_fifo` until later real input (or finish) authorizes them.
    fn process_emit_budget(&self) -> usize {
        if !self.prefix_budget_direct {
            return usize::MAX;
        }
        let allowed_total =
            expected_output_frames(self.processed_real_input, self.from_rate, self.to_rate);
        allowed_total
            .saturating_sub(self.emitted)
            .min(usize::MAX as u64) as usize
    }

    /// Move up to `max` pending frames into `output`, returning the count.
    fn emit_up_to(&mut self, output: &mut [f64], max: usize) -> usize {
        let count = (self.out_fifo.len() / self.channels)
            .min(output.len() / self.channels)
            .min(max);
        if count > 0 {
            let samples = count * self.channels;
            let copied = self.out_fifo.pop_into(&mut output[..samples]);
            let copied_frames = copied / self.channels;
            self.emitted += copied_frames as u64;
            return copied_frames;
        }
        0
    }

    pub(super) fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<BackendProgress, &'static str> {
        if self.draining {
            return Err("resampler backend already draining");
        }
        if !input.len().is_multiple_of(self.channels) || !output.len().is_multiple_of(self.channels)
        {
            return Err("resampler backend received an incomplete frame");
        }
        let mut consumed = 0usize;
        let mut produced = 0usize;
        let input_frames = input.len() / self.channels;
        let output_frames = output.len() / self.channels;
        loop {
            let before = (consumed, produced);
            produced += self.emit_up_to(
                &mut output[produced * self.channels..],
                self.process_emit_budget(),
            );
            let free = self.in_fifo.free() / self.channels;
            let take = free.min(input_frames - consumed);
            if take > 0 {
                let start = consumed * self.channels;
                let end = (consumed + take) * self.channels;
                self.in_fifo.push(&input[start..end])?;
                consumed += take;
                self.total_input += take as u64;
            }
            while self.in_fifo.len() / self.channels >= CHUNK_IN
                && self.out_fifo.free() >= self.out_stage.len()
            {
                let remaining_output = output_frames - produced;
                let duration_stable = self.direct_output_is_duration_stable();
                if (duration_stable || self.prefix_budget_direct)
                    && self.out_fifo.is_empty()
                    && remaining_output >= self.engine.output_frames_max()
                {
                    let direct_samples = self.engine.output_frames_max() * self.channels;
                    let chunk_frames = process_fifo_chunk_into(
                        &mut self.engine,
                        &mut self.in_fifo,
                        &mut output
                            [produced * self.channels..produced * self.channels + direct_samples],
                        self.channels,
                        &mut self.delay_remaining,
                    )?;
                    self.processed_real_input += CHUNK_IN as u64;
                    let direct = if duration_stable {
                        chunk_frames
                    } else {
                        // Expose only the cumulative rational real-input
                        // budget; spill the bounded overflow tail into the
                        // preallocated output ring for a later emit.
                        let budgeted = chunk_frames.min(self.process_emit_budget());
                        let spill_start = (produced + budgeted) * self.channels;
                        let spill_end = (produced + chunk_frames) * self.channels;
                        self.out_fifo.push(&output[spill_start..spill_end])?;
                        budgeted
                    };
                    self.emitted += direct as u64;
                    produced += direct;
                    continue;
                }
                self.run_chunk()?;
                self.processed_real_input += CHUNK_IN as u64;
            }
            produced += self.emit_up_to(
                &mut output[produced * self.channels..],
                self.process_emit_budget(),
            );
            if (consumed, produced) == before {
                break;
            }
            if produced == output_frames && consumed == input_frames {
                break;
            }
        }
        Ok(BackendProgress {
            input_frames: consumed,
            output_frames: produced,
        })
    }

    pub(super) fn drain(&mut self, output: &mut [f64]) -> Result<usize, &'static str> {
        if !output.len().is_multiple_of(self.channels) {
            return Err("resampler backend received an incomplete output frame");
        }
        if !self.draining {
            self.draining = true;
            // Per-chunk output counts can jitter by a frame around the exact
            // ratio; never let already-emitted frames underflow `remaining`.
            self.expected_total =
                expected_output_frames(self.total_input, self.from_rate, self.to_rate)
                    .saturating_add(self.engine.finish_extension_frames() as u64)
                    .max(self.emitted);
        }
        let mut produced = 0usize;
        let mut stall_rounds = 0usize;
        let output_frames = output.len() / self.channels;
        loop {
            let remaining = (self.expected_total - self.emitted) as usize;
            produced += self.emit_up_to(&mut output[produced * self.channels..], remaining);
            if self.emitted == self.expected_total {
                self.out_fifo.clear();
                self.in_fifo.clear();
                return Ok(produced);
            }
            if produced == output_frames {
                return Ok(produced);
            }
            // Reaching this point implies out_fifo is empty (emit was bounded
            // only by its length), so one full chunk of output always fits.
            // Flush staged real input first, then pad with zeros for the tail.
            let staged_frames = self.in_fifo.len() / self.channels;
            if staged_frames < CHUNK_IN {
                let pad_samples = (CHUNK_IN - staged_frames) * self.channels;
                self.in_fifo.push(&self.zero_chunk[..pad_samples])?;
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
        self.processed_real_input = 0;
        self.emitted = 0;
        self.expected_total = 0;
        self.delay_remaining = self.initial_delay;
        self.draining = false;
        Ok(())
    }

    pub(super) fn latency_frames(&self) -> usize {
        self.engine.latency_frames()
    }

    pub(super) fn finish_extension_frames(&self) -> usize {
        self.engine.finish_extension_frames()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_ring_wraps_without_reordering_or_allocating() {
        let mut ring = SampleRing::new(8);
        let first = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let second = [6.0, 7.0, 8.0, 9.0, 10.0];
        let mut prefix = [0.0; 5];
        let mut suffix = [0.0; 6];

        assert_no_alloc::assert_no_alloc(|| {
            ring.push(&first).unwrap();
            assert_eq!(ring.pop_into(&mut prefix), prefix.len());
            ring.push(&second).unwrap();
            assert_eq!(ring.pop_into(&mut suffix), suffix.len());
        });

        assert_eq!(prefix, [0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(suffix, [5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        assert!(ring.is_empty());
    }

    #[test]
    fn double_chunk_input_ring_keeps_each_front_chunk_contiguous() {
        const CHUNK: usize = 4;
        let mut ring = SampleRing::new(CHUNK * 2);
        ring.push(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
        assert_eq!(ring.front_contiguous(CHUNK).unwrap(), &[0.0, 1.0, 2.0, 3.0]);
        ring.consume(CHUNK).unwrap();
        ring.push(&[6.0, 7.0]).unwrap();
        assert_eq!(ring.front_contiguous(CHUNK).unwrap(), &[4.0, 5.0, 6.0, 7.0]);
        ring.consume(CHUNK).unwrap();
        assert!(ring.is_empty());
    }

    fn render_backend(backend: &mut MonoBackend, input: &[f64], channels: usize) -> Vec<f64> {
        render_backend_with_output_frames(backend, input, channels, CHUNK_IN * 4)
    }

    fn render_backend_with_output_frames(
        backend: &mut MonoBackend,
        input: &[f64],
        channels: usize,
        output_frames: usize,
    ) -> Vec<f64> {
        let input_frames = input.len() / channels;
        let mut output = Vec::new();
        let mut output_scratch = vec![0.0; output_frames * channels];
        let mut input_cursor = 0;
        let input_chunk_pattern = [127, 509, 31, 1_024];
        let mut pattern_cursor = 0;

        while input_cursor < input_frames {
            let chunk_frames = input_chunk_pattern[pattern_cursor % input_chunk_pattern.len()]
                .min(input_frames - input_cursor);
            pattern_cursor += 1;
            let chunk_end = input_cursor + chunk_frames;
            while input_cursor < chunk_end {
                let input_start = input_cursor * channels;
                let input_end = chunk_end * channels;
                let progress = backend
                    .process(
                        &input[input_start..input_end],
                        output_scratch.as_mut_slice(),
                    )
                    .unwrap();
                assert!(progress.input_frames > 0 || progress.output_frames > 0);
                output.extend_from_slice(&output_scratch[..progress.output_frames * channels]);
                input_cursor += progress.input_frames;
            }
        }

        loop {
            let produced_frames = backend.drain(output_scratch.as_mut_slice()).unwrap();
            output.extend_from_slice(&output_scratch[..produced_frames * channels]);
            if produced_frames == 0 {
                break;
            }
        }
        output
    }

    fn assert_interleaved_matches_independent_mono(
        from_rate: u32,
        to_rate: u32,
        quality: ResampleQuality,
    ) {
        const CHANNELS: usize = 2;
        const INPUT_FRAMES: usize = 4_097;
        let input: Vec<f64> = (0..INPUT_FRAMES)
            .flat_map(|frame| {
                let time = frame as f64;
                [
                    (time * 0.017).sin() * 0.5 + (time * 0.003).cos() * 0.1,
                    (time * 0.013 + 0.7).sin() * 0.4 - (time * 0.005).cos() * 0.2,
                ]
            })
            .collect();

        let mut interleaved = MonoBackend::new_interleaved(
            from_rate,
            to_rate,
            PhaseResponse::Linear,
            quality,
            CHANNELS,
        )
        .unwrap();
        let actual = render_backend(&mut interleaved, &input, CHANNELS);

        let mut mono_outputs = Vec::with_capacity(CHANNELS);
        for channel in 0..CHANNELS {
            let mono_input: Vec<f64> = input
                .chunks_exact(CHANNELS)
                .map(|frame| frame[channel])
                .collect();
            let mut mono =
                MonoBackend::new(from_rate, to_rate, PhaseResponse::Linear, quality).unwrap();
            mono_outputs.push(render_backend(&mut mono, &mono_input, 1));
        }

        let output_frames = actual.len() / CHANNELS;
        assert!(mono_outputs
            .iter()
            .all(|channel| channel.len() == output_frames));
        let mut max_error = 0.0_f64;
        for frame in 0..output_frames {
            for channel in 0..CHANNELS {
                max_error = max_error
                    .max((actual[frame * CHANNELS + channel] - mono_outputs[channel][frame]).abs());
            }
        }
        assert!(
            max_error <= 1.0e-14,
            "native interleaved {quality:?} output diverged from independent mono by {max_error:e}"
        );
    }

    #[test]
    fn fft_routing_accepts_common_audio_ratios_and_rejects_pathological_ones() {
        for quality in [
            ResampleQuality::Low,
            ResampleQuality::Standard,
            ResampleQuality::High,
        ] {
            for (from_rate, to_rate) in [(44_100, 48_000), (96_000, 44_100), (44_100, 192_000)] {
                assert!(should_use_fft(from_rate, to_rate, quality));
            }
        }
        assert!(!should_use_fft(44_100, 44_101, ResampleQuality::High));
        assert!(!should_use_fft(0, 48_000, ResampleQuality::High));
    }

    #[test]
    fn exact_two_x_high_upsampling_routes_to_halfband_without_broadening() {
        assert!(should_use_halfband_2x(
            48_000,
            96_000,
            PhaseResponse::Linear,
            ResampleQuality::High
        ));
        assert!(should_use_halfband_2x(
            44_100,
            88_200,
            PhaseResponse::Linear,
            ResampleQuality::High
        ));
        for (from_rate, to_rate, phase, quality) in [
            (
                48_000,
                96_000,
                PhaseResponse::Linear,
                ResampleQuality::Standard,
            ),
            (
                48_000,
                96_000,
                PhaseResponse::Linear,
                ResampleQuality::UltraHigh,
            ),
            (96_000, 48_000, PhaseResponse::Linear, ResampleQuality::High),
            (
                48_000,
                96_000,
                PhaseResponse::Minimum,
                ResampleQuality::High,
            ),
            (44_100, 48_000, PhaseResponse::Linear, ResampleQuality::High),
        ] {
            assert!(!should_use_halfband_2x(from_rate, to_rate, phase, quality));
        }
        assert!(matches!(
            RubatoEngine::new(
                48_000,
                96_000,
                PhaseResponse::Linear,
                ResampleQuality::High,
                2,
            )
            .unwrap(),
            RubatoEngine::Halfband(_)
        ));
    }

    #[test]
    fn ultra_high_preserves_the_sinc_quality_path_for_common_ratios() {
        assert!(matches!(
            RubatoEngine::new(
                44_100,
                48_000,
                PhaseResponse::Linear,
                ResampleQuality::High,
                1,
            )
            .unwrap(),
            RubatoEngine::Fft(_)
        ));
        assert!(matches!(
            RubatoEngine::new(
                44_100,
                48_000,
                PhaseResponse::Linear,
                ResampleQuality::UltraHigh,
                1,
            )
            .unwrap(),
            RubatoEngine::Sinc(_)
        ));
        assert!(matches!(
            RubatoEngine::new(
                44_100,
                44_101,
                PhaseResponse::Linear,
                ResampleQuality::High,
                1,
            )
            .unwrap(),
            RubatoEngine::Sinc(_)
        ));
    }

    #[test]
    fn native_interleaved_matches_independent_mono_for_fft_and_sinc() {
        assert_interleaved_matches_independent_mono(44_100, 48_000, ResampleQuality::High);
        assert_interleaved_matches_independent_mono(44_100, 48_000, ResampleQuality::UltraHigh);
        assert_interleaved_matches_independent_mono(48_000, 96_000, ResampleQuality::High);
    }

    #[test]
    fn duration_stable_direct_output_is_bit_exact_with_staged_output() {
        const INPUT_FRAMES: usize = 8_193;
        let input: Vec<f64> = (0..INPUT_FRAMES)
            .map(|frame| {
                let time = frame as f64;
                (time * 0.071).sin() * 0.6 + (time * 0.013).cos() * 0.2
            })
            .collect();

        for (from_rate, to_rate, quality) in [
            (48_000, 96_000, ResampleQuality::High),
            (96_000, 48_000, ResampleQuality::UltraHigh),
        ] {
            let mut direct =
                MonoBackend::new(from_rate, to_rate, PhaseResponse::Linear, quality).unwrap();
            let mut staged =
                MonoBackend::new(from_rate, to_rate, PhaseResponse::Linear, quality).unwrap();

            let direct_output = render_backend(&mut direct, &input, 1);
            let staged_output = render_backend_with_output_frames(&mut staged, &input, 1, 257);
            assert_eq!(
                direct_output.len(),
                staged_output.len(),
                "{from_rate}->{to_rate} output length"
            );
            if let Some(index) = direct_output
                .iter()
                .zip(&staged_output)
                .position(|(direct, staged)| direct.to_bits() != staged.to_bits())
            {
                panic!(
                    "{from_rate}->{to_rate} first mismatch at {index}: direct={} staged={}",
                    direct_output[index], staged_output[index]
                );
            }
        }
    }

    fn noninteger_input() -> Vec<f64> {
        const INPUT_FRAMES: usize = 9_311;
        (0..INPUT_FRAMES)
            .map(|frame| {
                let time = frame as f64;
                (time * 0.041).sin() * 0.55 + (time * 0.007).cos() * 0.25
            })
            .collect()
    }

    #[test]
    fn noninteger_direct_output_is_bit_exact_with_staged_output() {
        let input = noninteger_input();
        let expected_len = expected_output_frames(input.len() as u64, 44_100, 48_000) as usize;

        let make = || {
            MonoBackend::new(44_100, 48_000, PhaseResponse::Linear, ResampleQuality::High).unwrap()
        };
        assert!(make().prefix_budget_direct);

        // Ordinary large caller output: eligible for the direct branch.
        let direct = render_backend(&mut make(), &input, 1);
        // Output-constrained caller: always forced through the staged route.
        let staged = render_backend_with_output_frames(&mut make(), &input, 1, 257);
        // Irregular caller output sizes: mixes direct and staged chunks.
        let irregular = render_backend_with_output_frames(&mut make(), &input, 1, 1_201);

        assert_eq!(direct.len(), expected_len);
        assert_eq!(staged.len(), expected_len);
        assert_eq!(irregular.len(), expected_len);
        for (name, other) in [("staged", &staged), ("irregular", &irregular)] {
            if let Some(index) = direct
                .iter()
                .zip(other.iter())
                .position(|(left, right)| left.to_bits() != right.to_bits())
            {
                panic!(
                    "44100->48000 direct vs {name} first mismatch at {index}: {} vs {}",
                    direct[index], other[index]
                );
            }
        }
    }

    #[test]
    fn noninteger_direct_prefix_never_exceeds_real_input_budget() {
        let input = noninteger_input();
        let mut backend =
            MonoBackend::new(44_100, 48_000, PhaseResponse::Linear, ResampleQuality::High).unwrap();
        let mut output = vec![0.0; CHUNK_IN * 4];
        let mut collected = 0usize;
        let mut cursor = 0usize;
        while cursor < input.len() {
            let end = (cursor + 733).min(input.len());
            let progress = backend.process(&input[cursor..end], &mut output).unwrap();
            cursor += progress.input_frames;
            collected += progress.output_frames;
            assert!(backend.processed_real_input <= backend.total_input);
            let allowed =
                expected_output_frames(backend.processed_real_input, 44_100, 48_000) as usize;
            assert!(
                collected <= allowed,
                "emitted {collected} frames beyond real-input budget {allowed}"
            );
        }
        loop {
            let produced = backend.drain(&mut output).unwrap();
            collected += produced;
            if produced == 0 {
                break;
            }
        }
        let expected = expected_output_frames(input.len() as u64, 44_100, 48_000) as usize;
        assert_eq!(collected, expected, "complete finish duration mismatch");
    }

    #[test]
    fn noninteger_clear_after_process_and_partial_drain_matches_fresh() {
        let input = noninteger_input();
        let mut fresh =
            MonoBackend::new(44_100, 48_000, PhaseResponse::Linear, ResampleQuality::High).unwrap();
        let expected = render_backend(&mut fresh, &input, 1);

        // Reset after process only.
        let mut reused =
            MonoBackend::new(44_100, 48_000, PhaseResponse::Linear, ResampleQuality::High).unwrap();
        let mut scratch = vec![0.0; CHUNK_IN * 4];
        let mut cursor = 0usize;
        while cursor < 3_000 {
            let progress = backend_step(&mut reused, &input[cursor..3_000], &mut scratch);
            cursor += progress;
        }
        reused.clear().unwrap();
        let after_process_reset = render_backend(&mut reused, &input, 1);
        assert_bits_equal(&expected, &after_process_reset, "reset after process");

        // Reset after a partial drain.
        let mut reused =
            MonoBackend::new(44_100, 48_000, PhaseResponse::Linear, ResampleQuality::High).unwrap();
        let mut cursor = 0usize;
        while cursor < 3_000 {
            let progress = backend_step(&mut reused, &input[cursor..3_000], &mut scratch);
            cursor += progress;
        }
        let mut small = [0.0; 97];
        let _ = reused.drain(&mut small).unwrap();
        reused.clear().unwrap();
        let after_partial_drain_reset = render_backend(&mut reused, &input, 1);
        assert_bits_equal(&expected, &after_partial_drain_reset, "reset after drain");
    }

    fn backend_step(backend: &mut MonoBackend, input: &[f64], scratch: &mut [f64]) -> usize {
        let progress = backend.process(input, scratch).unwrap();
        assert!(progress.input_frames > 0 || progress.output_frames > 0);
        progress.input_frames
    }

    fn assert_bits_equal(expected: &[f64], actual: &[f64], label: &str) {
        assert_eq!(expected.len(), actual.len(), "{label} length");
        if let Some(index) = expected
            .iter()
            .zip(actual.iter())
            .position(|(left, right)| left.to_bits() != right.to_bits())
        {
            panic!(
                "{label} first mismatch at {index}: {} vs {}",
                expected[index], actual[index]
            );
        }
    }

    #[test]
    fn noninteger_process_and_drain_do_not_allocate_after_setup() {
        let input = noninteger_input();
        let mut backend =
            MonoBackend::new(44_100, 48_000, PhaseResponse::Linear, ResampleQuality::High).unwrap();
        let mut output = vec![0.0; CHUNK_IN * 4];
        assert_no_alloc::assert_no_alloc(|| {
            let mut cursor = 0usize;
            while cursor < input.len() {
                let end = (cursor + 733).min(input.len());
                let progress = backend.process(&input[cursor..end], &mut output).unwrap();
                cursor += progress.input_frames;
            }
            loop {
                if backend.drain(&mut output).unwrap() == 0 {
                    break;
                }
            }
        });
    }

    #[test]
    fn clear_restores_the_selected_engines_leading_delay() {
        for (from_rate, to_rate, quality) in [
            (44_100, 48_000, ResampleQuality::High),
            (48_000, 96_000, ResampleQuality::High),
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
