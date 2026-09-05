//! Polyphase half-band decimation for spectrum analyzer tier feeding.
//!
//! This module implements cascaded half-band lowpass decimation (÷2 per stage)
//! using Kaiser-windowed FIR filters. The decimation chain feeds lower-frequency
//! spectrum analyzer tiers with decimated input, reducing memory and computation.

/// Half-band decimator (÷2) using the two non-zero polyphase branches.
///
/// Applies an N-tap FIR filter and downsamples by 2, producing floor(n/2) outputs.
/// Maintains state across calls for seamless streaming.
pub(super) struct HalfbandDecimator {
    /// Symmetric coefficient pairs from the odd-sample polyphase branch.
    pair_coefficients: Vec<f64>,
    /// Center tap applied to the delayed even-sample branch.
    center_gain: f64,
    /// `[history | current block]` storage for even-indexed input samples.
    even_samples: Vec<f64>,
    /// `[history | current block]` storage for odd-indexed input samples.
    odd_samples: Vec<f64>,
    even_history_len: usize,
    odd_history_len: usize,
    max_output_samples: usize,
    /// Unpaired even-indexed sample waiting for its odd partner.
    pending_even: Option<f64>,
}

impl HalfbandDecimator {
    /// Create a new decimator with the given kernel and maximum block size.
    pub(super) fn new(kernel: Vec<f64>, max_block_size: usize) -> Self {
        let n = kernel.len();
        assert!(n >= 3 && n % 4 == 3, "kernel length must be 4m + 3");
        let even_history_len = (n - 3) / 4;
        let odd_history_len = 2 * even_history_len + 1;
        let center = odd_history_len;
        let pair_coefficients = (0..=even_history_len)
            .map(|pair| kernel[pair * 2])
            .collect();
        let max_output_samples = max_block_size.div_ceil(2);

        Self {
            pair_coefficients,
            center_gain: kernel[center],
            even_samples: vec![0.0; even_history_len + max_output_samples],
            odd_samples: vec![0.0; odd_history_len + max_output_samples],
            even_history_len,
            odd_history_len,
            max_output_samples,
            pending_even: None,
        }
    }

    /// Process input samples, producing floor((pending + n)/2) output samples.
    ///
    /// Applies FIR filter y[k] = Σ h[i] · x[2k + 1 - i] and returns output count.
    /// Bit-identical output regardless of input chunking.
    pub(super) fn process(&mut self, input: &[f64], output: &mut [f64]) -> usize {
        if input.is_empty() {
            return 0;
        }

        let pending_even = self.pending_even.take();
        let available = input.len() + usize::from(pending_even.is_some());
        let out_count = available / 2;
        if out_count == 0 {
            self.pending_even = Some(input[0]);
            return 0;
        }
        debug_assert!(out_count <= self.max_output_samples);
        debug_assert!(out_count <= output.len());

        let even_start = self.even_history_len;
        let odd_start = self.odd_history_len;
        let mut input_index = 0;
        let mut pair_index = 0;

        if let Some(even) = pending_even {
            self.even_samples[even_start] = even;
            self.odd_samples[odd_start] = input[0];
            input_index = 1;
            pair_index = 1;
        }
        while pair_index < out_count {
            self.even_samples[even_start + pair_index] = input[input_index];
            self.odd_samples[odd_start + pair_index] = input[input_index + 1];
            input_index += 2;
            pair_index += 1;
        }
        if available % 2 == 1 {
            self.pending_even = input.last().copied();
        }

        output[..out_count].fill(0.0);
        for (pair, &coefficient) in self.pair_coefficients.iter().enumerate() {
            let recent_start = odd_start - pair;
            let older_start = pair;
            for (k, out_sample) in output.iter_mut().take(out_count).enumerate() {
                *out_sample += coefficient
                    * (self.odd_samples[recent_start + k] + self.odd_samples[older_start + k]);
            }
        }
        for (k, out_sample) in output.iter_mut().take(out_count).enumerate() {
            *out_sample += self.center_gain * self.even_samples[k];
        }

        self.even_samples
            .copy_within(out_count..out_count + self.even_history_len, 0);
        self.odd_samples
            .copy_within(out_count..out_count + self.odd_history_len, 0);

        out_count
    }

