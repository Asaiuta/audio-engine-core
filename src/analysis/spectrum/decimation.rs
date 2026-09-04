//! Polyphase half-band decimation for spectrum analyzer tier feeding.
//!
//! This module implements cascaded half-band lowpass decimation (÷2 per stage)
//! using Kaiser-windowed FIR filters. The decimation chain feeds lower-frequency
//! spectrum analyzer tiers with decimated input, reducing memory and computation.

/// Half-band decimator (÷2) using direct-form FIR.
///
/// Applies an N-tap FIR filter and downsamples by 2, producing floor(n/2) outputs.
/// Maintains state across calls for seamless streaming.
pub(super) struct HalfbandDecimator {
    /// FIR kernel coefficients
    kernel: Vec<f64>,
    /// Input history buffer: [history(N-1) | new_samples(max_block)]
    input_buf: Vec<f64>,
    /// History length (N-1 for N-tap filter)
    hist_len: usize,
    /// Trailing unpaired sample from previous call
    trailing_sample: Option<f64>,
}

impl HalfbandDecimator {
    /// Create a new decimator with the given kernel and maximum block size.
    pub(super) fn new(kernel: Vec<f64>, max_block_size: usize) -> Self {
        let n = kernel.len();
        assert!(n % 2 == 1, "kernel must be odd length");
        let hist_len = n - 1;

        Self {
            kernel,
            input_buf: vec![0.0; hist_len + max_block_size],
            hist_len,
            trailing_sample: None,
        }
    }

    /// Process input samples, producing floor((trailing + n)/2) output samples.
    ///
    /// Applies FIR filter y[k] = Σ h[i] · x[2k - i] and returns output count.
    /// Bit-identical output regardless of input chunking.
    pub(super) fn process(&mut self, input: &[f64], output: &mut [f64]) -> usize {
        if input.is_empty() {
            return 0;
        }

        // Build the processing buffer: [trailing_sample (if any)] + [new input]
        let has_trailing = self.trailing_sample.is_some();
        if let Some(trail) = self.trailing_sample.take() {
            self.input_buf[self.hist_len] = trail;
            self.input_buf[self.hist_len + 1..self.hist_len + 1 + input.len()]
                .copy_from_slice(input);
        } else {
            self.input_buf[self.hist_len..self.hist_len + input.len()].copy_from_slice(input);
        }

        // Total samples available to process
        let available = if has_trailing {
            1 + input.len()
        } else {
            input.len()
        };

        let out_count = available / 2;

        if out_count == 0 {
            // Not enough for even one output, store as trailing
            self.trailing_sample = Some(input[0]);
            return 0;
        }

        // Compute outputs: y[k] = Σ_{i=0}^{N-1} h[i] · x[2k + hist_len - i]
        for (k, out_sample) in output.iter_mut().take(out_count).enumerate() {
            let center_pos = self.hist_len + 2 * k;
            let mut sum = 0.0;

            for (i, &coeff) in self.kernel.iter().enumerate() {
                sum += coeff * self.input_buf[center_pos - i];
            }

            *out_sample = sum;
        }

        // Store trailing sample if odd number of total available samples
        if available % 2 == 1 {
            self.trailing_sample = Some(self.input_buf[self.hist_len + 2 * out_count]);
        }

        // Shift history: the hist_len samples immediately before the next unprocessed position
        let next_pos = self.hist_len + 2 * out_count + (available % 2);
        self.input_buf
            .copy_within(next_pos - self.hist_len..next_pos, 0);

        out_count
    }

    /// Reset the decimator state.
    pub(super) fn reset(&mut self) {
        self.input_buf.fill(0.0);
        self.trailing_sample = None;
    }
}

/// Cascaded half-band decimation chain (÷2 per stage).
///
/// Four stages: input → ÷2 → ÷2 → ÷2 → ÷2
/// - Stage 1+2: tier B output (÷4)
/// - Stage 1+2+3+4: tier C output (÷16)
pub(super) struct DecimationChain {
    stage1: HalfbandDecimator,
    stage2: HalfbandDecimator,
    stage3: HalfbandDecimator,
    stage4: HalfbandDecimator,
    // Scratch buffers for intermediate stages
    scratch1: Vec<f64>,
    scratch2: Vec<f64>,
}

impl DecimationChain {
    /// Create a new decimation chain with 63-tap Kaiser half-band filters.
    pub(super) fn new(max_block_size: usize) -> Self {
        use crate::dsp::halfband_lowpass_kernel;

        let kernel = halfband_lowpass_kernel(63, 9.6);

        // Maximum outputs per stage, accounting for potential trailing sample from previous call
        // Stage 1: up to (max_block_size + 1) / 2
        // Stage 2: up to (max_stage1 + 1) / 2, etc.
        let max_stage1 = max_block_size.div_ceil(2) + 1;
        let max_stage2 = max_stage1.div_ceil(2) + 1;
        let max_stage3 = max_stage2.div_ceil(2) + 1;

        Self {
            stage1: HalfbandDecimator::new(kernel.clone(), max_block_size),
            stage2: HalfbandDecimator::new(kernel.clone(), max_stage1),
            stage3: HalfbandDecimator::new(kernel.clone(), max_stage2),
            stage4: HalfbandDecimator::new(kernel, max_stage3),
            scratch1: vec![0.0; max_stage1],
            scratch2: vec![0.0; max_stage2],
        }
    }

    /// Process input, producing tier B (÷4) and tier C (÷16) outputs.
    /// Returns (tier_b_count, tier_c_count).
    pub(super) fn process(
        &mut self,
        input: &[f64],
        tier_b_out: &mut [f64],
        tier_c_out: &mut [f64],
    ) -> (usize, usize) {
        let n1 = self.stage1.process(input, &mut self.scratch1);
        let n2 = self.stage2.process(&self.scratch1[..n1], tier_b_out);
        let n3 = self
            .stage3
            .process(tier_b_out[..n2].as_ref(), &mut self.scratch2);
        let n4 = self.stage4.process(&self.scratch2[..n3], tier_c_out);

        (n2, n4)
    }

