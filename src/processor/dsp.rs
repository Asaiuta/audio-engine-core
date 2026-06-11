//! DSP utilities - Volume control and Noise shaping
//!
//! NoiseShaper implementation based on SoX dither.c coefficients
//! with NTF-verified stability and realtime-safe xorshift64 RNG.

use serde::{Deserialize, Serialize};

const VOLUME_SMOOTHING_TIME_MS: f64 = 20.0;
const INV_U64_MAX: f64 = 1.0 / u64::MAX as f64;

// ============================================================================
// Common DSP Utility Functions (P1-4: centralized, previously duplicated)
// ============================================================================

/// Convert dB to linear gain. Shared across all processor modules.
#[inline(always)]
pub fn db_to_linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

/// Convert linear gain to dB. Shared across all processor modules.
#[inline(always)]
pub fn linear_to_db(linear: f64) -> f64 {
    if linear > 0.0 {
        20.0 * linear.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Volume controller with anti-zipper smoothing
///
/// FIX for Defect 36: Smoothing coefficient is now sample-rate aware.
/// The smoothing time constant is ~20ms regardless of sample rate.
pub struct VolumeController {
    current: f64,
    target: f64,
    smoothing: f64,
    one_minus_smoothing: f64,
    sample_rate: u32,
}

impl VolumeController {
    /// Create a new VolumeController with default sample rate (44100 Hz)
    pub fn new() -> Self {
        Self::with_sample_rate(44100)
    }

    /// Create a new VolumeController with specified sample rate
    ///
    /// FIX for Defect 36: Calculate smoothing coefficient based on sample rate
    /// to maintain consistent ~20ms smoothing time.
    pub fn with_sample_rate(sample_rate: u32) -> Self {
        // Target: ~20ms smoothing time
        // smoothing = exp(-1 / tau) where tau = samples for 20ms
        let smoothing_samples = (VOLUME_SMOOTHING_TIME_MS / 1000.0) * sample_rate as f64;
        let smoothing = (-1.0 / smoothing_samples).exp();
        let one_minus_smoothing = 1.0 - smoothing;

        Self {
            current: 1.0,
            target: 1.0,
            smoothing,
            one_minus_smoothing,
            sample_rate,
        }
    }

    /// Update sample rate (recalculates smoothing coefficient)
    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            let smoothing_samples = (VOLUME_SMOOTHING_TIME_MS / 1000.0) * sample_rate as f64;
            self.smoothing = (-1.0 / smoothing_samples).exp();
            self.one_minus_smoothing = 1.0 - self.smoothing;
        }
    }

    pub fn set_target(&mut self, volume: f64) {
        self.target = volume.clamp(0.0, 1.0);
    }

    #[inline(always)]
    pub fn next_volume(&mut self) -> f64 {
        self.current += (self.target - self.current) * self.one_minus_smoothing;
        self.current
    }

    #[inline]
    pub fn process(&mut self, buffer: &mut [f64], channels: usize) {
        let frames = buffer.len() / channels;
        for frame in 0..frames {
            let vol = self.next_volume();
            for ch in 0..channels {
                buffer[frame * channels + ch] *= vol;
            }
        }
    }
}

impl Default for VolumeController {
    fn default() -> Self {
        Self::new()
    }
}

/// Noise shaping curve presets
/// All coefficients from SoX src/dither.c, NTF zeros verified |z| < 1
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NoiseShaperCurve {
    /// Lipshitz 5-tap - general purpose, works well at 44.1/48kHz
    /// NTF max|z| = 0.961, 4kHz notch -27.2dB
    #[default]
    Lipshitz5,

    /// F-weighted 9-tap - psychoacoustically optimized for 44.1kHz
    /// Deepest notch in 2-5kHz region (human hearing most sensitive)
    /// NTF max|z| = 0.914
    FWeighted9,

    /// Modified-E 9-tap - moderate high-frequency push
    /// NTF max|z| = 0.916
    ModifiedE9,

    /// Improved-E 9-tap - most aggressive HF noise shaping
    /// NTF max|z| = 0.959
    ImprovedE9,

    /// Pure TPDF only, no noise shaping
    /// Recommended for 96kHz+ where shaping benefit diminishes
    TpdfOnly,
}

