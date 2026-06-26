//! FFT-based convolution for FIR filters.
//!
//! Short and medium impulse responses use a classic overlap-save engine. Long
//! impulse responses route to a uniform partitioned engine so callback work is
//! spread across fixed-size FFT blocks instead of one very large FFT.

use rustfft::{num_complex::Complex, FftPlanner};
use std::sync::Arc;

/// Per-channel IR length above which [`FFTConvolver::new`] selects the
/// partitioned path.
///
/// The value intentionally keeps current FIR EQ tap counts on the existing
/// overlap-save path while routing room/reverb-length IRs to partitioned
/// convolution. Re-run `audio_convolver_perf` and `audio_fir_eq_perf` before
/// changing it.
pub const PARTITIONED_CONVOLUTION_IR_THRESHOLD: usize = 4_096;

/// Uniform partition size used by the long-IR convolution path.
pub const PARTITIONED_CONVOLUTION_PARTITION_SIZE: usize = 1_024;

/// Runtime convolution strategy selected for an [`FFTConvolver`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvolutionStrategy {
    /// Classic single-kernel overlap-save convolution.
    OverlapSave,
    /// Head/tail uniform partitioned convolution for long impulse responses.
    Partitioned,
}

/// High-performance FFT convolver.
///
/// Zero-allocation implementation: all scratch buffers are pre-allocated at
/// construction time so `process_into`/`process_inplace` are realtime-safe.
#[derive(Clone)]
pub struct FFTConvolver {
    engine: ConvolverEngine,
}

#[derive(Clone)]
enum ConvolverEngine {
    OverlapSave(OverlapSaveConvolver),
    Partitioned(Box<PartitionedConvolver>),
}

impl FFTConvolver {
    /// Create a new FFT convolver with the given impulse response.
    ///
    /// # Arguments
    /// * `ir_data` - Impulse response samples in interleaved format [L0, R0, L1, R1, ...]
    /// * `channels` - Number of channels
    pub fn new(ir_data: &[f64], channels: usize) -> Self {
        assert!(channels > 0, "channels must be greater than zero");
        let ir_len_per_ch = ir_data.len() / channels;
        assert!(
            ir_len_per_ch > 0,
            "impulse response must contain at least one frame per channel"
        );

        let engine = if ir_len_per_ch > PARTITIONED_CONVOLUTION_IR_THRESHOLD {
            ConvolverEngine::Partitioned(Box::new(PartitionedConvolver::new(ir_data, channels)))
        } else {
            ConvolverEngine::OverlapSave(OverlapSaveConvolver::new(ir_data, channels))
        };

        Self { engine }
    }

    /// Get the IR length per channel.
    pub fn ir_length(&self) -> usize {
        match &self.engine {
            ConvolverEngine::OverlapSave(engine) => engine.ir_length(),
            ConvolverEngine::Partitioned(engine) => engine.ir_length(),
        }
    }

    /// Get the FFT size used by the active engine.
    pub fn fft_size(&self) -> usize {
        match &self.engine {
            ConvolverEngine::OverlapSave(engine) => engine.fft_size(),
            ConvolverEngine::Partitioned(engine) => engine.fft_size(),
        }
    }

    /// Return the selected convolution strategy.
    pub fn strategy(&self) -> ConvolutionStrategy {
        match &self.engine {
            ConvolverEngine::OverlapSave(_) => ConvolutionStrategy::OverlapSave,
            ConvolverEngine::Partitioned(_) => ConvolutionStrategy::Partitioned,
        }
    }

    /// Return the partition size for long-IR mode.
    pub fn partition_size(&self) -> Option<usize> {
        match &self.engine {
            ConvolverEngine::OverlapSave(_) => None,
            ConvolverEngine::Partitioned(engine) => Some(engine.partition_size()),
        }
    }

    /// Reset internal state. Call this when starting a new track to avoid artifacts.
    pub fn reset(&mut self) {
        match &mut self.engine {
            ConvolverEngine::OverlapSave(engine) => engine.reset(),
            ConvolverEngine::Partitioned(engine) => engine.reset(),
        }
    }

    /// Process audio block with zero allocation.
    ///
    /// # Arguments
    /// * `input` - Input samples in interleaved format
    /// * `output` - Output buffer (must be same size as input)
    ///
    /// # Safety
    /// This method is real-time safe: no heap allocations, no mutex, no syscalls.
    #[inline]
    pub fn process_into(&mut self, input: &[f64], output: &mut [f64]) {
        match &mut self.engine {
            ConvolverEngine::OverlapSave(engine) => engine.process_into(input, output),
            ConvolverEngine::Partitioned(engine) => engine.process_into(input, output),
        }
    }

