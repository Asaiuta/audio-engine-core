//! FFT-based spectrum analyzer for visualization

mod decimation;

use realfft::{num_complex::Complex, RealFftPlanner, RealToComplex};
use std::sync::Arc;

use crate::dsp::modified_bessel_i0;
use crate::processor::traits::{validate_sample_rate_hz, ProcessError};
use decimation::DecimationChain;

const MIN_FREQUENCY_HZ: f64 = 20.0;
const TILT_PIVOT_HZ: f64 = 1_000.0;
const PEAK_DECAY_DB_PER_SECOND: f64 = 20.0;
const MAX_KAISER_BETA: f64 = 50.0;
const MAX_MULTI_RES_SAMPLE_RATE_HZ: u32 = 384_000;
const PUSH_STEP_SAMPLES: usize = 4096;
const DECIMATED_PASSBAND_FRACTION: f64 = 0.8;

/// Window function for spectrum analysis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowFunction {
    /// Hann window (periodic): ENBW 1.50, PSLL −31.5 dB
    Hann,
    /// Blackman-Harris 4-term window: ENBW 2.00, PSLL −92 dB
    BlackmanHarris4,
    /// Kaiser window with a beta parameter in the inclusive range 0..=50.
    ///
    /// Common beta values:
    /// - 6.0: PSLL ≈ -44 dB (good general purpose)
    /// - 8.6: PSLL ≈ -63 dB (high rejection)
    /// - 12.0: PSLL ≈ -90 dB (ultra-high rejection)
    Kaiser {
        /// Shape parameter; larger values trade a wider main lobe for lower sidelobes.
        beta: f64,
    },
}

/// Configuration for [`SpectrumAnalyzer`].
///
/// Controls FFT geometry, output binning, overlap, windowing, visual
/// ballistics, tilt, dB range, and multi-resolution low-frequency analysis.
#[derive(Debug, Clone)]
pub struct SpectrumConfig {
    /// FFT size (must be >= 4). Defaults to 4096 and is ignored when
    /// `multi_resolution` is true.
    pub fft_size: usize,
    /// Number of output bins (must be > 0)
    pub num_bins: usize,
    /// Hop divisor: 1 = no overlap, 2 = 50% overlap, 4 = 75% overlap (default).
    pub hop_divisor: usize,
    /// Window function
    pub window: WindowFunction,
    /// Enable multi-resolution mode (3 tiers scaled by sample rate).
    ///
    /// When enabled, the analyzer uses three FFT tiers (e.g., 4096-point FFTs at
    /// 48, 12, and 3 kHz tier rates for a 48 kHz input) to achieve true frequency
    /// resolution down to 20 Hz. Each output band is computed from the coarsest
    /// tier that provides ≥2 bins of coverage within its usable passband (≤0.8×
    /// tier Nyquist), falling back to a finer usable tier for denser output grids.
    /// This enables distinct representation of low-frequency partials (e.g., 30 Hz
    /// vs 45 Hz) that would otherwise alias to the same bin.
    ///
    /// Low-frequency tiers are fed by decimated copies of the input (÷4 and ÷16
    /// via cascaded half-band filters), so all three tiers use the same FFT size
    /// while preserving the effective window length and frequency resolution.
    /// This reduces memory from ≈3.5 MB to ≈0.5 MB per analyzer at 48 kHz.
    ///
    /// Group delay is content-only: ≈93 input samples for tier B and ≈465 for
    /// tier C at 48 kHz with the default 63-tap decimation filter. Publication
    /// counts and visual ballistics are unchanged.
    ///
    /// Enabled by default. When disabled, uses a single FFT of size `fft_size`
    /// (legacy pooling behavior).
    ///
    /// **Pooling difference**: multi-tier uses sum-of-power (pink noise reads
    /// ≈flat), while single-tier uses mean-of-power to preserve the legacy
    /// oracle arithmetic.
    pub multi_resolution: bool,
    /// Attack time constant in milliseconds for dB-domain smoothing.
    ///
    /// Controls how quickly the spectrum rises when signal energy increases.
    /// Defaults to 30 ms. When `None`, no smoothing is applied.
    /// Typical values: 20-50 ms for responsive visualization, 100-200 ms for
    /// slower averaging.
    pub attack_ms: Option<f64>,
    /// Release time constant in milliseconds for dB-domain smoothing.
    ///
    /// Controls how quickly the spectrum falls when signal energy decreases.
    /// Defaults to 250 ms. When `None`, uses the same value as `attack_ms`. Typically set longer
    /// than attack (e.g., 2-3× attack) for visual stability.
    pub release_ms: Option<f64>,
    /// Peak hold time in milliseconds.
    ///
    /// When set, each band's peak value is held for this duration before
    /// decaying at 20 dB/s. When `None`, no peak hold is applied.
    pub peak_hold_ms: Option<f64>,
    /// Tilt in dB per octave, pivoted at 1 kHz.
    ///
    /// Applied after binning as an optional display-slope compensation.
    /// Common values: 0.0 (flat), 3.0 (gentle), 4.5 (natural/pink compensation).
    pub tilt_db_per_octave: f64,
    /// Minimum dB value for normalization (default -90.0)
    pub db_min: f64,
    /// Maximum dB value for normalization (default 0.0)
    pub db_max: f64,
}

impl Default for SpectrumConfig {
    fn default() -> Self {
        Self {
            fft_size: 4096,
            num_bins: 64,
            hop_divisor: 4, // 75% overlap
            window: WindowFunction::Hann,
            multi_resolution: true,
            attack_ms: Some(30.0),
            release_ms: Some(250.0),
            peak_hold_ms: None,
            tilt_db_per_octave: 0.0,
            db_min: -90.0,
            db_max: 0.0,
        }
    }
}

impl SpectrumConfig {
    /// Create a legacy-compatible configuration.
    ///
    /// Uses no overlap (hop_divisor = 1), Hann window, and single-tier mode
    /// to match the original `new(fft_size, num_bins)` behavior.
    pub fn legacy(fft_size: usize, num_bins: usize) -> Self {
        Self {
            fft_size,
            num_bins,
            hop_divisor: 1, // no overlap for legacy compatibility
            window: WindowFunction::Hann,
            multi_resolution: false,
            attack_ms: None,
            release_ms: None,
            peak_hold_ms: None,
            tilt_db_per_octave: 0.0,
            db_min: -90.0,
            db_max: 0.0,
        }
    }

    /// Set FFT size.
    pub fn with_fft_size(mut self, fft_size: usize) -> Self {
        self.fft_size = fft_size;
        self
    }

    /// Set number of output bins.
    pub fn with_bins(mut self, num_bins: usize) -> Self {
        self.num_bins = num_bins;
        self
    }

    /// Set overlap as a percentage.
    ///
    /// - 0% → hop_divisor = 1 (no overlap)
    /// - 50% → hop_divisor = 2
    /// - 75% → hop_divisor = 4
    ///
    /// Other percentages are clamped to the nearest supported value.
    pub fn with_overlap_percent(mut self, overlap_percent: u32) -> Self {
        self.hop_divisor = if overlap_percent < 25 {
            1
        } else if overlap_percent < 63 {
            2
        } else {
            4
        };
        self
    }

    /// Enable multi-resolution mode for improved low-frequency resolution.
    ///
    /// When enabled, uses 3 FFT tiers (scaled by sample rate) instead of a
    /// single FFT. See `SpectrumConfig::multi_resolution` documentation.
    pub fn with_multi_resolution(mut self, enabled: bool) -> Self {
        self.multi_resolution = enabled;
        self
    }

    /// Set window function.
    pub fn with_window(mut self, window: WindowFunction) -> Self {
        self.window = window;
        self
    }

    /// Set attack time constant in milliseconds.
    ///
    /// Controls how quickly the spectrum rises when signal energy increases.
    /// Pass `None` to disable smoothing; this also clears `release_ms` so a
    /// default configuration remains valid without a separate release call.
    pub fn with_attack_ms(mut self, attack_ms: Option<f64>) -> Self {
        self.attack_ms = attack_ms;
        if attack_ms.is_none() {
            self.release_ms = None;
        }
        self
    }

    /// Set release time constant in milliseconds.
    ///
    /// Controls how quickly the spectrum falls when signal energy decreases.
    /// Pass `None` to use the same value as attack_ms.
    pub fn with_release_ms(mut self, release_ms: Option<f64>) -> Self {
        self.release_ms = release_ms;
        self
    }

    /// Set peak hold time in milliseconds.
    ///
    /// When set, each band's peak value is held for this duration before decaying.
    pub fn with_peak_hold_ms(mut self, peak_hold_ms: Option<f64>) -> Self {
        self.peak_hold_ms = peak_hold_ms;
        self
    }

    /// Set tilt in dB per octave (pivoted at 1 kHz).
    ///
    /// Common values: 0.0 (flat), 3.0 (gentle), 4.5 (natural/pink compensation).
    pub fn with_tilt(mut self, tilt_db_per_octave: f64) -> Self {
        self.tilt_db_per_octave = tilt_db_per_octave;
        self
    }

    /// Set dB range for normalization.
    ///
    /// The output `spectrum()` method normalizes values between db_min and db_max
    /// to the range [0.0, 1.0].
    pub fn with_db_range(mut self, db_min: f64, db_max: f64) -> Self {
        self.db_min = db_min;
        self.db_max = db_max;
        self
    }
}

/// Preallocated state for one FFT resolution tier.
struct Tier {
    fft_size: usize,
    hop_size: usize,
    decimation: usize,
    ring_buffer: Vec<f64>,
    ring_pos: usize,
    filled_samples: usize,
    samples_since_hop: usize,
    fft: Arc<dyn RealToComplex<f64>>,
    window: Vec<f64>,
    fft_input: Vec<f64>,
    fft_spectrum: Vec<Complex<f64>>,
    fft_scratch: Vec<Complex<f64>>,
    magnitudes: Vec<f64>,
    window_power_mean: f64,
    attack_alpha: f64,
    release_alpha: f64,
}

/// FFT-based spectrum analyzer for visualization
///
/// The analyzed signal is real, so this uses `realfft` and reads only its
/// positive-frequency output. Input samples are mono; callers are responsible
/// for stereo downmixing and any cross-thread publication of the borrowed
/// output slices.
///
/// # Streaming input
///
/// The analyzer maintains preallocated ring buffers and computes new spectra
/// at fixed hop intervals. Call [`push`](Self::push) with any block length, then
/// use [`spectrum`](Self::spectrum) or [`spectrum_db`](Self::spectrum_db) to
/// borrow the latest frame. Each tier waits for one complete FFT window before
/// its first update.
///
/// # Multi-resolution mode
///
/// When [`SpectrumConfig::multi_resolution`] is enabled, three independently
/// warmed FFT tiers are stitched by output band. Each band is assigned to the
/// smallest tier that spans at least two FFT bins within its usable passband
/// (≤0.8× tier Nyquist), falling back to the next tier for finer output grids.
///
/// Low-frequency tiers are fed by decimated copies of the input (÷4 and ÷16
/// via cascaded half-band filters). All three tiers use the same 4096-point
/// FFT at 48 kHz, preserving the effective window length and frequency
/// resolution while reducing memory from ≈3.5 MB to ≈0.5 MB per analyzer.
///
/// At 48 kHz with 128 output bands, the three tiers preserve fast high-frequency
/// refresh while resolving low partials such as 30 Hz and 45 Hz into distinct
/// bands. Current host-specific performance evidence is maintained in the crate
/// quality documentation rather than in this API contract.
pub struct SpectrumAnalyzer {
    config: SpectrumConfig,
    sample_rate: u32,
    result_valid: bool,
    result: Vec<f32>,