impl NoiseShaperCurve {
    /// Auto-select curve based on sample rate
    /// - 44.1kHz: Lipshitz5 (safe default)
    /// - 48kHz: Lipshitz5 (acceptable 8.8% offset)
    /// - 88.2/96kHz+: TpdfOnly (shaping benefit diminishes)
    pub fn auto_select(sample_rate: u32) -> Self {
        if sample_rate <= 50_000 {
            Self::Lipshitz5
        } else {
            Self::TpdfOnly
        }
    }

    /// Get verified SoX coefficients
    /// Returns 9-element array (lower-order curves pad with zeros)
    pub fn coeffs(&self) -> [f64; 9] {
        match self {
            // SoX lip44 - Lipshitz 1992, 5-tap
            // Verified: NTF zeros all inside unit circle
            Self::Lipshitz5 => [2.033, -2.165, 1.959, -1.590, 0.6149, 0.0, 0.0, 0.0, 0.0],

            // SoX fwe44 - F-weighted, 9-tap
            // Best psychoacoustic performance at 44.1kHz
            Self::FWeighted9 => [
                2.412, -3.370, 3.937, -4.174, 3.353, -2.205, 1.281, -0.569, 0.0847,
            ],

            // SoX mew44 - Modified-E, 9-tap
            Self::ModifiedE9 => [
                1.662, -1.263, 0.4827, -0.2913, 0.1268, -0.1124, 0.03252, -0.01265, -0.03524,
            ],

            // SoX iew44 - Improved-E, 9-tap (most aggressive)
            Self::ImprovedE9 => [
                2.847, -4.685, 6.214, -7.184, 6.639, -5.032, 3.263, -1.632, 0.4191,
            ],

            // Pure TPDF - no noise shaping
            Self::TpdfOnly => [0.0; 9],
        }
    }

    #[inline]
    fn active_taps(&self) -> usize {
        match self {
            Self::Lipshitz5 => 5,
            Self::TpdfOnly => 0,
            Self::FWeighted9 | Self::ModifiedE9 | Self::ImprovedE9 => 9,
        }
    }

    /// Check if this curve is recommended for given sample rate
    /// Unified boundary at 50_000 Hz based on NTF degradation analysis:
    /// - At 48kHz: all curves have ≤6dB 4kHz notch degradation (acceptable)
    /// - At 88.2kHz: only FWeighted9/ImprovedE9 stay ≤6dB, others exceed
    /// - Conservative choice: recommend TpdfOnly for all rates >50kHz
    pub fn is_recommended_for(&self, sample_rate: u32) -> bool {
        match self {
            Self::TpdfOnly => true,     // Always safe
            _ => sample_rate <= 50_000, // Unified boundary
        }
    }
}

/// High-order noise shaping quantizer with SoX-verified coefficients
///
/// Features:
/// - 9-tap error feedback (supports all SoX curves)
/// - Internal xorshift64 RNG (realtime-safe, no thread_rng overhead)
/// - TPDF dither at ±1 LSB (standard amplitude)
/// - Error clamp ±2 LSB (prevents burst noise)
/// - Runtime curve switching with history reset
pub struct NoiseShaper {
    /// Per-channel error history (9 samples each)
    error_history: Vec<[f64; 9]>,
    /// Per-channel duplicated ring history for 9-tap curves.
    error_history_9tap: Vec<[f64; 18]>,
    /// Current head index in the duplicated 9-tap ring per channel.
    error_history_9tap_heads: Vec<usize>,
    /// Current coefficients
    coeffs: [f64; 9],
    /// Number of non-zero feedback taps for the current curve
    active_taps: usize,
    /// Target bit depth
    bits: u32,
    /// Cached 2^(bits-1)
    cached_scale: f64,
    /// Cached reciprocal of `cached_scale`
    cached_lsb: f64,
    /// Enable/disable flag
    enabled: bool,
    /// Current curve preset
    curve: NoiseShaperCurve,
    /// Sample rate for auto-selection
    sample_rate: u32,
    /// Per-channel xorshift64 state for TPDF generation. Each channel owns an
    /// independent, decorrelated stream so L/R dither is not identical (a single
    /// shared stream leaves the channels' dither correlated, which images as a
    /// center-panned noise artifact instead of diffuse noise).
    rng_state: Vec<u64>,
}