    /// Process audio block, returning a new Vec (convenience wrapper).
    ///
    /// Note: This method allocates. For real-time use, prefer process_into().
    pub fn process(&mut self, input: &[f64]) -> Vec<f64> {
        let mut output = vec![0.0; input.len()];
        self.process_into(input, &mut output);
        output
    }

    /// Process audio block in-place with zero allocation.
    ///
    /// Uses internal scratch buffers for temporary storage.
    #[inline]
    pub fn process_inplace(&mut self, buf: &mut [f64]) {
        match &mut self.engine {
            ConvolverEngine::OverlapSave(engine) => engine.process_inplace(buf),
            ConvolverEngine::Partitioned(engine) => engine.process_inplace(buf),
        }
    }
}

/// Classic overlap-save convolver used directly for short IRs and as the
/// zero-latency head path for the partitioned engine.
#[derive(Clone)]
struct OverlapSaveConvolver {
    fft_size: usize,
    impulse_response_fft: Vec<Vec<Complex<f64>>>, // one frequency-domain response per channel
    overlap_buffers: Vec<Vec<f64>>,               // overlap buffer per channel
    channels: usize,
    ir_len: usize,
    // Cached FFT plans to avoid recreating on each process call
    fft_forward: Arc<dyn rustfft::Fft<f64>>,
    fft_inverse: Arc<dyn rustfft::Fft<f64>>,
    // Pre-allocated scratch buffers for zero-allocation processing
    scratch_complex: Vec<Complex<f64>>,
}

impl OverlapSaveConvolver {
    fn new(ir_data: &[f64], channels: usize) -> Self {
        let ir_len_total = ir_data.len();
        let ir_len_per_ch = ir_len_total / channels;

        // Pick a suitable FFT size (a power of two larger than 2*ir_len).
        let mut fft_size = 1;
        while fft_size < (ir_len_per_ch * 2) {
            fft_size <<= 1;
        }

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);

        // Create cached plans for forward and inverse FFT
        let fft_forward = planner.plan_fft_forward(fft_size);
        let fft_inverse = planner.plan_fft_inverse(fft_size);

        let mut ir_ffts = Vec::with_capacity(channels);
        let mut overlap_bufs = Vec::with_capacity(channels);

        for ch in 0..channels {
            let mut buffer = vec![Complex::new(0.0, 0.0); fft_size];
            // Load the IR for this channel and zero-pad the rest.
            for i in 0..ir_len_per_ch {
                buffer[i] = Complex::new(ir_data[i * channels + ch], 0.0);
            }
            fft.process(&mut buffer);
            ir_ffts.push(buffer);
            overlap_bufs.push(vec![0.0; ir_len_per_ch - 1]);
        }

        // Pre-allocate scratch buffer for FFT workspace
        let scratch_complex = vec![Complex::new(0.0, 0.0); fft_size];