    /// Reset the decimator state.
    pub(super) fn reset(&mut self) {
        self.even_samples.fill(0.0);
        self.odd_samples.fill(0.0);
        self.pending_even = None;
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

        // Maximum outputs per stage, accounting for a pending even sample from the previous call.
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

        // Odd input → (n-1)/2 outputs, 1 pending even sample.
        let input_odd = vec![0.0; 101];
        let n = dec.process(&input_odd, &mut out);
        assert_eq!(n, 50);

        // The next even block pairs the pending sample first and leaves a new pending sample.
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
        let input: Vec<f64> = (0..4_097)
            .map(|index| {
                let t = index as f64;
                0.25 + 0.003 * t + (t * 0.17).sin() * 0.4
            })
            .collect();
        let mut dec1 = HalfbandDecimator::new(kernel.clone(), input.len());
        let mut out1 = vec![0.0; input.len() / 2];
        let n1 = dec1.process(&input, &mut out1);

        for block_size in [1, 7, 64, 1000] {
            let mut dec = HalfbandDecimator::new(kernel.clone(), block_size);
            let mut chunk_output = vec![0.0; input.len() / 2];
            let mut total = 0;
            for chunk in input.chunks(block_size) {
                let n = dec.process(chunk, &mut chunk_output[total..]);
                total += n;
            }

            assert_eq!(
                n1, total,
                "output counts differ for block size {block_size}"
            );
            for (index, (&expected, &actual)) in
                out1[..n1].iter().zip(&chunk_output[..total]).enumerate()
            {
                assert_eq!(
                    expected.to_bits(),
                    actual.to_bits(),
                    "output[{index}] differs for block size {block_size}: {expected} vs {actual}"
                );
            }
        }
    }

    #[test]
    fn polyphase_output_matches_full_fir_oracle() {
        let kernel = halfband_lowpass_kernel(63, 9.6);
        let input: Vec<f64> = (0..513)
            .map(|index| 0.2 * index as f64 + (index as f64 * 0.31).sin())
            .collect();
        let mut expected = vec![0.0; input.len() / 2];
        for (output_index, output_sample) in expected.iter_mut().enumerate() {
            let end = 2 * output_index + 1;
            for (tap, &coefficient) in kernel.iter().enumerate().take(end + 1) {
                *output_sample += coefficient * input[end - tap];
            }
        }

        let mut decimator = HalfbandDecimator::new(kernel, input.len());
        let mut actual = vec![0.0; expected.len()];
        assert_eq!(decimator.process(&input, &mut actual), expected.len());
        for (index, (&expected, &actual)) in expected.iter().zip(&actual).enumerate() {
            assert!(
                (expected - actual).abs() <= 1.0e-12,
                "output[{index}] differed: expected {expected}, actual {actual}"
            );
        }
    }

    #[test]
    fn reset_restores_fresh_state_after_history_and_pending_input() {
        let kernel = halfband_lowpass_kernel(63, 9.6);
        let history: Vec<f64> = (0..129).map(|index| (index as f64 * 0.07).sin()).collect();
        let probe: Vec<f64> = (0..128).map(|index| (index as f64 * 0.19).cos()).collect();
        let mut after_reset = HalfbandDecimator::new(kernel.clone(), 256);
        let mut scratch = vec![0.0; 128];
        after_reset.process(&history, &mut scratch);
        after_reset.reset();

        let mut fresh = HalfbandDecimator::new(kernel, 256);
        let mut expected = vec![0.0; 64];
        let mut actual = vec![0.0; 64];
        assert_eq!(fresh.process(&probe, &mut expected), expected.len());
        assert_eq!(after_reset.process(&probe, &mut actual), actual.len());
        for (index, (&expected, &actual)) in expected.iter().zip(&actual).enumerate() {
            assert_eq!(
                expected.to_bits(),
                actual.to_bits(),
                "output[{index}] retained pre-reset state"
            );
        }
    }

    #[test]
    fn decimator_process_is_allocation_free_after_setup() {
        let kernel = halfband_lowpass_kernel(63, 9.6);
        let mut dec = HalfbandDecimator::new(kernel, 512);
        let input: Vec<f64> = (0..256).map(|index| (index as f64 * 0.13).sin()).collect();
        let mut output = vec![0.0; 128];
        dec.process(&input, &mut output);

        assert_no_alloc::assert_no_alloc(|| {
            assert_eq!(dec.process(&input, &mut output), 128);
        });
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