/// Base seed for the per-channel TPDF RNG streams.
const NOISE_SHAPER_RNG_SEED: u64 = 0x1234_5678_9ABC_DEF0;

impl NoiseShaper {
    /// Create new NoiseShaper with auto-selected curve
    pub fn new(channels: usize, sample_rate: u32, bits: u32) -> Self {
        let curve = NoiseShaperCurve::auto_select(sample_rate);
        let coeffs = curve.coeffs();
        let bits = bits.clamp(8, 32);
        let (cached_scale, cached_lsb) = Self::scale_for_bits(bits);

        Self {
            error_history: vec![[0.0; 9]; channels],
            error_history_9tap: vec![[0.0; 18]; channels],
            error_history_9tap_heads: vec![0; channels],
            coeffs,
            active_taps: curve.active_taps(),
            bits,
            cached_scale,
            cached_lsb,
            enabled: true,
            curve,
            sample_rate,
            rng_state: (0..channels).map(Self::channel_seed).collect(),
        }
    }

    /// Derive a distinct, non-zero, decorrelated xorshift64 seed for a channel by
    /// running the base seed mixed with the channel index through the splitmix64
    /// finalizer. Independent seeds keep each channel's dither stream uncorrelated.
    fn channel_seed(ch: usize) -> u64 {
        let mut z =
            NOISE_SHAPER_RNG_SEED.wrapping_add((ch as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // xorshift64 requires a non-zero state.
        if z == 0 {
            NOISE_SHAPER_RNG_SEED
        } else {
            z
        }
    }

    /// Enable or disable noise shaping
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Set target bit depth (Defect 37 fix)
    ///
    /// Reachable from the audio thread via `NoiseShaperProcessor::sync_params`, so
    /// this must stay allocation- and log-free.
    pub fn set_bits(&mut self, bits: u32) {
        if bits != self.bits && (8..=32).contains(&bits) {
            self.bits = bits;
            let (cached_scale, cached_lsb) = Self::scale_for_bits(bits);
            self.cached_scale = cached_scale;
            self.cached_lsb = cached_lsb;
        }
    }

    #[inline]
    fn scale_for_bits(bits: u32) -> (f64, f64) {
        let scale = 2.0_f64.powi(bits as i32 - 1);
        (scale, 1.0 / scale)
    }

    /// Get current curve
    pub fn curve(&self) -> NoiseShaperCurve {
        self.curve
    }

    /// Get current sample rate
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get current bit depth
    pub fn bits(&self) -> u32 {
        self.bits
    }

    /// Switch to a different noise shaping curve.
    ///
    /// Clears error history to prevent artifacts from coefficient mismatch, and
    /// respects the caller's explicit choice even when the curve is not recommended
    /// for the current sample rate (callers can query [`NoiseShaperCurve::is_recommended_for`]
    /// beforehand). Reachable from the audio thread via
    /// `NoiseShaperProcessor::sync_params`, so it stays allocation- and log-free.
    pub fn set_curve(&mut self, curve: NoiseShaperCurve) {
        self.curve = curve;
        self.coeffs = curve.coeffs();
        self.active_taps = curve.active_taps();

        // MUST clear history when switching curves
        for h in &mut self.error_history {
            *h = [0.0; 9];
        }
        for h in &mut self.error_history_9tap {
            *h = [0.0; 18];
        }
        self.error_history_9tap_heads.fill(0);
    }

    /// Update sample rate (triggers curve auto-selection)
    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            let new_curve = NoiseShaperCurve::auto_select(sample_rate);
            self.set_curve(new_curve);
        }
    }