    /// Reset all decimator stages.
    pub(super) fn reset(&mut self) {
        self.stage1.reset();
        self.stage2.reset();
        self.stage3.reset();
        self.stage4.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::halfband_lowpass_kernel;

    #[test]
    fn decimator_output_count_floor_n_div_2() {
        let kernel = halfband_lowpass_kernel(63, 9.6);
        let mut dec = HalfbandDecimator::new(kernel, 256);
        let mut out = vec![0.0; 128];

        // Even input → n/2 outputs
        let input_even = vec![0.0; 100];
        let n = dec.process(&input_even, &mut out);
        assert_eq!(n, 50);

        // Odd input → (n-1)/2 outputs, 1 trailing
        let input_odd = vec![0.0; 101];
        let n = dec.process(&input_odd, &mut out);
        assert_eq!(n, 50);

        // Next even input picks up trailing → (trailing + 100) / 2 = 50 (with new trailing)
        let input_even2 = vec![0.0; 100];
        let n = dec.process(&input_even2, &mut out);
        assert_eq!(n, 50);
    }

    #[test]
    fn decimator_dc_input_unity_output() {
        let kernel = halfband_lowpass_kernel(63, 9.6);
        let mut dec = HalfbandDecimator::new(kernel, 256);

        // Feed DC=1.0, expect output ~1.0 after warmup
        let input = vec![1.0; 200];
        let mut output = vec![0.0; 100];
        let n = dec.process(&input, &mut output);

        // Check last output (fully warmed up)
        let dc_out = output[n - 1];
        assert!(
            (dc_out - 1.0).abs() < 1e-6,
            "DC output {}, expected 1.0",
            dc_out
        );
    }

    #[test]
    fn decimator_chunked_equivalence() {
        let kernel = halfband_lowpass_kernel(63, 9.6);

        // Process in one block
        let mut dec1 = HalfbandDecimator::new(kernel.clone(), 512);
        let input = vec![1.0; 256];
        let mut out1 = vec![0.0; 256];
        let n1 = dec1.process(&input, &mut out1);

        // Process in chunks
        let mut dec2 = HalfbandDecimator::new(kernel, 512);
        let mut out2_buffer = vec![0.0; 256];
        let mut total = 0;
        for chunk in input.chunks(37) {
            let mut chunk_out = vec![0.0; 128];
            let n = dec2.process(chunk, &mut chunk_out);
            out2_buffer[total..total + n].copy_from_slice(&chunk_out[..n]);
            total += n;
        }

        assert_eq!(n1, total, "output counts differ");
        for i in 0..n1 {
            let diff = (out1[i] - out2_buffer[i]).abs();
            assert!(
                diff < 1e-12,
                "output[{}] differs: {} vs {} (diff {})",
                i,
                out1[i],
                out2_buffer[i],
                diff
            );
        }
    }

    #[test]
    fn decimator_stopband_attenuation() {
        let kernel = halfband_lowpass_kernel(63, 9.6);
        let mut dec = HalfbandDecimator::new(kernel, 4096);

        // 0.35×fs tone (well into stopband of 0.5×fs decimated = 0.25×fs original)
        let input: Vec<f64> = (0..2048)
            .map(|i| (2.0 * std::f64::consts::PI * 0.35 * i as f64).sin())
            .collect();

        let mut output = vec![0.0; 1024];
        let n = dec.process(&input, &mut output);

        // Measure RMS after warmup (skip first 100 samples)
        let start = 100.min(n);
        let rms: f64 = output[start..n].iter().map(|&x| x * x).sum::<f64>() / (n - start) as f64;
        let rms = rms.sqrt();
        let db = 20.0 * rms.log10();

        assert!(
            db <= -90.0,
            "Stopband attenuation {} dB, expected >= 90 dB",
            -db
        );
    }

    #[test]
    fn decimator_tone_amplitude_preserved() {
        let kernel = halfband_lowpass_kernel(63, 9.6);
        let mut dec = HalfbandDecimator::new(kernel, 4096);

        // 0.15×fs tone (well within passband 0 to 0.2×fs)
        let input: Vec<f64> = (0..2048)
            .map(|i| (2.0 * std::f64::consts::PI * 0.15 * i as f64).sin())
            .collect();

        let mut output = vec![0.0; 1024];
        let n = dec.process(&input, &mut output);

        // Measure RMS after warmup
        let start = 100.min(n);
        let rms: f64 = output[start..n].iter().map(|&x| x * x).sum::<f64>() / (n - start) as f64;
        let rms = rms.sqrt();
        let expected_rms = 1.0 / 2.0_f64.sqrt(); // sin wave RMS
        let error_db = 20.0 * (rms / expected_rms).log10();

        assert!(
            error_db.abs() < 0.01,
            "Tone amplitude error {} dB, expected < 0.01 dB",
            error_db
        );
    }

    #[test]
    fn decimation_chain_tier_b_and_c_counts() {
        let mut chain = DecimationChain::new(512);
        let input = vec![1.0; 512];
        let mut tier_b = vec![0.0; 256];
        let mut tier_c = vec![0.0; 64];

        let (b_count, c_count) = chain.process(&input, &mut tier_b, &mut tier_c);

        // 512 input → 256 stage1 → 128 stage2 (tier B) → 64 stage3 → 32 stage4 (tier C)
        assert_eq!(b_count, 128, "tier B count");
        assert_eq!(c_count, 32, "tier C count");
    }
}