    /// One tier in single-tier mode, or the three multi-resolution tiers.
    tiers: Vec<Tier>,
    /// Maps each output band index to its tier index
    band_to_tier: Vec<usize>,
    /// Bin ranges aligned with output bands, interpreted in each band's tier.
    multi_band_ranges: Vec<(usize, usize)>,
    /// Decimation chain for multi-resolution mode (None in single-tier mode)
    decimation_chain: Option<DecimationChain>,
    /// Input sample counter for multi-resolution event stepping
    input_consumed: u64,
    /// Scratch buffers for decimated tier feeding (sized for PUSH_STEP_SAMPLES)
    tier_b_scratch: Vec<f64>,
    tier_c_scratch: Vec<f64>,

    /// Smoothing state: previous dB values per band (None when smoothing disabled)
    prev_db: Option<Vec<f64>>,
    /// Peak-hold state, allocated only when peak hold is enabled.
    peak_db: Option<Vec<f64>>,
    peak_hold_remaining_samples: Option<Vec<usize>>,
    peak_hold_samples: usize,
    /// Clamped dB values before normalization (for spectrum_db accessor)
    result_db: Vec<f32>,

    /// Single-tier bin ranges; empty in multi-resolution mode.
    bin_ranges: Vec<(usize, usize)>,
    /// Cached sample rate for single-tier bin ranges.
    bin_sample_rate: Option<u32>,
}

impl SpectrumAnalyzer {
    /// Calculate tier sizes scaled by sample rate.
    ///
    /// At 48 kHz: [4096, 16384, 65536]
    /// At 96 kHz: [8192, 32768, 131072]
    fn calculate_tier_sizes(sample_rate: u32) -> [usize; 3] {
        let scaled_base = (4096_u64 * u64::from(sample_rate)).div_ceil(48_000) as usize;
        let base = scaled_base.max(4).next_power_of_two();
        [base, base * 4, base * 16]
    }

    fn smoothing_alphas(
        config: &SpectrumConfig,
        sample_rate: u32,
        input_samples_per_hop: usize,
    ) -> (f64, f64) {
        if let Some(attack_ms) = config.attack_ms {
            let attack_coeff = Self::smoothing_alpha(attack_ms, sample_rate, input_samples_per_hop);
            let release_coeff = config.release_ms.map_or(attack_coeff, |release_ms| {
                Self::smoothing_alpha(release_ms, sample_rate, input_samples_per_hop)
            });
            (attack_coeff, release_coeff)
        } else {
            (0.0, 0.0)
        }
    }

    fn smoothing_alpha(time_ms: f64, sample_rate: u32, input_samples_per_hop: usize) -> f64 {
        1.0 - (-(input_samples_per_hop as f64) * 1_000.0 / (sample_rate as f64 * time_ms)).exp()
    }

    /// Generate Blackman-Harris 4-term window.
    fn blackman_harris_4(n: usize) -> Vec<f64> {
        const A0: f64 = 0.35875;
        const A1: f64 = 0.48829;
        const A2: f64 = 0.14128;
        const A3: f64 = 0.01168;

        (0..n)
            .map(|i| {
                let x = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
                A0 - A1 * x.cos() + A2 * (2.0 * x).cos() - A3 * (3.0 * x).cos()
            })
            .collect()
    }

    /// Generate Kaiser window with parameter beta.
    fn kaiser_window(n: usize, beta: f64) -> Vec<f64> {
        let i0_beta = modified_bessel_i0(beta);
        (0..n)
            .map(|i| {
                let x = 2.0 * i as f64 / (n - 1) as f64 - 1.0; // -1 to 1
                let arg = beta * (1.0 - x * x).sqrt();
                modified_bessel_i0(arg) / i0_beta
            })
            .collect()
    }