    /// xorshift64 PRNG for channel `ch` - fast, deterministic, period 2^64-1
    #[inline(always)]
    fn next_u64(&mut self, ch: usize) -> u64 {
        // Classic xorshift64 parameters (13, 7, 17)
        let s = &mut self.rng_state[ch];
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// Generate TPDF sample for channel `ch`: triangular distribution over (-1, 1)
    /// This gives ±1 LSB amplitude when multiplied by lsb
    /// Standard TPDF: two independent uniform samples subtracted
    #[inline(always)]
    fn tpdf(&mut self, ch: usize) -> f64 {
        // Two independent U(0,1) samples from this channel's stream
        let r1 = self.next_u64(ch) as f64 * INV_U64_MAX;
        let r2 = self.next_u64(ch) as f64 * INV_U64_MAX;
        // Triangular distribution: U(0,1) - U(0,1) = T(-1, 1)
        r1 - r2
    }

    /// Process a single sample with noise shaping and dither
    ///
    /// # Arguments
    /// * `sample` - Input sample in [-1, 1] range
    /// * `ch` - Channel index for error history
    ///
    /// # Returns
    /// * Quantized sample in [-1, 1] range
    #[inline(always)]
    pub fn process_sample(&mut self, sample: f64, ch: usize) -> f64 {
        match self.active_taps {
            0 => self.process_sample_with_taps::<0>(sample, ch),
            5 => self.process_sample_with_taps::<5>(sample, ch),
            _ => self.process_sample_9tap_ring(sample, ch),
        }
    }

    #[inline(always)]
    fn process_sample_with_taps<const TAPS: usize>(&mut self, sample: f64, ch: usize) -> f64 {
        if !self.enabled || ch >= self.error_history.len() {
            return sample;
        }

        // Adaptive dither: skip dither and noise shaping in silence regions
        // Threshold: -120 dBFS (1e-6)
        // Rationale: 24-bit TPDF dither RMS ≈ -146 dBFS, so -120 dBFS is far below
        // perceptible range. This avoids audible dither noise in quiet passages.
        const SILENCE_THRESHOLD: f64 = 1e-6; // -120 dBFS

        if sample.abs() < SILENCE_THRESHOLD {
            // Clear error history to prevent burst noise when audio resumes
            // If we don't do this, accumulated error from silence would suddenly
            // be released when signal returns, causing an audible click
            self.error_history[ch] = [0.0; 9];
            return sample;
        }

        // 1. Generate TPDF dither FIRST (before borrowing error_history)
        //    tpdf() returns (-1, 1) which is ±1 LSB in the integer domain
        //    This is the standard TPDF amplitude for dither
        let dither = self.tpdf(ch);

        // 2. Get error history and compute feedback
        let e = &mut self.error_history[ch];
        let feedback = if TAPS == 0 {
            0.0
        } else if TAPS == 5 {
            self.coeffs[0] * e[0]
                + self.coeffs[1] * e[1]
                + self.coeffs[2] * e[2]
                + self.coeffs[3] * e[3]
                + self.coeffs[4] * e[4]
        } else {
            self.coeffs[0] * e[0]
                + self.coeffs[1] * e[1]
                + self.coeffs[2] * e[2]
                + self.coeffs[3] * e[3]
                + self.coeffs[4] * e[4]
                + self.coeffs[5] * e[5]
                + self.coeffs[6] * e[6]
                + self.coeffs[7] * e[7]
                + self.coeffs[8] * e[8]
        };

        // 3. Quantize
        //    x is in the integer domain (sample * scale shifts to integer range)
        //    dither adds ±1 LSB to prevent quantization distortion
        let x = sample * self.cached_scale + feedback;
        let quantized = (x + dither).round();

        // 4. Update error history with clamp
        //    Clamping prevents error accumulation that could cause burst noise
        //    With ±1 LSB dither, max error is ~1.5 LSB, clamp at ±2 for safety margin
        let raw_error = x - quantized;
        let clamped_error = raw_error.clamp(-2.0, 2.0);

        // Shift only the active feedback window. TPDF-only has no feedback state.
        if TAPS == 5 {
            e[4] = e[3];
            e[3] = e[2];
            e[2] = e[1];
            e[1] = e[0];
            e[0] = clamped_error;
        } else if TAPS == 9 {
            e[8] = e[7];
            e[7] = e[6];
            e[6] = e[5];
            e[5] = e[4];
            e[4] = e[3];
            e[3] = e[2];
            e[2] = e[1];
            e[1] = e[0];
            e[0] = clamped_error;
        }

        quantized * self.cached_lsb
    }

    #[inline(always)]
    fn process_sample_lipshitz5(&mut self, sample: f64, ch: usize) -> f64 {
        self.process_sample_with_taps::<5>(sample, ch)
    }

    #[inline(always)]
    fn process_sample_tpdf_only(&mut self, sample: f64, ch: usize) -> f64 {
        self.process_sample_with_taps::<0>(sample, ch)
    }

    #[inline(always)]
    fn process_sample_9tap(&mut self, sample: f64, ch: usize) -> f64 {
        self.process_sample_9tap_ring(sample, ch)
    }

    #[inline(always)]
    fn process_sample_9tap_ring(&mut self, sample: f64, ch: usize) -> f64 {
        if !self.enabled || ch >= self.error_history_9tap.len() {
            return sample;
        }

        const SILENCE_THRESHOLD: f64 = 1e-6;

        if sample.abs() < SILENCE_THRESHOLD {
            self.error_history_9tap[ch] = [0.0; 18];
            self.error_history_9tap_heads[ch] = 0;
            return sample;
        }

        let dither = self.tpdf(ch);
        let head = self.error_history_9tap_heads[ch];
        let e = &mut self.error_history_9tap[ch];
        let feedback = self.coeffs[0] * e[head]
            + self.coeffs[1] * e[head + 1]
            + self.coeffs[2] * e[head + 2]
            + self.coeffs[3] * e[head + 3]
            + self.coeffs[4] * e[head + 4]
            + self.coeffs[5] * e[head + 5]
            + self.coeffs[6] * e[head + 6]
            + self.coeffs[7] * e[head + 7]
            + self.coeffs[8] * e[head + 8];
        let x = sample * self.cached_scale + feedback;
        let quantized = (x + dither).round();
        let clamped_error = (x - quantized).clamp(-2.0, 2.0);
        let next_head = if head == 0 { 8 } else { head - 1 };
        e[next_head] = clamped_error;
        e[next_head + 9] = clamped_error;
        self.error_history_9tap_heads[ch] = next_head;

        quantized * self.cached_lsb
    }

    /// Process a buffer of samples (convenience method)
    pub fn process(&mut self, buffer: &mut [f64], channels: usize) {
        if !self.enabled {
            return;
        }

        let frames = buffer.len() / channels;
        match self.active_taps {
            0 => {
                for frame in 0..frames {
                    for ch in 0..channels {
                        let idx = frame * channels + ch;
                        buffer[idx] = self.process_sample_tpdf_only(buffer[idx], ch);
                    }
                }
            }
            5 => {
                for frame in 0..frames {
                    for ch in 0..channels {
                        let idx = frame * channels + ch;
                        buffer[idx] = self.process_sample_lipshitz5(buffer[idx], ch);
                    }
                }
            }
            _ => {
                for frame in 0..frames {
                    for ch in 0..channels {
                        let idx = frame * channels + ch;
                        buffer[idx] = self.process_sample_9tap(buffer[idx], ch);
                    }
                }
            }
        }
    }

    /// Reset error history (useful when starting new track)
    pub fn reset(&mut self) {
        for h in &mut self.error_history {
            *h = [0.0; 9];
        }
        for h in &mut self.error_history_9tap {
            *h = [0.0; 18];
        }
        self.error_history_9tap_heads.fill(0);
        // Reset each channel's RNG stream to its seed for reproducibility
        for (ch, state) in self.rng_state.iter_mut().enumerate() {
            *state = Self::channel_seed(ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_history(ns: &NoiseShaper, ch: usize) -> Vec<f64> {
        match ns.active_taps {
            0 => Vec::new(),
            5 => ns.error_history[ch][..5].to_vec(),
            _ => {
                let head = ns.error_history_9tap_heads[ch];
                ns.error_history_9tap[ch][head..head + 9].to_vec()
            }
        }
    }

    fn legacy_process_sample(ns: &mut NoiseShaper, sample: f64, ch: usize) -> f64 {
        if !ns.enabled || ch >= ns.error_history.len() {
            return sample;
        }

        const SILENCE_THRESHOLD: f64 = 1e-6;

        if sample.abs() < SILENCE_THRESHOLD {
            ns.error_history[ch] = [0.0; 9];
            return sample;
        }

        let dither = ns.tpdf(ch);
        let e = &mut ns.error_history[ch];
        let feedback: f64 = ns.coeffs.iter().zip(e.iter()).map(|(c, ei)| c * ei).sum();
        let x = sample * ns.cached_scale + feedback;
        let quantized = (x + dither).round();
        let raw_error = x - quantized;
        let clamped_error = raw_error.clamp(-2.0, 2.0);

        e.copy_within(0..8, 1);
        e[0] = clamped_error;

        quantized * ns.cached_lsb
    }

    #[test]
    fn test_tpdf_distribution() {
        // TPDF should have triangular distribution centered at 0
        let mut ns = NoiseShaper::new(1, 44100, 24);
        let n_samples = 100_000;
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        let mut min = f64::MAX;
        let mut max = f64::MIN;

        for _ in 0..n_samples {
            let t = ns.tpdf(0);
            sum += t;
            sum_sq += t * t;
            min = min.min(t);
            max = max.max(t);
        }

        let mean = sum / n_samples as f64;
        let variance = sum_sq / n_samples as f64 - mean * mean;

        // TPDF (-1, 1): mean ≈ 0, variance = 1/6 ≈ 0.1667
        assert!(mean.abs() < 0.01, "TPDF mean should be ~0, got {}", mean);
        assert!(
            (variance - 1.0 / 6.0).abs() < 0.01,
            "TPDF variance should be ~0.1667, got {}",
            variance
        );
        assert!(
            min > -1.01 && max < 1.01,
            "TPDF range should be (-1, 1), got [{}, {}]",
            min,
            max
        );
    }

    #[test]
    fn test_stability_with_full_scale() {
        // Full-scale square wave should not cause error divergence
        let mut ns = NoiseShaper::new(1, 44100, 24);

        for i in 0..44100 {
            // Alternating full-scale signal (worst case for stability)
            let sample = if i % 2 == 0 { 1.0 } else { -1.0 };
            let out = ns.process_sample(sample, 0);

            // Output should stay in valid range (allow small overshoot from dither)
            assert!(out.abs() <= 1.001, "Output diverged: {}", out);

            // Check error history stays bounded by clamp
            let e = &ns.error_history[0];
            for &ei in e.iter() {
                assert!(ei.abs() <= 2.0, "Error history exceeds clamp: {}", ei);
            }
        }
    }

    #[test]
    fn test_curve_switch_clears_history() {
        let mut ns = NoiseShaper::new(1, 44100, 24);

        // Process some samples to build up error history
        for i in 0..100 {
            ns.process_sample(0.5 * (i as f64 / 100.0).sin(), 0);
        }

        // Verify history is non-zero
        let has_nonzero = ns.error_history[0].iter().any(|&e| e != 0.0);
        assert!(has_nonzero, "Error history should have non-zero values");

        // Switch curve
        ns.set_curve(NoiseShaperCurve::FWeighted9);

        // Verify history is cleared
        for &e in ns.error_history[0].iter() {
            assert_eq!(e, 0.0, "Error history should be cleared after curve switch");
        }
    }

    #[test]
    fn test_curve_switch_clears_9tap_ring_history() {
        let mut ns = NoiseShaper::new(1, 44100, 24);
        ns.set_curve(NoiseShaperCurve::FWeighted9);

        for i in 0..100 {
            ns.process_sample(0.5 * (i as f64 / 100.0).sin(), 0);
        }

        assert!(ns.error_history_9tap[0].iter().any(|&e| e != 0.0));

        ns.set_curve(NoiseShaperCurve::ImprovedE9);

        assert!(ns.error_history_9tap[0].iter().all(|&e| e == 0.0));
        assert_eq!(ns.error_history_9tap_heads[0], 0);
    }

    #[test]
    fn test_idle_tone_free() {
        // With adaptive dither, zero input returns zero output (silence bypass)
        // Test that near-silence (above threshold) produces dithered output
        let mut ns = NoiseShaper::new(1, 44100, 24);
        let n_samples = 44100;
        let mut samples = Vec::with_capacity(n_samples);

        // Use a signal just above the silence threshold
        let above_threshold = 2e-6; // -114 dBFS, above -120 dBFS threshold

        for _ in 0..n_samples {
            samples.push(ns.process_sample(above_threshold, 0));
        }

        // Check 1: Output should be non-zero (dither is working)
        let non_zero_count = samples.iter().filter(|&&x| x != 0.0).count();
        assert!(
            non_zero_count > n_samples / 2,
            "Dither not working: only {}/{} samples non-zero",
            non_zero_count,
            n_samples
        );

        // Check 2: Output should have reasonable variance (dither is adding noise)
        let mean = samples.iter().sum::<f64>() / n_samples as f64;
        let variance = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n_samples as f64;

        // For 24-bit with TPDF dither at ±1 LSB, expect some variance
        let lsb = 1.0 / 2.0_f64.powi(23);
        assert!(
            variance > lsb * lsb * 0.01,
            "Variance too low ({:.2e}), possible idle tone or stuck output",
            variance
        );
    }

    #[test]
    fn test_adaptive_dither_silence() {
        // Test that silence below threshold bypasses dither
        let mut ns = NoiseShaper::new(1, 44100, 24);

        // Zero input should return zero output
        assert_eq!(ns.process_sample(0.0, 0), 0.0);

        // Very low input below threshold should return input unchanged
        let below_threshold = 0.5e-6; // -126 dBFS, below -120 dBFS threshold
        assert_eq!(ns.process_sample(below_threshold, 0), below_threshold);

        // Error history should be cleared after silence
        ns.process_sample(1e-3, 0); // First, build up some error history
        let has_nonzero = ns.error_history[0].iter().any(|&e| e != 0.0);
        assert!(has_nonzero, "Error history should be non-zero after signal");

        // Now feed silence
        ns.process_sample(0.0, 0);

        // Error history should be cleared
        for &e in ns.error_history[0].iter() {
            assert_eq!(e, 0.0, "Error history should be cleared after silence");
        }
    }

    #[test]
    fn test_noise_shaper_unrolled_history_matches_legacy_update() {
        for curve in [
            NoiseShaperCurve::Lipshitz5,
            NoiseShaperCurve::FWeighted9,
            NoiseShaperCurve::ModifiedE9,
            NoiseShaperCurve::ImprovedE9,
            NoiseShaperCurve::TpdfOnly,
        ] {
            let mut optimized = NoiseShaper::new(2, 44100, 16);
            let mut legacy = NoiseShaper::new(2, 44100, 16);
            optimized.set_curve(curve);
            legacy.set_curve(curve);

            for frame in 0..256 {
                for ch in 0..2 {
                    let sample = ((frame * 2 + ch + 1) as f64 * 0.037).sin() * 0.4;
                    let optimized_out = optimized.process_sample(sample, ch);
                    let legacy_out = legacy_process_sample(&mut legacy, sample, ch);

                    assert_eq!(
                        optimized_out.to_bits(),
                        legacy_out.to_bits(),
                        "curve {:?}, frame {}, channel {} output mismatch",
                        curve,
                        frame,
                        ch
                    );
                    assert_eq!(
                        active_history(&optimized, ch),
                        legacy.error_history[ch][..curve.active_taps()].to_vec(),
                        "curve {:?}, frame {}, channel {} active history mismatch",
                        curve,
                        frame,
                        ch
                    );
                }
            }
        }
    }

    #[test]
    fn test_tpdf_only_does_not_update_error_history() {
        let mut ns = NoiseShaper::new(1, 96_000, 24);
        ns.set_curve(NoiseShaperCurve::TpdfOnly);

        for i in 0..128 {
            let sample = ((i + 1) as f64 * 0.031).sin() * 0.5;
            let _ = ns.process_sample(sample, 0);
        }

        assert!(ns.error_history[0].iter().all(|&e| e == 0.0));
    }

    #[test]
    fn test_noise_shaper_silence_reset_is_channel_local() {
        let mut ns = NoiseShaper::new(2, 44100, 24);

        for _ in 0..16 {
            ns.process_sample(0.25, 0);
            ns.process_sample(-0.25, 1);
        }
        assert!(ns.error_history[0].iter().any(|&e| e != 0.0));
        assert!(ns.error_history[1].iter().any(|&e| e != 0.0));

        ns.process_sample(0.0, 0);

        assert!(ns.error_history[0].iter().all(|&e| e == 0.0));
        assert!(ns.error_history[1].iter().any(|&e| e != 0.0));
    }

    #[test]
    fn test_noise_shaper_9tap_ring_silence_reset_is_channel_local() {
        let mut ns = NoiseShaper::new(2, 44100, 24);
        ns.set_curve(NoiseShaperCurve::FWeighted9);

        for _ in 0..16 {
            ns.process_sample(0.25, 0);
            ns.process_sample(-0.25, 1);
        }
        assert!(ns.error_history_9tap[0].iter().any(|&e| e != 0.0));
        assert!(ns.error_history_9tap[1].iter().any(|&e| e != 0.0));

        ns.process_sample(0.0, 0);

        assert!(ns.error_history_9tap[0].iter().all(|&e| e == 0.0));
        assert_eq!(ns.error_history_9tap_heads[0], 0);
        assert!(ns.error_history_9tap[1].iter().any(|&e| e != 0.0));
    }

    #[test]
    fn test_noise_shaper_cached_scale_updates_with_bits() {
        let mut ns = NoiseShaper::new(1, 44100, 24);
        assert_eq!(ns.cached_scale, 2.0_f64.powi(23));
        assert_eq!(ns.cached_lsb, 1.0 / 2.0_f64.powi(23));

        ns.set_bits(16);
        assert_eq!(ns.bits(), 16);
        assert_eq!(ns.cached_scale, 2.0_f64.powi(15));
        assert_eq!(ns.cached_lsb, 1.0 / 2.0_f64.powi(15));

        ns.set_bits(64);
        assert_eq!(ns.bits(), 16);
        assert_eq!(ns.cached_scale, 2.0_f64.powi(15));
        assert_eq!(ns.cached_lsb, 1.0 / 2.0_f64.powi(15));
    }

    #[test]
    fn test_channel_dither_streams_use_independent_seeds() {
        // Same channel is reproducible across fresh instances (deterministic seed).
        let mut a = NoiseShaper::new(2, 44_100, 24);
        let mut b = NoiseShaper::new(2, 44_100, 24);
        assert_eq!(a.tpdf(0).to_bits(), b.tpdf(0).to_bits());

        // Different channels must draw from independently seeded streams. A single
        // shared RNG would make channel 1's first draw equal channel 0's, leaving
        // the two channels' dither correlated.
        let mut c = NoiseShaper::new(2, 44_100, 24);
        let ch0_first = c.tpdf(0);
        let mut d = NoiseShaper::new(2, 44_100, 24);
        let ch1_first = d.tpdf(1);
        assert!(
            (ch0_first - ch1_first).abs() > 1e-12,
            "channel 0 and 1 must use independent seeds (got identical first draws)"
        );
    }

    #[test]
    fn test_volume_controller_one_minus_smoothing_updates_with_sample_rate() {
        let mut volume = VolumeController::with_sample_rate(44_100);
        let initial = volume.one_minus_smoothing;

        volume.set_sample_rate(96_000);

        assert_ne!(volume.one_minus_smoothing, initial);
        assert_eq!(volume.one_minus_smoothing, 1.0 - volume.smoothing);
    }

    #[test]
    fn test_all_curves_stable() {
        // Each curve should process without divergence
        for curve in [
            NoiseShaperCurve::Lipshitz5,
            NoiseShaperCurve::FWeighted9,
            NoiseShaperCurve::ModifiedE9,
            NoiseShaperCurve::ImprovedE9,
            NoiseShaperCurve::TpdfOnly,
        ] {
            let mut ns = NoiseShaper::new(1, 44100, 24);
            ns.set_curve(curve);

            // Process 1 second of full-scale sine wave
            for i in 0..44100 {
                let t = i as f64 / 44100.0;
                let sample = 0.9 * (2.0 * std::f64::consts::PI * 440.0 * t).sin();
                let out = ns.process_sample(sample, 0);
                assert!(out.abs() <= 1.0, "Curve {:?} diverged: {}", curve, out);
            }
        }
    }
}