        Self {
            fft_size,
            impulse_response_fft: ir_ffts,
            overlap_buffers: overlap_bufs,
            channels,
            ir_len: ir_len_per_ch,
            fft_forward,
            fft_inverse,
            scratch_complex,
        }
    }

    fn ir_length(&self) -> usize {
        self.ir_len
    }

    fn fft_size(&self) -> usize {
        self.fft_size
    }

    fn reset(&mut self) {
        for overlap in &mut self.overlap_buffers {
            overlap.fill(0.0);
        }
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn prepare_channel_chunk(
        scratch: &mut [Complex<f64>],
        overlap: &[f64],
        input: &[f64],
        channels: usize,
        channel: usize,
        processed_frames: usize,
        chunk_len: usize,
        ir_len: usize,
    ) {
        for i in 0..ir_len - 1 {
            scratch[i] = Complex::new(overlap[i], 0.0);
        }

        for i in 0..chunk_len {
            scratch[i + ir_len - 1] =
                Complex::new(input[(processed_frames + i) * channels + channel], 0.0);
        }
        scratch[ir_len - 1 + chunk_len..].fill(Complex::new(0.0, 0.0));
    }

    #[inline]
    fn update_channel_overlap(
        overlap: &mut [f64],
        input: &[f64],
        channels: usize,
        channel: usize,
        processed_frames: usize,
        chunk_len: usize,
        ir_len: usize,
    ) {
        if chunk_len >= ir_len - 1 {
            for i in 0..ir_len - 1 {
                overlap[i] =
                    input[(processed_frames + chunk_len - (ir_len - 1) + i) * channels + channel];
            }
        } else {
            let shift = chunk_len;
            let keep = ir_len - 1 - shift;
            overlap.copy_within(shift..shift + keep, 0);
            for i in 0..shift {
                overlap[keep + i] = input[(processed_frames + i) * channels + channel];
            }
        }
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn write_channel_output(
        scratch: &[Complex<f64>],
        output: &mut [f64],
        channels: usize,
        channel: usize,
        processed_frames: usize,
        chunk_len: usize,
        ir_len: usize,
        inv_n: f64,
    ) {
        for i in 0..chunk_len {
            output[(processed_frames + i) * channels + channel] =
                scratch[i + ir_len - 1].re * inv_n;
        }
    }

    #[inline]
    fn process_channel_chunk_fft(&mut self, channel: usize) {
        self.fft_forward.process(&mut self.scratch_complex);

        let ir_fft = &self.impulse_response_fft[channel];
        multiply_spectrum_in_place(&mut self.scratch_complex, ir_fft);

        self.fft_inverse.process(&mut self.scratch_complex);
    }

    #[inline]
    fn process_into(&mut self, input: &[f64], output: &mut [f64]) {
        debug_assert_eq!(input.len(), output.len());

        let channels = self.channels;
        let total_frames = input.len() / channels;
        let fft_size = self.fft_size;
        let ir_len = self.ir_len;
        let step_size = fft_size - ir_len + 1;
        let inv_n = 1.0 / fft_size as f64;

        // `total_frames` intentionally ignores an incomplete trailing frame.
        // Keep that remainder deterministic without clearing the whole buffer.
        output[total_frames * channels..].fill(0.0);

        for ch in 0..channels {
            let mut processed_frames = 0;

            while processed_frames < total_frames {
                let chunk_len = std::cmp::min(step_size, total_frames - processed_frames);

                Self::prepare_channel_chunk(
                    &mut self.scratch_complex,
                    &self.overlap_buffers[ch],
                    input,
                    channels,
                    ch,
                    processed_frames,
                    chunk_len,
                    ir_len,
                );
                self.process_channel_chunk_fft(ch);
                Self::write_channel_output(
                    &self.scratch_complex,
                    output,
                    channels,
                    ch,
                    processed_frames,
                    chunk_len,
                    ir_len,
                    inv_n,
                );

                Self::update_channel_overlap(
                    &mut self.overlap_buffers[ch],
                    input,
                    channels,
                    ch,
                    processed_frames,
                    chunk_len,
                    ir_len,
                );

                processed_frames += chunk_len;
            }
        }
    }

    #[inline]
    fn process_inplace(&mut self, buf: &mut [f64]) {
        // Use scratch_complex as temporary output buffer
        // First, we need a separate buffer for output since we can't read and write the same location
        // We'll use a two-phase approach: save input to scratch, process, write back

        let channels = self.channels;
        let total_frames = buf.len() / channels;
        let fft_size = self.fft_size;
        let ir_len = self.ir_len;
        let step_size = fft_size - ir_len + 1;
        let inv_n = 1.0 / fft_size as f64;

        // We need a temporary buffer for output
        // Re-purpose: use a separate approach - process channel by channel
        // For each channel, we process and immediately write back

        for ch in 0..channels {
            let mut processed_frames = 0;

            while processed_frames < total_frames {
                let chunk_len = std::cmp::min(step_size, total_frames - processed_frames);

                Self::prepare_channel_chunk(
                    &mut self.scratch_complex,
                    &self.overlap_buffers[ch],
                    buf,
                    channels,
                    ch,
                    processed_frames,
                    chunk_len,
                    ir_len,
                );
                self.process_channel_chunk_fft(ch);

                // 6. Save original input for overlap BEFORE writing output
                // (This is critical for inplace processing - we need the original input,
                // not the processed output, for the next chunk's overlap)
                Self::update_channel_overlap(
                    &mut self.overlap_buffers[ch],
                    buf,
                    channels,
                    ch,
                    processed_frames,
                    chunk_len,
                    ir_len,
                );

                // 7. Write processed output to buffer
                Self::write_channel_output(
                    &self.scratch_complex,
                    buf,
                    channels,
                    ch,
                    processed_frames,
                    chunk_len,
                    ir_len,
                    inv_n,
                );

                processed_frames += chunk_len;
            }
        }
    }
}

#[derive(Clone)]
struct PartitionedConvolver {
    channels: usize,
    ir_len: usize,
    partition_size: usize,
    fft_size: usize,
    tail_partitions: usize,
    head: OverlapSaveConvolver,
    channel_states: Vec<PartitionedChannelState>,
    fft_forward: Arc<dyn rustfft::Fft<f64>>,
    fft_inverse: Arc<dyn rustfft::Fft<f64>>,
    scratch_complex: Vec<Complex<f64>>,
    block_pos: usize,
    history_cursor: usize,
    inv_fft_size: f64,
    inplace_scratch: Vec<f64>,
}

#[derive(Clone)]
struct PartitionedChannelState {
    tail_ir_ffts: Vec<Vec<Complex<f64>>>,
    input_history_ffts: Vec<Vec<Complex<f64>>>,
    input_block: Vec<f64>,
    tail_output_block: Vec<f64>,
    tail_overlap: Vec<f64>,
}

impl PartitionedConvolver {
    fn new(ir_data: &[f64], channels: usize) -> Self {
        let ir_len = ir_data.len() / channels;
        let partition_size = PARTITIONED_CONVOLUTION_PARTITION_SIZE;
        let fft_size = partition_size * 2;
        let tail_partitions = ir_len
            .saturating_sub(partition_size)
            .div_ceil(partition_size);

        let mut planner = FftPlanner::new();
        let fft_forward = planner.plan_fft_forward(fft_size);
        let fft_inverse = planner.plan_fft_inverse(fft_size);

        let head_ir = interleaved_ir_head(ir_data, channels, partition_size);
        let head = OverlapSaveConvolver::new(&head_ir, channels);
        let mut channel_states = Vec::with_capacity(channels);

        for channel in 0..channels {
            let mut tail_ir_ffts = Vec::with_capacity(tail_partitions);

            for partition in 0..tail_partitions {
                let start_frame = partition_size * (partition + 1);
                let frames = ir_len.saturating_sub(start_frame).min(partition_size);
                let mut spectrum = vec![Complex::new(0.0, 0.0); fft_size];

                for frame in 0..frames {
                    spectrum[frame] =
                        Complex::new(ir_data[(start_frame + frame) * channels + channel], 0.0);
                }

                fft_forward.process(&mut spectrum);
                tail_ir_ffts.push(spectrum);
            }

            channel_states.push(PartitionedChannelState {
                tail_ir_ffts,
                input_history_ffts: vec![vec![Complex::new(0.0, 0.0); fft_size]; tail_partitions],
                input_block: vec![0.0; partition_size],
                tail_output_block: vec![0.0; partition_size],
                tail_overlap: vec![0.0; partition_size],
            });
        }

        Self {
            channels,
            ir_len,
            partition_size,
            fft_size,
            tail_partitions,
            head,
            channel_states,
            fft_forward,
            fft_inverse,
            scratch_complex: vec![Complex::new(0.0, 0.0); fft_size],
            block_pos: 0,
            history_cursor: 0,
            inv_fft_size: 1.0 / fft_size as f64,
            inplace_scratch: vec![0.0; partition_size * channels],
        }
    }

    fn ir_length(&self) -> usize {
        self.ir_len
    }

    fn fft_size(&self) -> usize {
        self.fft_size
    }

    fn partition_size(&self) -> usize {
        self.partition_size
    }

    fn reset(&mut self) {
        self.head.reset();
        self.block_pos = 0;
        self.history_cursor = 0;
        self.scratch_complex.fill(Complex::new(0.0, 0.0));

        for state in &mut self.channel_states {
            for history in &mut state.input_history_ffts {
                history.fill(Complex::new(0.0, 0.0));
            }
            state.input_block.fill(0.0);
            state.tail_output_block.fill(0.0);
            state.tail_overlap.fill(0.0);
        }
    }

    #[inline]
    fn process_into(&mut self, input: &[f64], output: &mut [f64]) {
        debug_assert_eq!(input.len(), output.len());

        self.head.process_into(input, output);
        self.add_partitioned_tail(input, output);
    }

    #[inline]
    fn process_inplace(&mut self, buf: &mut [f64]) {
        let total_frames = buf.len() / self.channels;
        let mut processed_frames = 0;

        while processed_frames < total_frames {
            let chunk_frames = (total_frames - processed_frames).min(self.partition_size);
            let chunk_samples = chunk_frames * self.channels;
            let start = processed_frames * self.channels;
            let end = start + chunk_samples;

            self.inplace_scratch[..chunk_samples].copy_from_slice(&buf[start..end]);
            self.head
                .process_into(&self.inplace_scratch[..chunk_samples], &mut buf[start..end]);
            self.add_partitioned_tail_from_scratch(chunk_samples, &mut buf[start..end]);

            processed_frames += chunk_frames;
        }
    }

    #[inline]
    fn add_partitioned_tail(&mut self, input: &[f64], output: &mut [f64]) {
        let total_frames = input.len() / self.channels;

        for frame in 0..total_frames {
            if self.block_pos == 0 {
                self.prepare_tail_output_block();
            }

            let block_pos = self.block_pos;
            for channel in 0..self.channels {
                let sample_index = frame * self.channels + channel;
                let state = &mut self.channel_states[channel];
                output[sample_index] += state.tail_output_block[block_pos];
                state.input_block[block_pos] = input[sample_index];
            }

            self.advance_partition_block();
        }
    }

    #[inline]
    fn add_partitioned_tail_from_scratch(&mut self, input_samples: usize, output: &mut [f64]) {
        let total_frames = input_samples / self.channels;

        for frame in 0..total_frames {
            if self.block_pos == 0 {
                self.prepare_tail_output_block();
            }

            let block_pos = self.block_pos;
            for channel in 0..self.channels {
                let sample_index = frame * self.channels + channel;
                let state = &mut self.channel_states[channel];
                output[sample_index] += state.tail_output_block[block_pos];
                state.input_block[block_pos] = self.inplace_scratch[sample_index];
            }

            self.advance_partition_block();
        }
    }

    #[inline]
    fn advance_partition_block(&mut self) {
        self.block_pos += 1;
        if self.block_pos == self.partition_size {
            self.commit_input_block();
            self.block_pos = 0;
        }
    }

    fn prepare_tail_output_block(&mut self) {
        let history_cursor = self.history_cursor;
        let tail_partitions = self.tail_partitions;
        let partition_size = self.partition_size;
        let inv_fft_size = self.inv_fft_size;

        for channel in 0..self.channels {
            self.scratch_complex.fill(Complex::new(0.0, 0.0));

            {
                let state = &self.channel_states[channel];
                for partition in 0..tail_partitions {
                    let history_index =
                        (history_cursor + tail_partitions - 1 - partition) % tail_partitions;
                    accumulate_spectrum_in_place(
                        &mut self.scratch_complex,
                        &state.input_history_ffts[history_index],
                        &state.tail_ir_ffts[partition],
                    );
                }
            }

            self.fft_inverse.process(&mut self.scratch_complex);

            let state = &mut self.channel_states[channel];
            for frame in 0..partition_size {
                state.tail_output_block[frame] =
                    self.scratch_complex[frame].re * inv_fft_size + state.tail_overlap[frame];
                state.tail_overlap[frame] =
                    self.scratch_complex[frame + partition_size].re * inv_fft_size;
            }
        }
    }

    fn commit_input_block(&mut self) {
        let history_slot = self.history_cursor;
        let partition_size = self.partition_size;

        for channel in 0..self.channels {
            self.scratch_complex[..partition_size]
                .iter_mut()
                .zip(&self.channel_states[channel].input_block)
                .for_each(|(dst, &sample)| *dst = Complex::new(sample, 0.0));
            self.scratch_complex[partition_size..].fill(Complex::new(0.0, 0.0));

            self.fft_forward.process(&mut self.scratch_complex);

            let state = &mut self.channel_states[channel];
            state.input_history_ffts[history_slot].copy_from_slice(&self.scratch_complex);
            state.input_block.fill(0.0);
        }

        self.history_cursor = (self.history_cursor + 1) % self.tail_partitions;
    }
}

fn interleaved_ir_head(ir_data: &[f64], channels: usize, partition_size: usize) -> Vec<f64> {
    let ir_len = ir_data.len() / channels;
    let head_frames = ir_len.min(partition_size);
    let mut head = vec![0.0; partition_size * channels];

    for frame in 0..head_frames {
        let src_start = frame * channels;
        let dst_start = frame * channels;
        head[dst_start..dst_start + channels]
            .copy_from_slice(&ir_data[src_start..src_start + channels]);
    }

    head
}

#[inline]
fn multiply_spectrum_in_place(samples: &mut [Complex<f64>], ir_fft: &[Complex<f64>]) {
    for (sample, ir) in samples.iter_mut().zip(ir_fft) {
        let re = sample.re * ir.re - sample.im * ir.im;
        let im = sample.re * ir.im + sample.im * ir.re;
        sample.re = re;
        sample.im = im;
    }
}

#[inline]
fn accumulate_spectrum_in_place(
    accumulator: &mut [Complex<f64>],
    input_fft: &[Complex<f64>],
    ir_fft: &[Complex<f64>],
) {
    for ((acc, input), ir) in accumulator.iter_mut().zip(input_fft).zip(ir_fft) {
        let re = input.re * ir.re - input.im * ir.im;
        let im = input.re * ir.im + input.im * ir.re;
        acc.re += re;
        acc.im += im;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convolver_identity() {
        // Identity impulse response [1.0, 0.0, 0.0, ...]
        let ir = vec![1.0, 0.0, 0.0, 0.0]; // 4 taps mono
        let mut conv = FFTConvolver::new(&ir, 1);

        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let mut output = vec![0.0; input.len()];

        conv.process_into(&input, &mut output);

        // With identity IR, output should match input
        for i in 0..input.len() {
            assert!(
                (output[i] - input[i]).abs() < 1e-10,
                "Mismatch at {}: {} vs {}",
                i,
                output[i],
                input[i]
            );
        }
    }

    #[test]
    fn test_convolver_stereo() {
        // Simple stereo IR
        let ir = vec![1.0, 1.0, 0.0, 0.0]; // 2 taps stereo (both channels same)
        let mut conv = FFTConvolver::new(&ir, 2);

        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let mut output = vec![0.0; input.len()];

        conv.process_into(&input, &mut output);

        // Verify output is not all zeros
        assert!(output.iter().any(|&x| x != 0.0));
    }

    #[test]
    fn test_zero_allocation() {
        let ir: Vec<f64> = (0..1024).map(|i| (i as f64 / 1024.0).sin()).collect();
        let mut conv = FFTConvolver::new(&ir, 1);

        let input = vec![0.5; 4096];
        let mut output = vec![0.0; 4096];

        // Multiple calls should not allocate
        for _ in 0..100 {
            conv.process_into(&input, &mut output);
        }

        // Just verify it doesn't crash
        assert!(output.iter().any(|&x| x != 0.0));
    }

    #[test]
    fn test_partitioned_strategy_selected_for_long_ir() {
        let channels = 2;
        let ir = synthetic_ir(PARTITIONED_CONVOLUTION_IR_THRESHOLD + 1, channels);
        let conv = FFTConvolver::new(&ir, channels);

        assert_eq!(conv.strategy(), ConvolutionStrategy::Partitioned);
        assert_eq!(
            conv.partition_size(),
            Some(PARTITIONED_CONVOLUTION_PARTITION_SIZE)
        );
        assert_eq!(conv.fft_size(), PARTITIONED_CONVOLUTION_PARTITION_SIZE * 2);
    }

    #[test]
    fn test_overlap_save_strategy_retained_for_short_ir() {
        let channels = 2;
        let ir = synthetic_ir(PARTITIONED_CONVOLUTION_IR_THRESHOLD, channels);
        let conv = FFTConvolver::new(&ir, channels);

        assert_eq!(conv.strategy(), ConvolutionStrategy::OverlapSave);
        assert_eq!(conv.partition_size(), None);
    }

    #[test]
    fn test_partitioned_matches_overlap_save_for_long_stereo_ir() {
        let channels = 2;
        let ir = synthetic_ir(PARTITIONED_CONVOLUTION_IR_THRESHOLD + 1537, channels);
        let input = synthetic_input(8192, channels);

        let mut reference = OverlapSaveConvolver::new(&ir, channels);
        let mut partitioned = FFTConvolver::new(&ir, channels);
        assert_eq!(partitioned.strategy(), ConvolutionStrategy::Partitioned);

        let mut expected = vec![0.0; input.len()];
        let mut actual = vec![0.0; input.len()];
        reference.process_into(&input, &mut expected);
        partitioned.process_into(&input, &mut actual);

        assert_close(&expected, &actual, 1.0e-8);
    }

    #[test]
    fn test_partitioned_matches_overlap_save_for_mono_and_surround_ir() {
        for channels in [1, 6] {
            let ir = synthetic_ir(PARTITIONED_CONVOLUTION_IR_THRESHOLD + 1537, channels);
            let input = synthetic_input(8192, channels);

            let mut reference = OverlapSaveConvolver::new(&ir, channels);
            let mut partitioned = FFTConvolver::new(&ir, channels);
            assert_eq!(partitioned.strategy(), ConvolutionStrategy::Partitioned);

            let mut expected = vec![0.0; input.len()];
            let mut actual = vec![0.0; input.len()];
            reference.process_into(&input, &mut expected);
            partitioned.process_into(&input, &mut actual);

            assert_close(&expected, &actual, 1.0e-8);
        }
    }

    #[test]
    fn test_partitioned_preserves_cross_buffer_continuity() {
        let channels = 2;
        let ir = synthetic_ir(PARTITIONED_CONVOLUTION_IR_THRESHOLD + 3073, channels);
        let input = synthetic_input(12_000, channels);
        let chunk_frames = [127, 512, 73, 1024, 1500, 31, 2048, 640, 997];

        let mut reference = OverlapSaveConvolver::new(&ir, channels);
        let mut partitioned = FFTConvolver::new(&ir, channels);
        let mut expected = vec![0.0; input.len()];
        let mut actual = vec![0.0; input.len()];
        let mut frame = 0;
        let mut chunk_index = 0;

        while frame < input.len() / channels {
            let frames =
                chunk_frames[chunk_index % chunk_frames.len()].min(input.len() / channels - frame);
            let start = frame * channels;
            let end = start + frames * channels;

            reference.process_into(&input[start..end], &mut expected[start..end]);
            partitioned.process_into(&input[start..end], &mut actual[start..end]);

            frame += frames;
            chunk_index += 1;
        }

        assert_close(&expected, &actual, 1.0e-8);
    }

    #[test]
    fn test_partitioned_reset_matches_fresh_convolver() {
        let channels = 6;
        let ir = synthetic_ir(PARTITIONED_CONVOLUTION_IR_THRESHOLD + 2049, channels);
        let warmup = synthetic_input(2048, channels);
        let input = synthetic_input(4096, channels);

        let mut reused = FFTConvolver::new(&ir, channels);
        let mut fresh = FFTConvolver::new(&ir, channels);
        let mut scratch = vec![0.0; warmup.len()];
        reused.process_into(&warmup, &mut scratch);
        reused.reset();

        let mut expected = vec![0.0; input.len()];
        let mut actual = vec![0.0; input.len()];
        fresh.process_into(&input, &mut expected);
        reused.process_into(&input, &mut actual);

        assert_close(&expected, &actual, 1.0e-8);
    }

    #[test]
    fn test_partitioned_inplace_matches_process_into() {
        let channels = 2;
        let ir = synthetic_ir(PARTITIONED_CONVOLUTION_IR_THRESHOLD + 2049, channels);
        let input = synthetic_input(8192, channels);

        let mut into_conv = FFTConvolver::new(&ir, channels);
        let mut inplace_conv = FFTConvolver::new(&ir, channels);

        let mut expected = vec![0.0; input.len()];
        let mut actual = input.clone();
        into_conv.process_into(&input, &mut expected);
        inplace_conv.process_inplace(&mut actual);

        assert_close(&expected, &actual, 1.0e-8);
    }

    #[test]
    fn test_partitioned_process_inplace_is_allocation_free_after_setup() {
        let channels = 2;
        let ir = synthetic_ir(PARTITIONED_CONVOLUTION_IR_THRESHOLD + 2049, channels);
        let mut conv = FFTConvolver::new(&ir, channels);
        let mut buffer = synthetic_input(2048, channels);

        conv.process_inplace(&mut buffer);

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..32 {
                conv.process_inplace(&mut buffer);
            }
        });
    }

    // === FIX for Defect 8: Boundary unit tests for process_inplace ===

    #[test]
    fn test_inplace_identity() {
        // Identity IR: process_inplace should preserve input
        let ir = vec![1.0, 0.0, 0.0, 0.0]; // 4 taps mono
        let mut conv = FFTConvolver::new(&ir, 1);

        let original = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let mut buf = original.clone();

        conv.process_inplace(&mut buf);

        for i in 0..original.len() {
            assert!(
                (buf[i] - original[i]).abs() < 1e-10,
                "Inplace identity mismatch at {}: {} vs {}",
                i,
                buf[i],
                original[i]
            );
        }
    }

    #[test]
    fn test_inplace_matches_process_into() {
        // Verify process_inplace produces same output as process_into
        let ir: Vec<f64> = (0..32).map(|i| (i as f64 / 32.0).sin() * 0.1).collect();
        let input: Vec<f64> = (0..256).map(|i| (i as f64 * 0.05).sin()).collect();

        let mut conv1 = FFTConvolver::new(&ir, 1);
        let mut conv2 = FFTConvolver::new(&ir, 1);

        let mut output_into = vec![0.0; input.len()];
        conv1.process_into(&input, &mut output_into);

        let mut buf_inplace = input.clone();
        conv2.process_inplace(&mut buf_inplace);

        for i in 0..input.len() {
            assert!(
                (output_into[i] - buf_inplace[i]).abs() < 1e-10,
                "Mismatch at {}: into={} vs inplace={}",
                i,
                output_into[i],
                buf_inplace[i]
            );
        }
    }

    fn assert_processing_paths_equivalent(channels: usize, ir_frames: usize, input_frames: usize) {
        let ir: Vec<f64> = (0..ir_frames * channels)
            .map(|i| ((i + 1) as f64 * 0.17).sin() * 0.05)
            .collect();
        let input: Vec<f64> = (0..input_frames * channels)
            .map(|i| ((i + 3) as f64 * 0.11).cos() * 0.5)
            .collect();

        let mut process_conv = FFTConvolver::new(&ir, channels);
        let mut into_conv = FFTConvolver::new(&ir, channels);
        let mut inplace_conv = FFTConvolver::new(&ir, channels);

        let process_output = process_conv.process(&input);

        let mut into_output = vec![f64::NAN; input.len()];
        into_conv.process_into(&input, &mut into_output);

        let mut inplace_output = input.clone();
        inplace_conv.process_inplace(&mut inplace_output);

        for i in 0..input.len() {
            assert!(
                (process_output[i] - into_output[i]).abs() < 1e-10,
                "process/process_into mismatch at {i}: {} vs {}",
                process_output[i],
                into_output[i]
            );
            assert!(
                (process_output[i] - inplace_output[i]).abs() < 1e-10,
                "process/process_inplace mismatch at {i}: {} vs {}",
                process_output[i],
                inplace_output[i]
            );
        }
    }

    #[test]
    fn test_processing_paths_equivalent_for_boundary_chunk_sizes() {
        assert_processing_paths_equivalent(1, 8, 4);
        assert_processing_paths_equivalent(2, 8, 8);
        assert_processing_paths_equivalent(6, 8, 20);
    }

    #[test]
    fn test_inplace_small_buffer() {
        // Buffer smaller than IR length
        let ir = vec![1.0, 0.5, 0.25, 0.125, 0.0, 0.0, 0.0, 0.0]; // 8 taps mono
        let mut conv = FFTConvolver::new(&ir, 1);

        // Only 4 samples (less than 8-tap IR)
        let mut buf = vec![1.0, 0.0, 0.0, 0.0];
        conv.process_inplace(&mut buf);

        // Should produce convolution of delta with IR, truncated to 4 samples
        // Result: [1.0, 0.5, 0.25, 0.125]
        assert!((buf[0] - 1.0).abs() < 1e-10, "Expected 1.0, got {}", buf[0]);
        assert!((buf[1] - 0.5).abs() < 1e-10, "Expected 0.5, got {}", buf[1]);
        assert!(
            (buf[2] - 0.25).abs() < 1e-10,
            "Expected 0.25, got {}",
            buf[2]
        );
        assert!(
            (buf[3] - 0.125).abs() < 1e-10,
            "Expected 0.125, got {}",
            buf[3]
        );
    }

    #[test]
    fn test_inplace_stereo_identity() {
        // Stereo identity IR
        let ir = vec![1.0, 1.0, 0.0, 0.0]; // 2 taps stereo identity
        let mut conv = FFTConvolver::new(&ir, 2);

        let original = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]; // 4 frames stereo
        let mut buf = original.clone();

        conv.process_inplace(&mut buf);

        for i in 0..original.len() {
            assert!(
                (buf[i] - original[i]).abs() < 1e-10,
                "Stereo inplace identity mismatch at {}: {} vs {}",
                i,
                buf[i],
                original[i]
            );
        }
    }

    #[test]
    fn test_inplace_multi_chunk() {
        // Multiple consecutive calls with continuity
        let ir = vec![1.0, 0.5, 0.0, 0.0]; // 4 taps mono
        let mut conv = FFTConvolver::new(&ir, 1);

        let mut buf1 = vec![1.0, 0.0, 0.0, 0.0];
        conv.process_inplace(&mut buf1);

        // Second chunk should carry overlap from first
        let mut buf2 = vec![0.0, 0.0, 0.0, 0.0];
        conv.process_inplace(&mut buf2);

        // buf1 should be [1.0, 0.5, 0.0, 0.0]
        assert!((buf1[0] - 1.0).abs() < 1e-10);
        assert!((buf1[1] - 0.5).abs() < 1e-10);
    }

    fn synthetic_ir(frames: usize, channels: usize) -> Vec<f64> {
        let mut ir = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            let decay = (-(frame as f64) / 900.0).exp();
            for channel in 0..channels {
                let direct = if frame == 0 { 0.8 } else { 0.0 };
                let early = if frame == 97 + channel * 13 {
                    0.08
                } else {
                    0.0
                };
                let tail = ((frame + 1 + channel * 17) as f64 * 0.021).sin() * decay * 0.03;
                ir.push(direct + early + tail);
            }
        }
        ir
    }

    fn synthetic_input(frames: usize, channels: usize) -> Vec<f64> {
        let mut input = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            for channel in 0..channels {
                let phase = frame as f64 * (0.013 + channel as f64 * 0.002);
                let transient = if frame % 257 == 0 { 0.25 } else { 0.0 };
                input.push((phase.sin() * 0.4 + (phase * 0.31).cos() * 0.1 + transient) * 0.7);
            }
        }
        input
    }

    fn assert_close(expected: &[f64], actual: &[f64], tolerance: f64) {
        assert_eq!(expected.len(), actual.len());
        for (index, (&left, &right)) in expected.iter().zip(actual).enumerate() {
            let delta = (left - right).abs();
            assert!(
                delta <= tolerance,
                "sample {index} mismatch: expected {left:.12e}, actual {right:.12e}, delta {delta:.3e}"
            );
        }
    }
}