    fn window_values(window: WindowFunction, fft_size: usize) -> Vec<f64> {
        match window {
            WindowFunction::Hann => (0..fft_size)
                .map(|i| {
                    0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / fft_size as f64).cos())
                })
                .collect(),
            WindowFunction::BlackmanHarris4 => Self::blackman_harris_4(fft_size),
            WindowFunction::Kaiser { beta } => Self::kaiser_window(fft_size, beta),
        }
    }

    fn build_tier(
        planner: &mut RealFftPlanner<f64>,
        config: &SpectrumConfig,
        sample_rate: u32,
        fft_size: usize,
        decimation: usize,
    ) -> Tier {
        let hop_size = fft_size / config.hop_divisor;
        let fft = planner.plan_fft_forward(fft_size);
        let fft_scratch_len = fft.get_scratch_len();
        let window = Self::window_values(config.window, fft_size);
        let window_power_mean =
            window.iter().map(|value| value * value).sum::<f64>() / fft_size as f64;
        let input_samples_per_hop = hop_size * decimation;
        let (attack_alpha, release_alpha) =
            Self::smoothing_alphas(config, sample_rate, input_samples_per_hop);

        Tier {
            fft_size,
            hop_size,
            decimation,
            ring_buffer: vec![0.0; fft_size],
            ring_pos: 0,
            filled_samples: 0,
            samples_since_hop: 0,
            fft,
            window,
            fft_input: vec![0.0; fft_size],
            fft_spectrum: vec![Complex::new(0.0, 0.0); fft_size / 2 + 1],
            fft_scratch: vec![Complex::new(0.0, 0.0); fft_scratch_len],
            magnitudes: vec![0.0; fft_size.saturating_div(2).saturating_sub(1)],
            window_power_mean,
            attack_alpha,
            release_alpha,
        }
    }

    fn invalid_parameter(parameter: &'static str, message: &'static str) -> ProcessError {
        ProcessError::InvalidParameter {
            processor: "SpectrumAnalyzer",
            parameter,
            message,
        }
    }

    fn validate_config(config: &SpectrumConfig) -> Result<(), ProcessError> {
        if config.num_bins == 0 {
            return Err(ProcessError::InvalidGeometry {
                processor: "SpectrumAnalyzer",
                operation: "create analyzer",
                message: "output bin count must be greater than zero",
            });
        }
        if config.hop_divisor == 0 {
            return Err(ProcessError::InvalidGeometry {
                processor: "SpectrumAnalyzer",
                operation: "create analyzer",
                message: "hop divisor must be greater than zero",
            });
        }
        if !config.multi_resolution && config.fft_size < 4 {
            return Err(ProcessError::InvalidGeometry {
                processor: "SpectrumAnalyzer",
                operation: "create analyzer",
                message: "FFT size must provide at least one non-DC/non-Nyquist bin",
            });
        }

        for (parameter, value) in [
            ("attack_ms", config.attack_ms),
            ("release_ms", config.release_ms),
            ("peak_hold_ms", config.peak_hold_ms),
        ] {
            if value.is_some_and(|value| !value.is_finite() || value <= 0.0) {
                return Err(Self::invalid_parameter(
                    parameter,
                    "value must be finite and greater than zero",
                ));
            }
        }
        if config.attack_ms.is_none() && config.release_ms.is_some() {
            return Err(Self::invalid_parameter(
                "release_ms",
                "release smoothing requires attack_ms",
            ));
        }
        if !config.tilt_db_per_octave.is_finite() {
            return Err(Self::invalid_parameter(
                "tilt_db_per_octave",
                "value must be finite",
            ));
        }
        if !config.db_min.is_finite() || !config.db_max.is_finite() {
            return Err(Self::invalid_parameter("db_range", "bounds must be finite"));
        }
        if config.db_min >= config.db_max {
            return Err(Self::invalid_parameter(
                "db_range",
                "minimum must be less than maximum",
            ));
        }
        if let WindowFunction::Kaiser { beta } = config.window {
            if !beta.is_finite() || !(0.0..=MAX_KAISER_BETA).contains(&beta) {
                return Err(Self::invalid_parameter(
                    "window.beta",
                    "Kaiser beta must be finite and in the range 0..=50",
                ));
            }
        }
        Ok(())
    }

    /// Create an analyzer with the given configuration and sample rate.
    ///
    /// All buffers are allocated during construction; [`Self::push`],
    /// [`Self::spectrum`], and [`Self::spectrum_db`] perform no allocation.
    ///
    /// Geometry failures use [`ProcessError::InvalidGeometry`]. Non-finite,
    /// non-positive, or inconsistent visual parameters use
    /// [`ProcessError::InvalidParameter`]. Multi-resolution input supports
    /// sample rates through 384 kHz and requires Nyquist to exceed 20 Hz.
    pub fn with_config(config: SpectrumConfig, sample_rate: u32) -> Result<Self, ProcessError> {
        Self::validate_config(&config)?;
        validate_sample_rate_hz("SpectrumAnalyzer", sample_rate)?;
        if sample_rate <= (MIN_FREQUENCY_HZ * 2.0) as u32
            || (config.multi_resolution && sample_rate > MAX_MULTI_RES_SAMPLE_RATE_HZ)
        {
            return Err(ProcessError::InvalidSampleRate {
                processor: "SpectrumAnalyzer",
                sample_rate_hz: sample_rate,
            });
        }

        // Branch: multi-resolution or single-tier mode
        if config.multi_resolution {
            Self::with_config_multi_res(config, sample_rate)
        } else {
            Self::with_config_single_tier(config, sample_rate)
        }
    }

    /// Create a single-tier analyzer (legacy behavior).
    fn with_config_single_tier(
        config: SpectrumConfig,
        sample_rate: u32,
    ) -> Result<Self, ProcessError> {
        let hop_size = config.fft_size / config.hop_divisor;
        if hop_size == 0 {
            return Err(ProcessError::InvalidGeometry {
                processor: "SpectrumAnalyzer",
                operation: "create analyzer",
                message: "hop size must be greater than zero",
            });
        }

        let mut planner = RealFftPlanner::<f64>::new();
        let tier = Self::build_tier(&mut planner, &config, sample_rate, config.fft_size, 1);
        let result = vec![0.0; config.num_bins];
        let result_db = vec![config.db_min as f32; config.num_bins];

        let prev_db = config
            .attack_ms
            .map(|_| vec![config.db_min; config.num_bins]);

        let peak_hold_samples = config
            .peak_hold_ms
            .map_or(0, |ms| (ms * sample_rate as f64 / 1_000.0).ceil() as usize);
        let peak_db = config
            .peak_hold_ms
            .map(|_| vec![config.db_min; config.num_bins]);
        let peak_hold_remaining_samples = config.peak_hold_ms.map(|_| vec![0; config.num_bins]);

        let mut analyzer = Self {
            config: config.clone(),
            sample_rate,
            result_valid: false,
            result,

            tiers: vec![tier],
            band_to_tier: Vec::new(),
            multi_band_ranges: Vec::new(),
            decimation_chain: None,
            input_consumed: 0,
            tier_b_scratch: Vec::new(),
            tier_c_scratch: Vec::new(),

            prev_db,
            peak_db,
            peak_hold_remaining_samples,
            peak_hold_samples,
            result_db,

            bin_ranges: Vec::with_capacity(config.num_bins),
            bin_sample_rate: None,
        };

        // Precompute bin ranges for the given sample rate
        analyzer.ensure_bin_ranges(sample_rate);

        Ok(analyzer)
    }

    /// Create a multi-resolution analyzer with 3 tiers.
    fn with_config_multi_res(
        config: SpectrumConfig,
        sample_rate: u32,
    ) -> Result<Self, ProcessError> {
        let tier_sizes = Self::calculate_tier_sizes(sample_rate);
        if config.hop_divisor > tier_sizes[0] {
            return Err(ProcessError::InvalidGeometry {
                processor: "SpectrumAnalyzer",
                operation: "create analyzer",
                message: "hop size must be greater than zero",
            });
        }
        let result = vec![0.0; config.num_bins];

        let mut tiers = Vec::with_capacity(3);
        let mut planner = RealFftPlanner::<f64>::new();

        // All three tiers use the same FFT size (base), but different decimation factors
        let decimations = [1, 4, 16];
        for &decimation in &decimations {
            tiers.push(Self::build_tier(
                &mut planner,
                &config,
                sample_rate,
                tier_sizes[0],
                decimation,
            ));
        }

        // Create decimation chain with 63-tap Kaiser beta=9.6 kernel
        let decimation_chain = Some(DecimationChain::new(PUSH_STEP_SAMPLES));

        // Assign bands to tiers and precompute bin ranges
        let (band_to_tier, multi_band_ranges) =
            Self::assign_bands_to_tiers(config.num_bins, sample_rate, &tier_sizes);

        let result_db = vec![config.db_min as f32; config.num_bins];

        let prev_db = config
            .attack_ms
            .map(|_| vec![config.db_min; config.num_bins]);
        let peak_hold_samples = config
            .peak_hold_ms
            .map_or(0, |ms| (ms * sample_rate as f64 / 1_000.0).ceil() as usize);
        let peak_db = config
            .peak_hold_ms
            .map(|_| vec![config.db_min; config.num_bins]);
        let peak_hold_remaining_samples = config.peak_hold_ms.map(|_| vec![0; config.num_bins]);

        // Create analyzer with multi-res fields populated
        let analyzer = Self {
            config: config.clone(),
            sample_rate,
            result_valid: false,
            result,

            // Multi-resolution fields
            tiers,
            band_to_tier,
            multi_band_ranges,
            decimation_chain,
            input_consumed: 0,
            tier_b_scratch: vec![0.0; PUSH_STEP_SAMPLES / 4 + 1],
            tier_c_scratch: vec![0.0; PUSH_STEP_SAMPLES / 16 + 1],

            prev_db,
            peak_db,
            peak_hold_remaining_samples,
            peak_hold_samples,
            result_db,

            bin_ranges: Vec::new(),
            bin_sample_rate: None,
        };

        Ok(analyzer)
    }

    /// Assign each output band to a tier based on 2-bin margin rule with passband guard.
    ///
    /// Returns `(band_to_tier, band_ranges)`:
    /// - band_to_tier[i] = tier index for output band i
    /// - band_ranges[i] = (idx_low, idx_high) in that band's assigned tier
    ///
    /// A decimated tier is a candidate only if the band's upper edge is ≤ 0.8× that
    /// tier's Nyquist (usable passband guard).
    fn assign_bands_to_tiers(
        num_bins: usize,
        sample_rate: u32,
        tier_sizes: &[usize; 3],
    ) -> (Vec<usize>, Vec<(usize, usize)>) {
        let nyquist = sample_rate as f64 / 2.0;
        let min_freq = MIN_FREQUENCY_HZ;
        let max_freq = nyquist;
        let log_min = min_freq.log10();
        let log_max = max_freq.log10();

        // Decimation factors for the three tiers
        let decimations = [1, 4, 16];

        let mut band_to_tier = vec![0; num_bins];
        let mut band_ranges = vec![(0, 0); num_bins];

        for (bin_idx, band_tier) in band_to_tier.iter_mut().enumerate().take(num_bins) {
            let freq_low =
                10.0_f64.powf(log_min + (log_max - log_min) * bin_idx as f64 / num_bins as f64);
            let freq_high = 10.0_f64
                .powf(log_min + (log_max - log_min) * (bin_idx + 1) as f64 / num_bins as f64);
            let band_width = freq_high - freq_low;

            // Find candidates: tiers whose usable passband contains freq_high
            let mut candidates = Vec::new();
            for (tier_idx, &decimation) in decimations.iter().enumerate() {
                let tier_nyquist = nyquist / decimation as f64;
                let usable_hz = DECIMATED_PASSBAND_FRACTION * tier_nyquist;
                if freq_high <= usable_hz {
                    candidates.push(tier_idx);
                }
            }

            // Tier A (decimation=1) is always a candidate
            if candidates.is_empty() {
                candidates.push(0);
            }

            // Among candidates, find the first where band spans >= 2 bins
            let mut assigned_tier = *candidates.last().unwrap(); // default to last candidate
            for &tier_idx in &candidates {
                let decimation = decimations[tier_idx];
                let df_tier = nyquist / (tier_sizes[0] / 2) as f64 / decimation as f64;
                if band_width >= 2.0 * df_tier {
                    assigned_tier = tier_idx;
                    break;
                }
            }

            *band_tier = assigned_tier;

            // Compute bin range in the assigned tier's magnitude array
            // All tiers use tier_sizes[0], but effective rate is sample_rate / decimation
            let decimation = decimations[assigned_tier];
            let tier_size = tier_sizes[0];
            let positive_bin_count = tier_size / 2 - 1;
            let freq_per_bin = (sample_rate as f64 / decimation as f64) / tier_size as f64;
            let fft_bin_low = (freq_low / freq_per_bin).floor() as usize;
            let fft_bin_high = (freq_high / freq_per_bin).ceil() as usize;
            let idx_low = fft_bin_low
                .max(1)
                .saturating_sub(1)
                .min(positive_bin_count.saturating_sub(1));
            let idx_high = fft_bin_high
                .saturating_sub(1)
                .clamp(idx_low + 1, positive_bin_count);

            band_ranges[bin_idx] = (idx_low, idx_high);
        }

        (band_to_tier, band_ranges)
    }

    /// Create an analyzer with the given FFT and output bin geometry.
    ///
    /// **Deprecated:** Use `with_config(SpectrumConfig::legacy(fft_size, num_bins), 48_000)`
    /// for equivalent behavior, or `with_config(SpectrumConfig::default(), sample_rate)` for
    /// streaming input with configurable overlap.
    ///
    /// Rejects `fft_size < 4` and `num_bins == 0` before planning any FFT.
    #[deprecated(
        since = "1.2.0",
        note = "Use with_config() for streaming input and configurable overlap"
    )]
    pub fn new(fft_size: usize, num_bins: usize) -> Result<Self, ProcessError> {
        Self::with_config(SpectrumConfig::legacy(fft_size, num_bins), 48_000)
    }

    /// Push samples into the analyzer.
    ///
    /// Accepts any block length. No frame is published until a complete FFT
    /// window has been received. Thereafter this returns the number of input
    /// positions at which at least one tier published an update. Simultaneous
    /// tier updates count as one frame.
    ///
    /// `samples` contains mono audio. Downmix interleaved or planar channel
    /// data before calling this method.
    ///
    /// After pushing, call `spectrum()` to read the most recent frame.
    pub fn push(&mut self, samples: &[f64]) -> usize {
        self.push_blockwise(samples)
    }

    /// Append one bounded block to a tier ring using at most two bulk copies.
    fn write_tier_samples(tier: &mut Tier, samples: &[f64]) {
        if samples.is_empty() {
            return;
        }
        debug_assert!(samples.len() <= tier.fft_size);

        let first_len = samples.len().min(tier.fft_size - tier.ring_pos);
        tier.ring_buffer[tier.ring_pos..tier.ring_pos + first_len]
            .copy_from_slice(&samples[..first_len]);
        let second_len = samples.len() - first_len;
        if second_len > 0 {
            tier.ring_buffer[..second_len].copy_from_slice(&samples[first_len..]);
            tier.ring_pos = second_len;
        } else {
            tier.ring_pos += first_len;
            if tier.ring_pos == tier.fft_size {
                tier.ring_pos = 0;
            }
        }
    }

    /// Advance a tier by one event-bounded block and report whether it fired.
    fn advance_tier(tier: &mut Tier, sample_count: usize) -> bool {
        if sample_count == 0 {
            return false;
        }

        if tier.filled_samples < tier.fft_size {
            debug_assert!(sample_count <= tier.fft_size - tier.filled_samples);
            tier.filled_samples += sample_count;
            tier.filled_samples == tier.fft_size
        } else {
            debug_assert!(sample_count <= tier.hop_size - tier.samples_since_hop);
            tier.samples_since_hop += sample_count;
            if tier.samples_since_hop == tier.hop_size {
                tier.samples_since_hop = 0;
                true
            } else {
                false
            }
        }
    }

    /// Push either analyzer mode through the same event-stepped block loop.
    fn push_blockwise(&mut self, samples: &[f64]) -> usize {
        let mut published = 0;
        let mut remaining = samples;

        while !remaining.is_empty() {
            // Calculate input distance to the next event for each tier.  The
            // running counter accounts for a decimator whose next output can
            // begin in the middle of the caller's block.
            let mut step = remaining.len().min(PUSH_STEP_SAMPLES);
            for tier in &self.tiers {
                let tier_samples_until_event = if tier.filled_samples < tier.fft_size {
                    tier.fft_size - tier.filled_samples
                } else {
                    tier.hop_size - tier.samples_since_hop
                };
                let phase = (self.input_consumed % tier.decimation as u64) as usize;
                let input_until_event = tier_samples_until_event
                    .saturating_mul(tier.decimation)
                    .saturating_sub(phase)
                    .max(1);
                step = step.min(input_until_event);
            }
            // Keeping the step no larger than tier A's ring makes every ring
            // append a single bounded operation (at most two copies), even
            // for the small FFT sizes used by focused tests.
            step = step.min(self.tiers[0].fft_size).max(1);
            let input_chunk = &remaining[..step];

            let mut tier_sample_counts = [step, 0, 0];
            Self::write_tier_samples(&mut self.tiers[0], input_chunk);

            if let Some(chain) = &mut self.decimation_chain {
                let (b_count, c_count) = chain.process(
                    input_chunk,
                    &mut self.tier_b_scratch,
                    &mut self.tier_c_scratch,
                );
                tier_sample_counts[1] = b_count;
                tier_sample_counts[2] = c_count;
                Self::write_tier_samples(&mut self.tiers[1], &self.tier_b_scratch[..b_count]);
                Self::write_tier_samples(&mut self.tiers[2], &self.tier_c_scratch[..c_count]);
            }

            let mut ready = [false; 3];
            for tier_idx in 0..self.tiers.len() {
                ready[tier_idx] =
                    Self::advance_tier(&mut self.tiers[tier_idx], tier_sample_counts[tier_idx]);
            }

            if self.config.multi_resolution {
                for (tier_idx, &tier_ready) in ready.iter().enumerate().take(self.tiers.len()) {
                    if tier_ready {
                        self.compute_spectrum_tier(tier_idx);
                    }
                }
            } else if ready[0] {
                self.compute_spectrum_single_tier();
            }

            if ready[..self.tiers.len()]
                .iter()
                .any(|&tier_ready| tier_ready)
            {
                self.result_valid = true;
                published += 1;
            }

            self.input_consumed += step as u64;
            remaining = &remaining[step..];
        }

        published
    }

    /// Return the most recent spectrum, or `None` if no spectrum has been computed yet.
    ///
    /// Values are normalized to 0..1 based on the configured dB range.
    pub fn spectrum(&self) -> Option<&[f32]> {
        if self.result_valid {
            Some(&self.result)
        } else {
            None
        }
    }

    /// Return the most recent spectrum in dB, or `None` if no spectrum has been computed yet.
    ///
    /// Values are clamped to the configured dB range before normalization.
    pub fn spectrum_db(&self) -> Option<&[f32]> {
        if self.result_valid {
            Some(&self.result_db)
        } else {
            None
        }
    }

    /// Return the current sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Return the current configuration.
    pub fn config(&self) -> &SpectrumConfig {
        &self.config
    }

    fn reset_streaming_state(&mut self) {
        self.result.fill(0.0);
        self.result_db.fill(self.config.db_min as f32);
        self.result_valid = false;
        if let Some(prev) = &mut self.prev_db {
            prev.fill(self.config.db_min);
        }
        if let Some(peaks) = &mut self.peak_db {
            peaks.fill(self.config.db_min);
        }
        if let Some(remaining) = &mut self.peak_hold_remaining_samples {
            remaining.fill(0);
        }

        for tier in &mut self.tiers {
            tier.ring_pos = 0;
            tier.filled_samples = 0;
            tier.samples_since_hop = 0;
        }

        if let Some(chain) = &mut self.decimation_chain {
            chain.reset();
        }
        self.input_consumed = 0;
    }

    /// Compute band center frequency in Hz for the given band index.
    fn compute_band_center_hz(&self, band_idx: usize) -> f64 {
        let min_freq = MIN_FREQUENCY_HZ;
        let max_freq = self.sample_rate as f64 / 2.0;
        let log_min = min_freq.log10();
        let log_max = max_freq.log10();
        let num_bins = self.config.num_bins;

        // Band edges
        let band_frac = band_idx as f64 / num_bins as f64;
        let f_low = 10.0_f64.powf(log_min + (log_max - log_min) * band_frac);

        let band_frac_high = (band_idx + 1) as f64 / num_bins as f64;
        let f_high = 10.0_f64.powf(log_min + (log_max - log_min) * band_frac_high);

        // Geometric mean
        (f_low * f_high).sqrt()
    }

    fn tilt_db_for_band(&self, band_idx: usize) -> f64 {
        if self.config.tilt_db_per_octave == 0.0 {
            return 0.0;
        }
        let band_center_hz = self.compute_band_center_hz(band_idx);
        self.config.tilt_db_per_octave * (band_center_hz / TILT_PIVOT_HZ).log2()
    }

    fn apply_level_processing(
        &mut self,
        band_idx: usize,
        mut db: f64,
        attack_alpha: f64,
        release_alpha: f64,
        elapsed_samples: usize,
    ) -> f64 {
        db = db.clamp(self.config.db_min, self.config.db_max);

        if let Some(prev) = &mut self.prev_db {
            let alpha = if db > prev[band_idx] {
                attack_alpha
            } else {
                release_alpha
            };
            db = prev[band_idx] + alpha * (db - prev[band_idx]);
            prev[band_idx] = db;
        }

        if let (Some(peaks), Some(remaining)) =
            (&mut self.peak_db, &mut self.peak_hold_remaining_samples)
        {
            if db >= peaks[band_idx] {
                peaks[band_idx] = db;
                remaining[band_idx] = self.peak_hold_samples;
            } else {
                let held_samples = remaining[band_idx].min(elapsed_samples);
                remaining[band_idx] -= held_samples;
                let decay_samples = elapsed_samples - held_samples;
                if decay_samples > 0 {
                    peaks[band_idx] -=
                        PEAK_DECAY_DB_PER_SECOND * decay_samples as f64 / self.sample_rate as f64;
                    if peaks[band_idx] <= db {
                        peaks[band_idx] = db;
                        remaining[band_idx] = self.peak_hold_samples;
                    }
                }
            }
            db = peaks[band_idx];
        }

        db.clamp(self.config.db_min, self.config.db_max)
    }

    /// Compute a spectrum from the current ring buffer contents (single-tier mode).
    ///
    /// Copies samples in chronological order, applies the window, runs the FFT,
    /// and bins the result.
    fn compute_spectrum_single_tier(&mut self) {
        {
            let tier = &mut self.tiers[0];

            // ring_pos points to the next write location, which is the oldest sample.
            let split_point = tier.ring_pos;
            let older_len = tier.fft_size - split_point;
            tier.fft_input[..older_len].copy_from_slice(&tier.ring_buffer[split_point..]);
            tier.fft_input[older_len..].copy_from_slice(&tier.ring_buffer[..split_point]);

            for (slot, &window) in tier.fft_input.iter_mut().zip(&tier.window) {
                *slot *= window;
            }

            debug_assert_eq!(tier.fft_input.len(), tier.fft_size);
            debug_assert_eq!(tier.fft_spectrum.len(), tier.fft.complex_len());
            let _ = tier.fft.process_with_scratch(
                &mut tier.fft_input,
                &mut tier.fft_spectrum,
                &mut tier.fft_scratch,
            );

            // Preserve the legacy single-tier magnitude arithmetic and its
            // `1..N/2` bin selection. For odd N this intentionally excludes
            // the highest positive-frequency bin, matching the original
            // complex-FFT implementation.
            for (dst, c) in tier
                .magnitudes
                .iter_mut()
                .zip(tier.fft_spectrum[1..tier.fft_size / 2].iter())
            {
                *dst = c.norm() / tier.fft_size as f64;
            }
        }

        self.log_bin_single_tier();
        self.result_valid = true;
    }

    /// Compute a spectrum from a specific tier (multi-resolution mode).
    fn compute_spectrum_tier(&mut self, tier_idx: usize) {
        let tier = &mut self.tiers[tier_idx];

        // Copy from ring buffer in chronological order
        let split_point = tier.ring_pos;
        let older_len = tier.fft_size - split_point;

        tier.fft_input[..older_len].copy_from_slice(&tier.ring_buffer[split_point..]);
        tier.fft_input[older_len..].copy_from_slice(&tier.ring_buffer[..split_point]);

        // Apply window
        for (slot, &window) in tier.fft_input.iter_mut().zip(&tier.window) {
            *slot *= window;
        }

        // Run FFT
        debug_assert_eq!(tier.fft_input.len(), tier.fft_size);
        let _ = tier.fft.process_with_scratch(
            &mut tier.fft_input,
            &mut tier.fft_spectrum,
            &mut tier.fft_scratch,
        );

        // Extract power spectrum: |X|² / N² (not magnitude)
        // Convert before multiplying so supported large tiers remain correct
        // on 32-bit targets, where `usize * usize` could overflow first.
        let fft_size = tier.fft_size as f64;
        let n_squared = fft_size * fft_size;
        for (dst, c) in tier
            .magnitudes
            .iter_mut()
            .zip(tier.fft_spectrum[1..tier.fft_size / 2].iter())
        {
            *dst = c.norm_sqr() / n_squared;
        }

        let attack_alpha = tier.attack_alpha;
        let release_alpha = tier.release_alpha;
        let input_samples_per_hop = tier.hop_size * tier.decimation;

        // Only publish the bands owned by this tier. Other tiers retain their
        // latest independently timestamped values until their next update.
        self.log_bin_multi_res_tier(tier_idx, attack_alpha, release_alpha, input_samples_per_hop);
    }

    /// Bin into log-spaced output bands (single-tier mode, mean-of-power pooling).
    fn log_bin_single_tier(&mut self) {
        self.result_db.fill(self.config.db_min as f32);
        let attack_alpha = self.tiers[0].attack_alpha;
        let release_alpha = self.tiers[0].release_alpha;
        let elapsed_samples = self.tiers[0].hop_size;

        for band_idx in 0..self.bin_ranges.len() {
            let (idx_low, idx_high) = self.bin_ranges[band_idx];
            if idx_high > idx_low {
                let sum: f64 = self.tiers[0].magnitudes[idx_low..idx_high]
                    .iter()
                    .map(|m| m * m)
                    .sum();
                let rms = (sum / (idx_high - idx_low) as f64).sqrt();
                let mut db = 20.0 * (rms + 1e-9).log10();

                // Apply tilt
                db += self.tilt_db_for_band(band_idx);

                db = self.apply_level_processing(
                    band_idx,
                    db,
                    attack_alpha,
                    release_alpha,
                    elapsed_samples,
                );

                self.result_db[band_idx] = db as f32;

                // Normalize to 0..1
                let normalized =
                    (db - self.config.db_min) / (self.config.db_max - self.config.db_min);
                self.result[band_idx] = normalized.clamp(0.0, 1.0) as f32;
            }
        }
    }

    /// Bin into log-spaced output bands (multi-resolution mode, sum-of-power pooling).
    fn log_bin_multi_res_tier(
        &mut self,
        updated_tier_idx: usize,
        attack_alpha: f64,
        release_alpha: f64,
        input_samples_per_hop: usize,
    ) {
        for band_idx in 0..self.result.len() {
            let tier_idx = self.band_to_tier[band_idx];
            if tier_idx != updated_tier_idx {
                continue;
            }

            let (idx_low, idx_high) = self.multi_band_ranges[band_idx];
            if idx_high > idx_low {
                let tier = &self.tiers[tier_idx];
                let sum_power: f64 = tier.magnitudes[idx_low..idx_high].iter().sum();
                let normalized_power = sum_power / tier.window_power_mean;
                let mut db = 10.0 * (normalized_power + 1e-18).log10();

                db += self.tilt_db_for_band(band_idx);

                db = self.apply_level_processing(
                    band_idx,
                    db,
                    attack_alpha,
                    release_alpha,
                    input_samples_per_hop,
                );
                self.result_db[band_idx] = db as f32;
                self.result[band_idx] = ((db - self.config.db_min)
                    / (self.config.db_max - self.config.db_min))
                    .clamp(0.0, 1.0) as f32;
            }
        }
    }

    /// Analyze a block of samples and return the binned magnitudes.
    ///
    /// **Deprecated:** Use `push()` and `spectrum()` for streaming input.
    /// This method resets the internal ring buffer on each call, so it cannot
    /// be mixed with `push()`.
    ///
    /// Rejects an invalid sample rate before touching cached state. A
    /// single-tier block shorter than the FFT size produces a zero-filled
    /// result. Multi-resolution mode requires the construction sample rate and
    /// consumes the complete block, warming each tier independently.
    #[deprecated(
        since = "1.2.0",
        note = "Use push() and spectrum() for streaming input; analyze() resets state on each call"
    )]
    pub fn analyze(&mut self, samples: &[f64], sample_rate: u32) -> Result<&[f32], ProcessError> {
        validate_sample_rate_hz("SpectrumAnalyzer", sample_rate)?;
        if sample_rate <= (MIN_FREQUENCY_HZ * 2.0) as u32
            || (self.config.multi_resolution && sample_rate > MAX_MULTI_RES_SAMPLE_RATE_HZ)
        {
            return Err(ProcessError::InvalidSampleRate {
                processor: "SpectrumAnalyzer",
                sample_rate_hz: sample_rate,
            });
        }

        if self.config.multi_resolution {
            if sample_rate != self.sample_rate {
                return Err(ProcessError::SampleRateMismatch {
                    processor: "SpectrumAnalyzer",
                    expected_sample_rate_hz: self.sample_rate,
                    actual_sample_rate_hz: sample_rate,
                });
            }
            self.reset_streaming_state();
            self.push_blockwise(samples);
            return Ok(&self.result);
        }

        // Update bin ranges if sample rate changed
        if self.bin_sample_rate != Some(sample_rate) {
            self.ensure_bin_ranges(sample_rate);
        }
        self.sample_rate = sample_rate;
        self.peak_hold_samples = self
            .config
            .peak_hold_ms
            .map_or(0, |ms| (ms * sample_rate as f64 / 1_000.0).ceil() as usize);
        let hop_size = self.tiers[0].hop_size;
        let input_samples_per_hop = hop_size * self.tiers[0].decimation;
        let (attack_alpha, release_alpha) =
            Self::smoothing_alphas(&self.config, sample_rate, input_samples_per_hop);
        self.tiers[0].attack_alpha = attack_alpha;
        self.tiers[0].release_alpha = release_alpha;

        self.reset_streaming_state();
        let fft_size = self.tiers[0].fft_size;
        self.push_blockwise(&samples[..samples.len().min(fft_size)]);
        Ok(&self.result)
    }

    fn ensure_bin_ranges(&mut self, sample_rate: u32) {
        if self.bin_sample_rate == Some(sample_rate)
            && self.bin_ranges.len() == self.config.num_bins
        {
            return;
        }

        let nyquist = sample_rate as f64 / 2.0;
        let min_freq = MIN_FREQUENCY_HZ;
        let max_freq = nyquist;
        let log_min = min_freq.log10();
        let log_max = max_freq.log10();
        let magnitude_count = self.tiers[0].magnitudes.len();
        let freq_per_bin = nyquist / magnitude_count.max(1) as f64;

        self.bin_ranges.clear();
        for bin_idx in 0..self.config.num_bins {
            let freq_low = 10.0_f64
                .powf(log_min + (log_max - log_min) * bin_idx as f64 / self.config.num_bins as f64);
            let freq_high = 10.0_f64.powf(
                log_min + (log_max - log_min) * (bin_idx + 1) as f64 / self.config.num_bins as f64,
            );
            let idx_low =
                ((freq_low / freq_per_bin) as usize).clamp(0, magnitude_count.saturating_sub(1));
            let idx_high =
                ((freq_high / freq_per_bin) as usize).clamp(idx_low + 1, magnitude_count);
            self.bin_ranges.push((idx_low, idx_high));
        }
        self.bin_sample_rate = Some(sample_rate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The reference implementation below is deliberately kept on rustfft's
    // complex transform: it is the oracle that pins the realfft migration, so
    // rewriting it with realfft would make it self-confirming.
    use rustfft::num_complex::Complex as OracleComplex;
    use rustfft::FftPlanner;

    #[test]
    #[allow(deprecated)]
    fn constructor_rejects_empty_magnitude_and_output_domains() {
        for fft_size in 0..4 {
            assert!(matches!(
                SpectrumAnalyzer::new(fft_size, 4),
                Err(ProcessError::InvalidGeometry {
                    processor: "SpectrumAnalyzer",
                    operation: "create analyzer",
                    ..
                })
            ));
        }
        assert!(matches!(
            SpectrumAnalyzer::new(4, 0),
            Err(ProcessError::InvalidGeometry {
                processor: "SpectrumAnalyzer",
                operation: "create analyzer",
                ..
            })
        ));
    }

    #[test]
    fn zero_sample_rate_rejection_preserves_cached_fft_and_bins() {
        #[allow(deprecated)]
        let mut analyzer = SpectrumAnalyzer::new(16, 4).unwrap();
        let samples: Vec<f64> = (0..16).map(|index| (index as f64 * 0.1).sin()).collect();
        #[allow(deprecated)]
        let _ = analyzer.analyze(&samples, 48_000).unwrap();
        let fft_input = analyzer.tiers[0].fft_input.clone();
        let fft_spectrum = analyzer.tiers[0].fft_spectrum.clone();
        let fft_scratch = analyzer.tiers[0].fft_scratch.clone();
        let magnitudes = analyzer.tiers[0].magnitudes.clone();
        let result = analyzer.result.clone();
        let bin_ranges = analyzer.bin_ranges.clone();
        let bin_sample_rate = analyzer.bin_sample_rate;

        assert_no_alloc::assert_no_alloc(|| {
            #[allow(deprecated)]
            {
                assert!(matches!(
                    analyzer.analyze(&samples, 0),
                    Err(ProcessError::InvalidSampleRate {
                        processor: "SpectrumAnalyzer",
                        sample_rate_hz: 0,
                    })
                ));
            }
        });

        assert_eq!(analyzer.tiers[0].fft_input, fft_input);
        assert_eq!(analyzer.tiers[0].fft_spectrum, fft_spectrum);
        assert_eq!(analyzer.tiers[0].fft_scratch, fft_scratch);
        assert_eq!(analyzer.tiers[0].magnitudes, magnitudes);
        assert_eq!(analyzer.result, result);
        assert_eq!(analyzer.bin_ranges, bin_ranges);
        assert_eq!(analyzer.bin_sample_rate, bin_sample_rate);
    }

    #[test]
    fn short_input_returns_reused_zero_bins() {
        #[allow(deprecated)]
        let mut analyzer = SpectrumAnalyzer::new(16, 4).unwrap();
        #[allow(deprecated)]
        let first_ptr = analyzer.analyze(&[0.0; 8], 48_000).unwrap().as_ptr();
        #[allow(deprecated)]
        {
            assert_eq!(analyzer.analyze(&[0.0; 8], 48_000).unwrap(), &[0.0; 4]);
            assert_eq!(
                analyzer.analyze(&[0.0; 8], 48_000).unwrap().as_ptr(),
                first_ptr
            );
        }
    }

    #[test]
    fn analyze_reuses_result_and_recomputes_ranges_on_sample_rate_change() {
        #[allow(deprecated)]
        let mut analyzer = SpectrumAnalyzer::new(64, 8).unwrap();
        let samples: Vec<f64> = (0..64).map(|i| (i as f64 * 0.1).sin()).collect();

        #[allow(deprecated)]
        let first_ptr = analyzer.analyze(&samples, 48_000).unwrap().as_ptr();
        let first_ranges = analyzer.bin_ranges.clone();
        #[allow(deprecated)]
        {
            assert!(analyzer
                .analyze(&samples, 48_000)
                .unwrap()
                .iter()
                .any(|&v| v > 0.0));
            assert_eq!(
                analyzer.analyze(&samples, 48_000).unwrap().as_ptr(),
                first_ptr
            );
        }
        assert_eq!(analyzer.bin_ranges, first_ranges);

        #[allow(deprecated)]
        let _ = analyzer.analyze(&samples, 96_000).unwrap();
        assert_ne!(analyzer.bin_ranges, first_ranges);
    }

    #[test]
    fn analyzer_output_matches_legacy_allocation_path() {
        #[allow(deprecated)]
        let mut analyzer = SpectrumAnalyzer::new(128, 16).unwrap();
        let samples: Vec<f64> = (0..128)
            .map(|i| {
                let t = i as f64 / 48_000.0;
                (2.0 * std::f64::consts::PI * 997.0 * t).sin() * 0.4
            })
            .collect();

        #[allow(deprecated)]
        let actual = analyzer.analyze(&samples, 48_000).unwrap().to_vec();
        let expected = legacy_analyze(&samples, 128, 16, 48_000);

        for (idx, (actual, expected)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!(
                (actual - expected).abs() <= 1e-6,
                "bin {idx}: actual={actual}, expected={expected}"
            );
        }
    }

    #[test]
    fn odd_single_tier_preserves_legacy_bin_selection() {
        #[allow(deprecated)]
        let mut analyzer = SpectrumAnalyzer::new(5, 4).unwrap();
        let samples = [0.25, -0.5, 0.75, -1.0, 0.5];

        #[allow(deprecated)]
        let actual = analyzer.analyze(&samples, 48_000).unwrap().to_vec();
        let expected = legacy_analyze(&samples, 5, 4, 48_000);

        // The historical complex-FFT path selected `1..N/2`; for N=5 that
        // means only bin 1, even though the real half-spectrum also has bin 2.
        assert_eq!(analyzer.tiers[0].magnitudes.len(), 1);
        for (idx, (actual, expected)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!(
                (actual - expected).abs() <= 1e-6,
                "bin {idx}: actual={actual}, expected={expected}"
            );
        }
    }

    #[test]
    fn with_config_rejects_invalid_geometry() {
        // FFT size too small
        for fft_size in 0..4 {
            assert!(matches!(
                SpectrumAnalyzer::with_config(
                    SpectrumConfig {
                        fft_size,
                        num_bins: 4,
                        hop_divisor: 2,
                        window: WindowFunction::Hann,
                        multi_resolution: false,
                        attack_ms: None,
                        release_ms: None,
                        peak_hold_ms: None,
                        tilt_db_per_octave: 0.0,
                        db_min: -90.0,
                        db_max: 0.0,
                    },
                    48_000
                ),
                Err(ProcessError::InvalidGeometry {
                    processor: "SpectrumAnalyzer",
                    ..
                })
            ));
        }

        // Zero bins
        assert!(matches!(
            SpectrumAnalyzer::with_config(
                SpectrumConfig {
                    fft_size: 128,
                    num_bins: 0,
                    hop_divisor: 2,
                    window: WindowFunction::Hann,
                    multi_resolution: false,
                    attack_ms: None,
                    release_ms: None,
                    peak_hold_ms: None,
                    tilt_db_per_octave: 0.0,
                    db_min: -90.0,
                    db_max: 0.0,
                },
                48_000
            ),
            Err(ProcessError::InvalidGeometry {
                processor: "SpectrumAnalyzer",
                ..
            })
        ));

        // Zero hop divisor
        assert!(matches!(
            SpectrumAnalyzer::with_config(
                SpectrumConfig {
                    fft_size: 128,
                    num_bins: 4,
                    hop_divisor: 0,
                    window: WindowFunction::Hann,
                    multi_resolution: false,
                    attack_ms: None,
                    release_ms: None,
                    peak_hold_ms: None,
                    tilt_db_per_octave: 0.0,
                    db_min: -90.0,
                    db_max: 0.0,
                },
                48_000
            ),
            Err(ProcessError::InvalidGeometry {
                processor: "SpectrumAnalyzer",
                ..
            })
        ));

        // Zero sample rate
        assert!(matches!(
            SpectrumAnalyzer::with_config(SpectrumConfig::default(), 0),
            Err(ProcessError::InvalidSampleRate {
                processor: "SpectrumAnalyzer",
                sample_rate_hz: 0,
            })
        ));
    }

    #[test]
    fn sample_rate_limits_apply_consistently_to_construction_and_analyze() {
        for sample_rate_hz in [1, 40, MAX_MULTI_RES_SAMPLE_RATE_HZ + 1] {
            assert!(matches!(
                SpectrumAnalyzer::with_config(SpectrumConfig::default(), sample_rate_hz),
                Err(ProcessError::InvalidSampleRate {
                    processor: "SpectrumAnalyzer",
                    sample_rate_hz: actual,
                }) if actual == sample_rate_hz
            ));
        }

        let mut single = SpectrumAnalyzer::with_config(
            SpectrumConfig {
                multi_resolution: false,
                ..SpectrumConfig::default()
            },
            48_000,
        )
        .unwrap();
        #[allow(deprecated)]
        let result = single.analyze(&[0.0; 4_096], 40);
        assert!(matches!(
            result,
            Err(ProcessError::InvalidSampleRate {
                processor: "SpectrumAnalyzer",
                sample_rate_hz: 40,
            })
        ));
    }

    #[test]
    fn overlap_builder_rounds_to_the_nearest_supported_value() {
        for (percent, expected_divisor) in [(0, 1), (24, 1), (25, 2), (62, 2), (63, 4), (100, 4)] {
            assert_eq!(
                SpectrumConfig::default()
                    .with_overlap_percent(percent)
                    .hop_divisor,
                expected_divisor,
                "unexpected divisor for {percent}% overlap"
            );
        }
    }

    #[test]
    fn attack_builder_none_disables_default_smoothing_consistently() {
        let config = SpectrumConfig::default()
            .with_multi_resolution(false)
            .with_attack_ms(None);

        assert_eq!(config.attack_ms, None);
        assert_eq!(config.release_ms, None);
        assert!(SpectrumAnalyzer::with_config(config, 48_000).is_ok());
    }

    #[test]
    fn with_config_rejects_non_finite_and_out_of_range_parameters() {
        fn assert_invalid_parameter(config: SpectrumConfig, parameter: &'static str) {
            assert!(matches!(
                SpectrumAnalyzer::with_config(config, 48_000),
                Err(ProcessError::InvalidParameter {
                    processor: "SpectrumAnalyzer",
                    parameter: actual,
                    ..
                }) if actual == parameter
            ));
        }

        for attack_ms in [Some(0.0), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_invalid_parameter(
                SpectrumConfig {
                    attack_ms,
                    multi_resolution: false,
                    ..SpectrumConfig::default()
                },
                "attack_ms",
            );
        }
        for release_ms in [Some(0.0), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_invalid_parameter(
                SpectrumConfig {
                    attack_ms: Some(10.0),
                    release_ms,
                    multi_resolution: false,
                    ..SpectrumConfig::default()
                },
                "release_ms",
            );
        }
        assert_invalid_parameter(
            SpectrumConfig {
                attack_ms: None,
                release_ms: Some(10.0),
                multi_resolution: false,
                ..SpectrumConfig::default()
            },
            "release_ms",
        );
        for peak_hold_ms in [Some(0.0), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_invalid_parameter(
                SpectrumConfig {
                    peak_hold_ms,
                    multi_resolution: false,
                    ..SpectrumConfig::default()
                },
                "peak_hold_ms",
            );
        }
        assert_invalid_parameter(
            SpectrumConfig {
                tilt_db_per_octave: f64::NAN,
                multi_resolution: false,
                ..SpectrumConfig::default()
            },
            "tilt_db_per_octave",
        );
        for (db_min, db_max) in [
            (f64::NAN, 0.0),
            (-90.0, f64::INFINITY),
            (0.0, 0.0),
            (1.0, 0.0),
        ] {
            assert_invalid_parameter(
                SpectrumConfig {
                    db_min,
                    db_max,
                    multi_resolution: false,
                    ..SpectrumConfig::default()
                },
                "db_range",
            );
        }
        for beta in [-1.0, 50.1, f64::NAN, f64::INFINITY] {
            assert_invalid_parameter(
                SpectrumConfig {
                    window: WindowFunction::Kaiser { beta },
                    multi_resolution: false,
                    ..SpectrumConfig::default()
                },
                "window.beta",
            );
        }
    }

    #[test]
    fn blocks_publish_after_a_full_window_then_at_each_hop() {
        let config = SpectrumConfig {
            fft_size: 4_096,
            num_bins: 64,
            hop_divisor: 2, // 50% overlap, hop = 2,048
            window: WindowFunction::Hann,
            multi_resolution: false,
            attack_ms: None,
            release_ms: None,
            peak_hold_ms: None,
            tilt_db_per_octave: 0.0,
            db_min: -90.0,
            db_max: 0.0,
        };
        let mut analyzer = SpectrumAnalyzer::with_config(config, 48_000).unwrap();
        let input: Vec<f64> = (0..6_144)
            .map(|sample_idx| {
                (std::f64::consts::TAU * 997.0 * sample_idx as f64 / 48_000.0).sin() * 0.5
            })
            .collect();

        assert!(analyzer.spectrum().is_none());

        // Seven 512-sample callback-sized blocks are still one block short of
        // the first complete 4096-sample FFT window.
        for block in input[..3_584].chunks_exact(512) {
            assert_eq!(analyzer.push(block), 0);
            assert!(analyzer.spectrum().is_none());
        }

        assert_eq!(analyzer.push(&input[3_584..4_096]), 1);
        assert!(analyzer
            .spectrum()
            .is_some_and(|spectrum| spectrum.iter().any(|value| *value > 0.0)));

        assert_eq!(analyzer.push(&input[4_096..6_143]), 0);
        assert_eq!(analyzer.push(&input[6_143..]), 1);
    }

    #[test]
    fn chunked_push_matches_one_shot_analyze() {
        let config = SpectrumConfig {
            fft_size: 128,
            num_bins: 16,
            hop_divisor: 1, // No overlap for direct comparison
            window: WindowFunction::Hann,
            multi_resolution: false,
            attack_ms: None,
            release_ms: None,
            peak_hold_ms: None,
            tilt_db_per_octave: 0.0,
            db_min: -90.0,
            db_max: 0.0,
        };
        let mut streaming = SpectrumAnalyzer::with_config(config.clone(), 48_000).unwrap();
        #[allow(deprecated)]
        let mut one_shot = SpectrumAnalyzer::new(128, 16).unwrap();

        let samples: Vec<f64> = (0..128)
            .map(|i| {
                let t = i as f64 / 48_000.0;
                (2.0 * std::f64::consts::PI * 440.0 * t).sin() * 0.5
            })
            .collect();

        // Push in chunks
        streaming.push(&samples[..32]);
        streaming.push(&samples[32..64]);
        streaming.push(&samples[64..96]);
        streaming.push(&samples[96..]);

        let streaming_result = streaming.spectrum().unwrap();
        #[allow(deprecated)]
        let one_shot_result = one_shot.analyze(&samples, 48_000).unwrap();

        for (idx, (&s, &o)) in streaming_result
            .iter()
            .zip(one_shot_result.iter())
            .enumerate()
        {
            assert!(
                (s - o).abs() <= 1e-6,
                "bin {idx}: streaming={s}, one_shot={o}"
            );
        }
    }

    #[test]
    fn ring_buffer_wraps_correctly() {
        let config = SpectrumConfig {
            fft_size: 16,
            num_bins: 4,
            hop_divisor: 2, // hop = 8
            window: WindowFunction::Hann,
            multi_resolution: false,
            attack_ms: None,
            release_ms: None,
            peak_hold_ms: None,
            tilt_db_per_octave: 0.0,
            db_min: -90.0,
            db_max: 0.0,
        };
        let mut analyzer = SpectrumAnalyzer::with_config(config, 48_000).unwrap();

        // Fill ring buffer and advance past multiple wraps
        let samples: Vec<f64> = (0..100).map(|i| (i as f64 * 0.1).sin()).collect();
        let count = analyzer.push(&samples);

        // One full-window publication, then floor((100 - 16) / 8) hop updates.
        assert_eq!(count, 11);

        // Verify ring_pos wrapped (should be at position 100 % 16 = 4)
        assert_eq!(analyzer.tiers[0].ring_pos, 4);

        // Verify samples_since_hop is residual (100 % 8 = 4)
        assert_eq!(analyzer.tiers[0].samples_since_hop, 4);
    }

    #[test]
    fn push_and_spectrum_are_allocation_free() {
        let config = SpectrumConfig {
            fft_size: 4096,
            num_bins: 256,
            hop_divisor: 4, // 75% overlap
            window: WindowFunction::Hann,
            multi_resolution: false,
            attack_ms: None,
            release_ms: None,
            peak_hold_ms: None,
            tilt_db_per_octave: 0.0,
            db_min: -90.0,
            db_max: 0.0,
        };
        let mut analyzer = SpectrumAnalyzer::with_config(config, 48_000).unwrap();

        // Prime the analyzer with enough samples for first spectrum
        let warmup: Vec<f64> = (0..4096).map(|i| (i as f64 * 0.01).sin()).collect();
        analyzer.push(&warmup);

        // Now verify allocation-free operation
        // Larger than the ring to cover oversized caller blocks as well as
        // the ordinary callback-sized path.
        let samples: Vec<f64> = (0..8192).map(|i| (i as f64 * 0.01).sin()).collect();

        assert_no_alloc::assert_no_alloc(|| {
            let count = analyzer.push(&samples);
            assert_eq!(count, 8);
            let spectrum = analyzer.spectrum();
            assert!(spectrum.is_some());
            assert_eq!(spectrum.unwrap().len(), 256);
        });
    }

    #[test]
    fn multi_resolution_warms_tiers_independently_and_counts_publications() {
        let config = SpectrumConfig {
            num_bins: 64,
            hop_divisor: 4,
            multi_resolution: true,
            ..SpectrumConfig::default()
        };
        let mut analyzer = SpectrumAnalyzer::with_config(config, 48_000).unwrap();
        let input: Vec<f64> = (0..17_408)
            .map(|i| (std::f64::consts::TAU * 100.0 * i as f64 / 48_000.0).sin())
            .collect();

        assert_eq!(analyzer.push(&input[..4_095]), 0);
        assert!(analyzer.spectrum().is_none());
        assert_eq!(analyzer.push(&input[4_095..4_096]), 1);

        let db_after_tier_a = analyzer.spectrum_db().unwrap();
        for (band_idx, &tier_idx) in analyzer.band_to_tier.iter().enumerate() {
            if tier_idx > 0 {
                assert_eq!(db_after_tier_a[band_idx], analyzer.config.db_min as f32);
            }
        }

        // Publications occur at 4096, then every 1024 input samples. The
        // tier-A and tier-B updates at 16384 share one published frame.
        assert_eq!(analyzer.push(&input[4_096..16_384]), 12);
        assert_eq!(analyzer.tiers[0].filled_samples, 4_096);
        // Tier 1 uses ÷4 decimation: 12288 input samples ÷ 4 = 3072 decimated samples
        // After 16384 total inputs, tier B has received 16384/4 = 4096 decimated samples
        assert_eq!(analyzer.tiers[1].filled_samples, 4_096);
        // Tier 2 uses ÷16 decimation: 16384 input samples ÷ 16 = 1024 decimated samples
        assert_eq!(analyzer.tiers[2].filled_samples, 1_024);
        for (band_idx, &tier_idx) in analyzer.band_to_tier.iter().enumerate() {
            if tier_idx == 2 {
                assert_eq!(analyzer.result_db[band_idx], analyzer.config.db_min as f32);
            }
        }

        let tier_b_before: Vec<f32> = analyzer
            .result_db
            .iter()
            .zip(&analyzer.band_to_tier)
            .filter_map(|(&db, &tier_idx)| (tier_idx == 1).then_some(db))
            .collect();
        assert!(tier_b_before
            .iter()
            .any(|&db| db > analyzer.config.db_min as f32));

        // Record tier B's hop counter before the push
        let tier_b_samples_since_hop_before = analyzer.tiers[1].samples_since_hop;

        assert_eq!(analyzer.push(&input[16_384..]), 1);

        // Tier B should not have computed a new spectrum (hop counter should have increased)
        let tier_b_samples_since_hop_after = analyzer.tiers[1].samples_since_hop;
        assert!(
            tier_b_samples_since_hop_after > tier_b_samples_since_hop_before,
            "Tier B should not have hopped (before: {}, after: {})",
            tier_b_samples_since_hop_before,
            tier_b_samples_since_hop_after
        );
    }

    #[test]
    fn multi_resolution_push_and_read_are_allocation_free_after_construction() {
        let config = SpectrumConfig {
            num_bins: 128,
            hop_divisor: 4,
            multi_resolution: true,
            attack_ms: Some(30.0),
            release_ms: Some(250.0),
            peak_hold_ms: Some(500.0),
            ..SpectrumConfig::default()
        };
        let mut analyzer = SpectrumAnalyzer::with_config(config, 48_000).unwrap();
        let warmup: Vec<f64> = (0..65_536)
            .map(|i| (std::f64::consts::TAU * 997.0 * i as f64 / 48_000.0).sin())
            .collect();
        assert_eq!(analyzer.push(&warmup), 61);
        // Tier 2 uses ÷16 decimation: 65536 ÷ 16 = 4096 decimated samples (fills the 4096 FFT buffer)
        assert_eq!(analyzer.tiers[2].filled_samples, 4_096);
        assert_eq!(analyzer.tiers[2].samples_since_hop, 0);
        let input = &warmup[..16_384];

        assert_no_alloc::assert_no_alloc(|| {
            assert_eq!(analyzer.push(input), 16);
            assert_eq!(analyzer.spectrum().unwrap().len(), 128);
            assert_eq!(analyzer.spectrum_db().unwrap().len(), 128);
        });
    }

    #[test]
    fn multi_resolution_window_power_is_consistent_across_tiers() {
        let config = SpectrumConfig {
            num_bins: 64,
            multi_resolution: true,
            ..SpectrumConfig::default()
        };
        let mut analyzer = SpectrumAnalyzer::with_config(config, 48_000).unwrap();
        let mut levels_db = [0.0; 3];

        for (tier_idx, level_db) in levels_db.iter_mut().enumerate() {
            let fft_size = analyzer.tiers[tier_idx].fft_size;
            let fft_bin = 37.0;
            for (sample_idx, sample) in analyzer.tiers[tier_idx].ring_buffer.iter_mut().enumerate()
            {
                *sample = 0.5
                    * (std::f64::consts::TAU * fft_bin * sample_idx as f64 / fft_size as f64).sin();
            }
            analyzer.compute_spectrum_tier(tier_idx);
            let tier = &analyzer.tiers[tier_idx];
            *level_db =
                10.0 * (tier.magnitudes.iter().sum::<f64>() / tier.window_power_mean).log10();
        }

        let minimum = levels_db.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = levels_db.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(maximum - minimum < 1.0e-9, "tier levels: {levels_db:?}");
    }

    #[test]
    fn multi_resolution_is_bit_exact_across_random_input_chunks() {
        let config = SpectrumConfig {
            num_bins: 128,
            hop_divisor: 4,
            window: WindowFunction::Hann,
            multi_resolution: true,
            attack_ms: None,
            release_ms: None,
            peak_hold_ms: None,
            tilt_db_per_octave: 0.0,
            ..SpectrumConfig::default()
        };
        let samples: Vec<f64> = (0..100_000)
            .map(|index| {
                let t = index as f64 / 48_000.0;
                0.31 * (std::f64::consts::TAU * 40.0 * t).sin()
                    + 0.23 * (std::f64::consts::TAU * 997.0 * t).sin()
                    + (index as f64 * 0.037).cos() * 0.07
            })
            .collect();

        let mut one_block = SpectrumAnalyzer::with_config(config.clone(), 48_000).unwrap();
        let expected_publications = one_block.push(&samples);

        let mut chunked = SpectrumAnalyzer::with_config(config, 48_000).unwrap();
        let mut offset = 0;
        let mut state = 0x4d59_5df4_d0f3_3173_u64;
        let mut actual_publications = 0;
        while offset < samples.len() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let requested = (state as usize % 5_000) + 1;
            let end = (offset + requested).min(samples.len());
            actual_publications += chunked.push(&samples[offset..end]);
            offset = end;
        }

        assert_eq!(actual_publications, expected_publications);
        for (band, (&expected, &actual)) in one_block
            .spectrum_db()
            .unwrap()
            .iter()
            .zip(chunked.spectrum_db().unwrap())
            .enumerate()
        {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "band {band} differed: expected {expected}, actual {actual}"
            );
        }
    }

    #[test]
    fn multi_resolution_band_assignment_matches_reference_and_passband_limits() {
        for (num_bins, tier_c_end, tier_b_end) in [(64, 8, 20), (128, 29, 54)] {
            let analyzer = SpectrumAnalyzer::with_config(
                SpectrumConfig {
                    num_bins,
                    multi_resolution: true,
                    ..SpectrumConfig::default()
                },
                48_000,
            )
            .unwrap();

            assert!(analyzer.band_to_tier[..=tier_c_end]
                .iter()
                .all(|&tier| tier == 2));
            assert!(analyzer.band_to_tier[tier_c_end + 1..=tier_b_end]
                .iter()
                .all(|&tier| tier == 1));
            assert!(analyzer.band_to_tier[tier_b_end + 1..]
                .iter()
                .all(|&tier| tier == 0));
        }

        let analyzer = SpectrumAnalyzer::with_config(
            SpectrumConfig {
                num_bins: 2_048,
                multi_resolution: true,
                ..SpectrumConfig::default()
            },
            48_000,
        )
        .unwrap();
        let log_min = MIN_FREQUENCY_HZ.log10();
        let log_max = (48_000.0_f64 / 2.0).log10();
        for (band, (&tier_idx, &(idx_low, idx_high))) in analyzer
            .band_to_tier
            .iter()
            .zip(&analyzer.multi_band_ranges)
            .enumerate()
        {
            let freq_high =
                10.0_f64.powf(log_min + (log_max - log_min) * (band + 1) as f64 / 2_048.0);
            let tier = &analyzer.tiers[tier_idx];
            if tier.decimation > 1 {
                let usable_hz =
                    DECIMATED_PASSBAND_FRACTION * (48_000.0 / (2.0 * tier.decimation as f64));
                assert!(
                    freq_high <= usable_hz,
                    "band {band} reaches {freq_high} Hz beyond tier {tier_idx} edge {usable_hz} Hz"
                );
            }
            assert!(idx_low < idx_high, "empty range for band {band}");
            assert!(
                idx_high <= tier.magnitudes.len(),
                "band {band} range {idx_low}..{idx_high} exceeds tier {tier_idx}"
            );
        }
    }

    fn full_rate_reference_band_db(
        samples: &[f64],
        fft_size: usize,
        bin_range: (usize, usize),
    ) -> f64 {
        let window = SpectrumAnalyzer::window_values(WindowFunction::Hann, fft_size);
        let window_power_mean =
            window.iter().map(|value| value * value).sum::<f64>() / fft_size as f64;
        let mut buffer: Vec<OracleComplex<f64>> = samples[samples.len() - fft_size..]
            .iter()
            .zip(&window)
            .map(|(&sample, &weight)| OracleComplex::new(sample * weight, 0.0))
            .collect();
        FftPlanner::new()
            .plan_fft_forward(fft_size)
            .process(&mut buffer);

        let (idx_low, idx_high) = bin_range;
        let n_squared = (fft_size as f64) * (fft_size as f64);
        let sum_power = buffer[1 + idx_low..1 + idx_high]
            .iter()
            .map(|value| value.norm_sqr() / n_squared)
            .sum::<f64>();
        10.0 * (sum_power / window_power_mean + 1e-18).log10()
    }

    #[test]
    fn decimated_tier_levels_match_full_rate_fft_reference() {
        const SAMPLE_RATE: u32 = 48_000;
        const INPUT_LEN: usize = 98_304;

        for (frequency_hz, expected_tier) in [(40.0, 2), (150.0, 1)] {
            let config = SpectrumConfig {
                num_bins: 128,
                hop_divisor: 4,
                window: WindowFunction::Hann,
                multi_resolution: true,
                attack_ms: None,
                release_ms: None,
                peak_hold_ms: None,
                tilt_db_per_octave: 0.0,
                ..SpectrumConfig::default()
            };
            let samples: Vec<f64> = (0..INPUT_LEN)
                .map(|index| {
                    0.5 * (std::f64::consts::TAU * frequency_hz * index as f64 / SAMPLE_RATE as f64)
                        .sin()
                })
                .collect();
            let mut analyzer = SpectrumAnalyzer::with_config(config, SAMPLE_RATE).unwrap();
            analyzer.push(&samples);

            let log_min = MIN_FREQUENCY_HZ.log10();
            let log_max = (SAMPLE_RATE as f64 / 2.0).log10();
            let band = (0..analyzer.config.num_bins)
                .find(|&band| {
                    let low = 10.0_f64.powf(
                        log_min
                            + (log_max - log_min) * band as f64 / analyzer.config.num_bins as f64,
                    );
                    let high = 10.0_f64.powf(
                        log_min
                            + (log_max - log_min) * (band + 1) as f64
                                / analyzer.config.num_bins as f64,
                    );
                    frequency_hz >= low && frequency_hz < high
                })
                .unwrap();
            assert_eq!(analyzer.band_to_tier[band], expected_tier);

            let tier = &analyzer.tiers[expected_tier];
            let reference_db = full_rate_reference_band_db(
                &samples,
                tier.fft_size * tier.decimation,
                analyzer.multi_band_ranges[band],
            );
            let actual_db = analyzer.spectrum_db().unwrap()[band] as f64;
            assert!(
                (actual_db - reference_db).abs() <= 0.1,
                "{frequency_hz} Hz band {band}: tier={actual_db:.6} dB, reference={reference_db:.6} dB"
            );
        }
    }

    #[test]
    #[allow(deprecated)]
    fn deprecated_analyze_publishes_state_and_supports_multi_resolution() {
        let samples: Vec<f64> = (0..65_536)
            .map(|i| (std::f64::consts::TAU * 440.0 * i as f64 / 48_000.0).sin())
            .collect();

        let mut legacy = SpectrumAnalyzer::new(4_096, 64).unwrap();
        legacy.analyze(&samples[..4_096], 48_000).unwrap();
        assert!(legacy.spectrum().is_some());
        assert!(legacy.spectrum_db().is_some());

        let mut multi = SpectrumAnalyzer::with_config(
            SpectrumConfig {
                multi_resolution: true,
                ..SpectrumConfig::default()
            },
            48_000,
        )
        .unwrap();
        multi.analyze(&samples, 48_000).unwrap();
        assert!(multi.spectrum().is_some());
        assert!(matches!(
            multi.analyze(&samples, 44_100),
            Err(ProcessError::SampleRateMismatch {
                processor: "SpectrumAnalyzer",
                expected_sample_rate_hz: 48_000,
                actual_sample_rate_hz: 44_100,
            })
        ));
    }

    #[test]
    fn window_selection_uses_expected_shapes() {
        let analyzer_for = |window| {
            SpectrumAnalyzer::with_config(
                SpectrumConfig {
                    fft_size: 64,
                    num_bins: 8,
                    window,
                    multi_resolution: false,
                    ..SpectrumConfig::default()
                },
                48_000,
            )
            .unwrap()
        };
        let hann = analyzer_for(WindowFunction::Hann);
        let blackman_harris = analyzer_for(WindowFunction::BlackmanHarris4);
        let rectangular = analyzer_for(WindowFunction::Kaiser { beta: 0.0 });

        assert_eq!(hann.tiers[0].window[0], 0.0);
        assert!((blackman_harris.tiers[0].window[0] - 0.000_06).abs() < 1.0e-10);
        assert!(rectangular.tiers[0]
            .window
            .iter()
            .all(|&sample| sample == 1.0));
        assert_ne!(hann.tiers[0].window, blackman_harris.tiers[0].window);
    }

    #[test]
    fn tilt_matches_the_configured_octave_relationship() {
        let base = SpectrumConfig {
            fft_size: 4_096,
            num_bins: 64,
            multi_resolution: false,
            attack_ms: None,
            release_ms: None,
            db_min: -120.0,
            ..SpectrumConfig::default()
        };
        let mut flat = SpectrumAnalyzer::with_config(base.clone(), 48_000).unwrap();
        let mut tilted = SpectrumAnalyzer::with_config(
            SpectrumConfig {
                tilt_db_per_octave: 4.5,
                ..base
            },
            48_000,
        )
        .unwrap();
        let tone: Vec<f64> = (0..4_096)
            .map(|i| 0.2 * (std::f64::consts::TAU * 500.0 * i as f64 / 48_000.0).sin())
            .collect();
        flat.push(&tone);
        tilted.push(&tone);
        let peak_band = flat
            .spectrum_db()
            .unwrap()
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .unwrap()
            .0;
        let expected_delta = tilted.tilt_db_for_band(peak_band);
        let actual_delta = f64::from(tilted.result_db[peak_band] - flat.result_db[peak_band]);
        assert!((actual_delta - expected_delta).abs() < 1.0e-5);
    }

    #[test]
    fn release_and_peak_hold_follow_elapsed_sample_time() {
        let config = SpectrumConfig {
            fft_size: 100,
            num_bins: 1,
            hop_divisor: 10,
            multi_resolution: false,
            attack_ms: Some(10.0),
            release_ms: Some(100.0),
            peak_hold_ms: Some(100.0),
            db_min: -100.0,
            ..SpectrumConfig::default()
        };
        let mut analyzer = SpectrumAnalyzer::with_config(config, 1_000).unwrap();
        analyzer.prev_db.as_mut().unwrap()[0] = -20.0;
        analyzer.peak_db = None;
        analyzer.peak_hold_remaining_samples = None;

        let attack_alpha = analyzer.tiers[0].attack_alpha;
        let release_alpha = analyzer.tiers[0].release_alpha;
        let mut released = -20.0;
        for _ in 0..10 {
            released = analyzer.apply_level_processing(0, -80.0, attack_alpha, release_alpha, 10);
        }
        let expected = -20.0 + (1.0 - (-1.0_f64).exp()) * (-80.0 - -20.0);
        assert!((released - expected).abs() < 1.0e-10);

        analyzer.prev_db = None;
        analyzer.peak_db = Some(vec![-100.0]);
        analyzer.peak_hold_remaining_samples = Some(vec![0]);
        assert_eq!(
            analyzer.apply_level_processing(0, -10.0, 0.0, 0.0, 10),
            -10.0
        );
        for _ in 0..10 {
            assert_eq!(
                analyzer.apply_level_processing(0, -60.0, 0.0, 0.0, 10),
                -10.0
            );
        }
        let decayed = analyzer.apply_level_processing(0, -60.0, 0.0, 0.0, 10);
        assert!((decayed - -10.2).abs() < 1.0e-10);
    }

    #[test]
    fn multi_resolution_mode_produces_distinct_low_frequency_peaks() {
        use std::f64::consts::PI;

        let config = SpectrumConfig {
            num_bins: 64,
            hop_divisor: 4,
            multi_resolution: true,
            ..SpectrumConfig::default()
        };

        let peak_band = |frequency_hz: f64| {
            let mut analyzer = SpectrumAnalyzer::with_config(config.clone(), 48_000).unwrap();
            let samples: Vec<f64> = (0..65_536)
                .map(|i| 0.3 * (2.0 * PI * frequency_hz * i as f64 / 48_000.0).sin())
                .collect();
            analyzer.push(&samples);
            analyzer
                .spectrum_db()
                .unwrap()
                .iter()
                .take(16)
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .unwrap()
                .0
        };

        let expected_band = |frequency_hz: f64| {
            (((frequency_hz / MIN_FREQUENCY_HZ).ln() / ((48_000.0 / 2.0) / MIN_FREQUENCY_HZ).ln())
                * 64.0)
                .floor() as usize
        };
        let band_30_hz = peak_band(30.0);
        let band_45_hz = peak_band(45.0);

        assert!(band_30_hz.abs_diff(expected_band(30.0)) <= 1);
        assert!(band_45_hz.abs_diff(expected_band(45.0)) <= 1);
        assert!(band_30_hz.abs_diff(band_45_hz) >= 2);
    }

    #[test]
    fn ballistics_attack_time_constant() {
        use std::f64::consts::PI;

        let attack_ms = 100.0; // 100 ms attack time
        let config = SpectrumConfig {
            fft_size: 2048,
            num_bins: 32,
            hop_divisor: 2,
            window: WindowFunction::Hann,
            multi_resolution: false,
            attack_ms: Some(attack_ms),
            release_ms: None, // Use same for release
            peak_hold_ms: None,
            tilt_db_per_octave: 0.0,
            db_min: -90.0,
            db_max: 0.0,
        };

        let sample_rate = 48_000;
        let mut analyzer = SpectrumAnalyzer::with_config(config, sample_rate).unwrap();

        // Generate 1 kHz tone at -20 dB (0.1 amplitude)
        let tone_freq = 1000.0;
        let tone_amp = 0.1;

        // Feed silence first (prime the analyzer)
        let silence: Vec<f64> = vec![0.0; 4096];
        analyzer.push(&silence);

        // Now feed the tone continuously and measure rise time
        let hop_size = 2048 / 2; // 50% overlap
        let frame_duration_ms = (hop_size as f64 / sample_rate as f64) * 1000.0;

        // Feed enough samples to reach steady state (5 time constants = 500 ms)
        let num_frames_to_steady = ((5.0 * attack_ms) / frame_duration_ms).ceil() as usize;

        let mut peak_band_levels_db = Vec::new();

        for _ in 0..num_frames_to_steady {
            let samples: Vec<f64> = (0..hop_size)
                .map(|i| {
                    let t = i as f64 / sample_rate as f64;
                    tone_amp * (2.0 * PI * tone_freq * t).sin()
                })
                .collect();

            analyzer.push(&samples);

            if let Some(spectrum_db) = analyzer.spectrum_db() {
                // Find the peak band (around 1 kHz)
                let peak_db = spectrum_db
                    .iter()
                    .copied()
                    .max_by(|a, b| a.partial_cmp(b).unwrap())
                    .unwrap();
                peak_band_levels_db.push(peak_db);
            }
        }

        // The final steady-state level
        let steady_state_db = *peak_band_levels_db.last().unwrap();

        // After one time constant (attack_ms), the level should reach ~63.2% of final value
        // In dB domain with exponential smoothing: y(t) = y_final * (1 - e^(-t/τ))
        // After one τ: y(τ) = y_final * (1 - e^(-1)) ≈ 0.632 * y_final
        //
        // Since we start from silence (very low dB), we measure the rise from initial to final.
        // Frame index for one time constant: attack_ms / frame_duration_ms
        let one_tc_frame_idx = (attack_ms / frame_duration_ms).round() as usize;

        if one_tc_frame_idx < peak_band_levels_db.len() {
            let initial_db = peak_band_levels_db[0];
            let db_at_one_tc = peak_band_levels_db[one_tc_frame_idx];

            // The rise should be approximately 63% of the total rise
            let total_rise = steady_state_db - initial_db;
            let rise_at_one_tc = db_at_one_tc - initial_db;
            let fraction = rise_at_one_tc / total_rise;

            // Allow ±10% tolerance (0.63 ± 0.1)
            assert!(
                (fraction - 0.632).abs() < 0.1,
                "After one time constant ({} ms), signal should reach ~63% of final level. \
                 Got {:.1}%, expected ~63.2%",
                attack_ms,
                fraction * 100.0
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
        let mut buffer: Vec<OracleComplex<f64>> = samples[..fft_size]
            .iter()
            .zip(&window)
            .map(|(&s, &w)| OracleComplex::new(s * w, 0.0))
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
