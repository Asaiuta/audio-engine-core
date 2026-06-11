//! FFT-based spectrum analyzer for visualization

use rustfft::{num_complex::Complex, FftPlanner};
use std::sync::Arc;

/// FFT-based spectrum analyzer for visualization
pub struct SpectrumAnalyzer {
    fft_size: usize,
    fft: Arc<dyn rustfft::Fft<f64>>,
    window: Vec<f64>,
    num_bins: usize,
    fft_buffer: Vec<Complex<f64>>,
    magnitudes: Vec<f64>,
    result: Vec<f32>,
    bin_ranges: Vec<(usize, usize)>,
    bin_sample_rate: Option<u32>,
}

impl SpectrumAnalyzer {
    pub fn new(fft_size: usize, num_bins: usize) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        let window: Vec<f64> = (0..fft_size)
            .map(|i| 0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / fft_size as f64).cos()))
            .collect();

        Self {
            fft_size,
            fft,
            window,
            num_bins,
            fft_buffer: vec![Complex::new(0.0, 0.0); fft_size],
            magnitudes: vec![0.0; fft_size.saturating_div(2).saturating_sub(1)],
            result: vec![0.0; num_bins],
            bin_ranges: Vec::with_capacity(num_bins),
            bin_sample_rate: None,
        }
    }

    pub fn analyze(&mut self, samples: &[f64], sample_rate: u32) -> &[f32] {
        if samples.len() < self.fft_size {
            self.result.fill(0.0);
            return &self.result;
        }

        for ((slot, &sample), &window) in self
            .fft_buffer
            .iter_mut()
            .zip(samples.iter().take(self.fft_size))
            .zip(&self.window)
        {
            *slot = Complex::new(sample * window, 0.0);
        }

        self.fft.process(&mut self.fft_buffer);

        for (dst, c) in self
            .magnitudes
            .iter_mut()
            .zip(self.fft_buffer[1..self.fft_size / 2].iter())
        {
            *dst = c.norm() / self.fft_size as f64;
        }

        self.ensure_bin_ranges(sample_rate);
        self.log_bin();
        &self.result
    }

    fn ensure_bin_ranges(&mut self, sample_rate: u32) {
        if self.bin_sample_rate == Some(sample_rate) && self.bin_ranges.len() == self.num_bins {
            return;
        }

        let nyquist = sample_rate as f64 / 2.0;
        let min_freq = 20.0f64;
        let max_freq = nyquist;
        let log_min = min_freq.log10();
        let log_max = max_freq.log10();
        let freq_per_bin = nyquist / self.magnitudes.len().max(1) as f64;

        self.bin_ranges.clear();
        for bin_idx in 0..self.num_bins {
            let freq_low = 10.0_f64
                .powf(log_min + (log_max - log_min) * bin_idx as f64 / self.num_bins as f64);
            let freq_high = 10.0_f64
                .powf(log_min + (log_max - log_min) * (bin_idx + 1) as f64 / self.num_bins as f64);
            let idx_low = ((freq_low / freq_per_bin) as usize)
                .clamp(0, self.magnitudes.len().saturating_sub(1));
            let idx_high =
                ((freq_high / freq_per_bin) as usize).clamp(idx_low + 1, self.magnitudes.len());
            self.bin_ranges.push((idx_low, idx_high));
        }
        self.bin_sample_rate = Some(sample_rate);
    }

    fn log_bin(&mut self) {
        self.result.fill(0.0);
        for (result_val, &(idx_low, idx_high)) in self.result.iter_mut().zip(&self.bin_ranges) {
            if idx_high > idx_low {
                let sum: f64 = self.magnitudes[idx_low..idx_high]
                    .iter()
                    .map(|m| m * m)
                    .sum();
                let rms = (sum / (idx_high - idx_low) as f64).sqrt();
                let db = 20.0 * (rms + 1e-9).log10();
                *result_val = ((db + 90.0) / 90.0).clamp(0.0, 1.0) as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfft::FftPlanner;

    #[test]
    fn short_input_returns_reused_zero_bins() {
        let mut analyzer = SpectrumAnalyzer::new(16, 4);
        let first_ptr = analyzer.analyze(&[0.0; 8], 48_000).as_ptr();
        assert_eq!(analyzer.analyze(&[0.0; 8], 48_000), &[0.0; 4]);
        assert_eq!(analyzer.analyze(&[0.0; 8], 48_000).as_ptr(), first_ptr);
    }

    #[test]
    fn analyze_reuses_result_and_recomputes_ranges_on_sample_rate_change() {
        let mut analyzer = SpectrumAnalyzer::new(64, 8);
        let samples: Vec<f64> = (0..64).map(|i| (i as f64 * 0.1).sin()).collect();

        let first_ptr = analyzer.analyze(&samples, 48_000).as_ptr();
        let first_ranges = analyzer.bin_ranges.clone();
        assert!(analyzer.analyze(&samples, 48_000).iter().any(|&v| v > 0.0));
        assert_eq!(analyzer.analyze(&samples, 48_000).as_ptr(), first_ptr);
        assert_eq!(analyzer.bin_ranges, first_ranges);

        analyzer.analyze(&samples, 96_000);
        assert_ne!(analyzer.bin_ranges, first_ranges);
    }

    #[test]
    fn analyzer_output_matches_legacy_allocation_path() {
        let mut analyzer = SpectrumAnalyzer::new(128, 16);
        let samples: Vec<f64> = (0..128)
            .map(|i| {
                let t = i as f64 / 48_000.0;
                (2.0 * std::f64::consts::PI * 997.0 * t).sin() * 0.4
            })
            .collect();

        let actual = analyzer.analyze(&samples, 48_000).to_vec();
        let expected = legacy_analyze(&samples, 128, 16, 48_000);

        for (idx, (actual, expected)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!(
                (actual - expected).abs() <= 1e-6,
                "bin {idx}: actual={actual}, expected={expected}"
            );
        }
    }

    fn legacy_analyze(
        samples: &[f64],
        fft_size: usize,
        num_bins: usize,
        sample_rate: u32,
    ) -> Vec<f32> {
        if samples.len() < fft_size {
            return vec![0.0; num_bins];
        }

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        let window: Vec<f64> = (0..fft_size)
            .map(|i| 0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / fft_size as f64).cos()))
            .collect();
        let mut buffer: Vec<Complex<f64>> = samples[..fft_size]
            .iter()
            .zip(&window)
            .map(|(&s, &w)| Complex::new(s * w, 0.0))
            .collect();

        fft.process(&mut buffer);
        let magnitudes: Vec<f64> = buffer[1..fft_size / 2]
            .iter()
            .map(|c| c.norm() / fft_size as f64)
            .collect();
        legacy_log_bin(&magnitudes, sample_rate, num_bins)
    }

    fn legacy_log_bin(magnitudes: &[f64], sample_rate: u32, num_bins: usize) -> Vec<f32> {
        let mut result = vec![0.0f32; num_bins];
        let nyquist = sample_rate as f64 / 2.0;
        let min_freq = 20.0f64;
        let max_freq = nyquist;
        let log_min = min_freq.log10();
        let log_max = max_freq.log10();

        for (bin_idx, result_val) in result.iter_mut().enumerate() {
            let freq_low =
                10.0_f64.powf(log_min + (log_max - log_min) * bin_idx as f64 / num_bins as f64);
            let freq_high = 10.0_f64
                .powf(log_min + (log_max - log_min) * (bin_idx + 1) as f64 / num_bins as f64);
            let freq_per_bin = nyquist / magnitudes.len() as f64;
            let idx_low =
                ((freq_low / freq_per_bin) as usize).clamp(0, magnitudes.len().saturating_sub(1));
            let idx_high =
                ((freq_high / freq_per_bin) as usize).clamp(idx_low + 1, magnitudes.len());

            if idx_high > idx_low {
                let sum: f64 = magnitudes[idx_low..idx_high].iter().map(|m| m * m).sum();
                let rms = (sum / (idx_high - idx_low) as f64).sqrt();
                let db = 20.0 * (rms + 1e-9).log10();
                *result_val = ((db + 90.0) / 90.0).clamp(0.0, 1.0) as f32;
            }
        }

        result
    }
}
