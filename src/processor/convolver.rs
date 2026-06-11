//! FFT-based convolution for long FIR filters (Overlap-Save algorithm)
//!
//! Zero-allocation real-time implementation with pre-allocated scratch buffers.

use rustfft::{num_complex::Complex, FftPlanner};
use std::sync::Arc;

/// High-performance FFT convolver (Overlap-Save algorithm).
///
/// Zero-allocation implementation: all scratch buffers are pre-allocated at
/// construction time so `process_into`/`process_inplace` are realtime-safe.
pub struct FFTConvolver {
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

impl Clone for FFTConvolver {
    fn clone(&self) -> Self {
        Self {
            fft_size: self.fft_size,
            impulse_response_fft: self.impulse_response_fft.clone(),
            overlap_buffers: self.overlap_buffers.clone(),
            channels: self.channels,
            ir_len: self.ir_len,
            fft_forward: Arc::clone(&self.fft_forward),
            fft_inverse: Arc::clone(&self.fft_inverse),
            scratch_complex: self.scratch_complex.clone(),
        }
    }
}

impl FFTConvolver {
    /// Create a new FFT convolver with the given impulse response
    ///
    /// # Arguments
    /// * `ir_data` - Impulse response samples in interleaved format [L0, R0, L1, R1, ...]
    /// * `channels` - Number of channels
    pub fn new(ir_data: &[f64], channels: usize) -> Self {
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

        FFTConvolver {
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

    /// Get the IR length per channel
    pub fn ir_length(&self) -> usize {
        self.ir_len
    }

    /// Get the FFT size used
    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    /// Reset internal state (overlap buffers)
    /// Call this when starting a new track to avoid artifacts
    pub fn reset(&mut self) {
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

    /// Process audio block with zero allocation
    ///
    /// # Arguments
    /// * `input` - Input samples in interleaved format
    /// * `output` - Output buffer (must be same size as input)
    ///
    /// # Safety
    /// This method is real-time safe: no heap allocations, no mutex, no syscalls
    #[inline]
    pub fn process_into(&mut self, input: &[f64], output: &mut [f64]) {
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

    /// Process audio block, returning a new Vec (convenience wrapper)
    ///
    /// Note: This method allocates. For real-time use, prefer process_into().
    pub fn process(&mut self, input: &[f64]) -> Vec<f64> {
        let mut output = vec![0.0; input.len()];
        self.process_into(input, &mut output);
        output
    }

    /// Process audio block in-place with zero allocation
    ///
    /// Uses internal scratch buffer for temporary storage.
    /// This is the recommended method for real-time audio processing.
    ///
    /// # Arguments
    /// * `buf` - Input/output samples in interleaved format (modified in place)
    #[inline]
    pub fn process_inplace(&mut self, buf: &mut [f64]) {
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

#[inline]
fn multiply_spectrum_in_place(samples: &mut [Complex<f64>], ir_fft: &[Complex<f64>]) {
    for (sample, ir) in samples.iter_mut().zip(ir_fft) {
        let re = sample.re * ir.re - sample.im * ir.im;
        let im = sample.re * ir.im + sample.im * ir.re;
        sample.re = re;
        sample.im = im;
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
}
