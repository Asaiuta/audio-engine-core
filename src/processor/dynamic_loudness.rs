//! Dynamic Loudness Compensation based on ISO 226:2003 Equal-Loudness Contours
//!
//! Implements a 7-band dynamic EQ that compensates for human hearing's frequency
//! sensitivity changes at different loudness levels (Fletcher-Munson effect).
//!
//! # Features
//!
//! - 7-band dynamic EQ (Low Shelf, 5 Peaking, High Shelf)
//! - ISO 226 inspired compensation curves
//! - Block-based coefficient updates for CPU efficiency
//! - Smooth parameter transitions (50ms default)
//! - User-adjustable strength (0-100%)
//!
//! # DSP Chain Position
//!
//! ```text
//! Source buffer
//!   → Loudness normalizer gain
//!   → DspChain: EQ → Saturation → Crossfeed
//!              → merged FFT convolver (external IR and/or FIR EQ)
//!              → Volume → DynamicLoudness → PeakLimiter
//!   → resampler → NoiseShaper → output
//! ```

use atomic_float::AtomicF32;
use std::sync::atomic::{AtomicBool, Ordering};

// ============================================================================
// Biquad Filter Types
// ============================================================================

/// Biquad filter coefficients (normalized)
#[derive(Clone, Copy, Debug)]
struct BiquadCoeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

/// Biquad filter state (delay elements)
#[derive(Clone, Debug, Default)]
struct BiquadState {
    z1: f64,
    z2: f64,
}

/// Frequency/sample-rate invariants for a biquad filter.
#[derive(Clone, Debug)]
struct BiquadGeometry {
    freq: f64,
    q: f64,
    sample_rate: f64,
    cos_w0: f64,
    sin_w0: f64,
    alpha: f64,
}

impl BiquadGeometry {
    fn new(freq: f64, q: f64, sample_rate: f64, filter_type: FilterType) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = match filter_type {
            FilterType::Peaking => sin_w0 / (2.0 * q),
            FilterType::LowShelf | FilterType::HighShelf => sin_w0 / std::f64::consts::SQRT_2,
        };

        Self {
            freq,
            q,
            sample_rate,
            cos_w0,
            sin_w0,
            alpha,
        }
    }
}

/// Biquad filter with multiple filter types
#[derive(Clone, Debug)]
struct BiquadFilter {
    geometry: BiquadGeometry,
    coeffs: BiquadCoeffs,
    state: BiquadState,
    filter_type: FilterType,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum FilterType {
    Peaking,
    LowShelf,
    HighShelf,
}

impl BiquadFilter {
    /// Create a peaking/bell filter
    fn peaking(freq: f64, gain_db: f64, q: f64, sample_rate: f64) -> Self {
        let filter_type = FilterType::Peaking;
        let geometry = BiquadGeometry::new(freq, q, sample_rate, filter_type);
        let coeffs = Self::calc_peaking_coeffs(&geometry, gain_db);
        Self {
            geometry,
            coeffs,
            state: BiquadState::default(),
            filter_type,
        }
    }

    /// Create a low shelf filter
    fn low_shelf(freq: f64, gain_db: f64, sample_rate: f64) -> Self {
        let filter_type = FilterType::LowShelf;
        let geometry = BiquadGeometry::new(freq, 0.7, sample_rate, filter_type);
        let coeffs = Self::calc_low_shelf_coeffs(&geometry, gain_db);
        Self {
            geometry,
            coeffs,
            state: BiquadState::default(),
            filter_type,
        }
    }

    /// Create a high shelf filter
    fn high_shelf(freq: f64, gain_db: f64, sample_rate: f64) -> Self {
        let filter_type = FilterType::HighShelf;
        let geometry = BiquadGeometry::new(freq, 0.7, sample_rate, filter_type);
        let coeffs = Self::calc_high_shelf_coeffs(&geometry, gain_db);
        Self {
            geometry,
            coeffs,
            state: BiquadState::default(),
            filter_type,
        }
    }

    /// Calculate peaking filter coefficients
    /// Using RBJ Audio EQ Cookbook formulas
    fn calc_peaking_coeffs(geometry: &BiquadGeometry, gain_db: f64) -> BiquadCoeffs {
        if gain_db.abs() < 0.0001 {
            // Unity gain: bypass
            return BiquadCoeffs::default();
        }

        let a = 10.0_f64.powf(gain_db / 40.0); // gain_db/40 for peaking
        let cos_w0 = geometry.cos_w0;
        let alpha = geometry.alpha;

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        BiquadCoeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Calculate low shelf filter coefficients
    /// Using RBJ cookbook with S=1 (shelf slope, 12dB/octave)
    fn calc_low_shelf_coeffs(geometry: &BiquadGeometry, gain_db: f64) -> BiquadCoeffs {
        if gain_db.abs() < 0.0001 {
            return BiquadCoeffs::default();
        }

        let a = 10.0_f64.powf(gain_db / 40.0);
        let cos_w0 = geometry.cos_w0;
        let sin_w0 = geometry.sin_w0;

        // RBJ cookbook: S=1 (shelf slope), alpha and beta formulas
        // alpha = sin(w0)/2 * sqrt(2) when S=1
        // beta = 2 * sqrt(A) * alpha
        let alpha = geometry.alpha;
        let beta = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + beta * sin_w0);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - beta * sin_w0);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + beta * sin_w0;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - beta * sin_w0;

        BiquadCoeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Calculate high shelf filter coefficients
    /// Using RBJ cookbook with S=1 (shelf slope, 12dB/octave)
    fn calc_high_shelf_coeffs(geometry: &BiquadGeometry, gain_db: f64) -> BiquadCoeffs {
        if gain_db.abs() < 0.0001 {
            return BiquadCoeffs::default();
        }

        let a = 10.0_f64.powf(gain_db / 40.0);
        let cos_w0 = geometry.cos_w0;
        let sin_w0 = geometry.sin_w0;

        // RBJ cookbook: S=1 (shelf slope), alpha and beta formulas
        let alpha = geometry.alpha;
        let beta = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + beta * sin_w0);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - beta * sin_w0);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + beta * sin_w0;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - beta * sin_w0;

        BiquadCoeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Update gain (recalculates coefficients)
    #[cfg(test)]
    fn set_gain_db(&mut self, gain_db: f64) {
        self.coeffs = match self.filter_type {
            FilterType::Peaking => Self::calc_peaking_coeffs(&self.geometry, gain_db),
            FilterType::LowShelf => Self::calc_low_shelf_coeffs(&self.geometry, gain_db),
            FilterType::HighShelf => Self::calc_high_shelf_coeffs(&self.geometry, gain_db),
        };
    }

    /// Process a single sample (Direct Form I)
    #[inline(always)]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.coeffs.b0 * x + self.state.z1;
        self.state.z1 = self.coeffs.b1 * x - self.coeffs.a1 * y + self.state.z2;
        self.state.z2 = self.coeffs.b2 * x - self.coeffs.a2 * y;
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            self.state.z1 = crate::runtime::flush_subnormal_sample(self.state.z1);
            self.state.z2 = crate::runtime::flush_subnormal_sample(self.state.z2);
        }
        y
    }

    /// Reset filter state
    fn reset(&mut self) {
        self.state = BiquadState::default();
    }

    /// Update sample rate (recalculates coefficients)
    fn set_sample_rate(&mut self, sample_rate: f64) {
        if (self.geometry.sample_rate - sample_rate).abs() > 1.0 {
            self.geometry = BiquadGeometry::new(
                self.geometry.freq,
                self.geometry.q,
                sample_rate,
                self.filter_type,
            );
            // Recalculate with current gain (will be updated later)
            self.coeffs = match self.filter_type {
                FilterType::Peaking => Self::calc_peaking_coeffs(&self.geometry, 0.0),
                FilterType::LowShelf => Self::calc_low_shelf_coeffs(&self.geometry, 0.0),
                FilterType::HighShelf => Self::calc_high_shelf_coeffs(&self.geometry, 0.0),
            };
        }
    }
}

// ============================================================================
// Parameter Smoother
// ============================================================================

/// Exponential parameter smoother for click-free transitions
#[derive(Debug, Clone)]
struct ParameterSmoother {
    current: f64,
    target: f64,
    /// Smoothing coefficient per sample (exp(-1/tau))
    coeff: f64,
    /// Samples remaining to reach target (for block-based updates)
    samples_remaining: usize,
}

impl ParameterSmoother {
    /// Create a new smoother with time constant in milliseconds
    fn new(smoothing_time_ms: f64, sample_rate: f64) -> Self {
        let tau = (smoothing_time_ms / 1000.0) * sample_rate;
        let coeff = if tau > 0.0 { (-1.0 / tau).exp() } else { 0.0 };

        Self {
            current: 0.0,
            target: 0.0,
            coeff,
            samples_remaining: 0,
        }
    }

    /// Set target value
    fn set_target(&mut self, target: f64) {
        if (self.target - target).abs() > 0.0001 {
            self.target = target;
            self.samples_remaining = usize::MAX; // Start smoothing
        }
    }

    /// Get smoothed value for a block (call once per block)
    /// Returns the value at the end of the block
    fn next_block(&mut self, block_size: usize) -> f64 {
        if self.samples_remaining > 0 {
            // Apply smoothing for entire block at once
            // remaining_factor = coeff^block_size
            let remaining_factor = self.coeff.powi(block_size as i32);
            self.current = self.current + (self.target - self.current) * (1.0 - remaining_factor);

            if (self.current - self.target).abs() < 0.0001 {
                self.current = self.target;
                self.samples_remaining = 0;
            }
        }
        self.current
    }

    /// Reset to zero
    fn reset(&mut self) {
        self.current = 0.0;
        self.target = 0.0;
        self.samples_remaining = 0;
    }
}

// ============================================================================
// 7-Band Dynamic Loudness Compensation
// ============================================================================

/// ISO 226 inspired 7-band loudness compensation curve
///
/// Frequency bands and maximum boost at very low volume:
/// - 40 Hz:  +12 dB (deep bass)
/// - 100 Hz: +10 dB (bass fundamental)
/// - 300 Hz: +4 dB  (low-mids)
/// - 1 kHz:  0 dB   (reference, unchanged)
/// - 3 kHz:  +2 dB  (presence)
/// - 8 kHz:  +4 dB  (highs)
/// - 12 kHz: +6 dB  (air)
pub const LOUDNESS_BANDS: [(f64, f64, f64); 7] = [
    (40.0, 12.0, 0.0), // freq, max_gain_db, Q (0 = shelf)
    (100.0, 10.0, 0.9),
    (300.0, 4.0, 1.0),
    (1000.0, 0.0, 1.0), // Reference band (no boost)
    (3000.0, 2.0, 0.9),
    (8000.0, 4.0, 0.8),
    (12000.0, 6.0, 0.0), // High shelf
];

pub const LOUDNESS_BANDS_N: usize = 7;

/// Block size for coefficient updates (CPU optimization)
const BLOCK_SIZE: usize = 64;
const GAIN_UPDATE_EPSILON_DB: f64 = 0.01;
const BAND_ACTIVE_EPSILON_DB: f64 = 0.0001;

/// Dynamic Loudness Compensation processor
///
/// Implements ISO 226 inspired loudness compensation using a 7-band dynamic EQ.
/// At low volumes, boosts low and high frequencies to compensate for the
/// ear's reduced sensitivity (Fletcher-Munson effect).
pub struct DynamicLoudness {
    /// Per-channel filter banks
    filters: Vec<[BiquadFilter; LOUDNESS_BANDS_N]>,
    /// Per-band parameter smoothers
    smoothers: Vec<ParameterSmoother>,
    /// Last gain actually applied to each band coefficient set.
    last_applied_gains: [f64; LOUDNESS_BANDS_N],
    /// Whether each band currently has non-identity coefficients.
    active_bands: [bool; LOUDNESS_BANDS_N],
    /// Maximum boost per band (dB)
    max_gains: [f64; LOUDNESS_BANDS_N],
    /// Reference volume in dB (above this, no compensation)
    ref_volume_db: f64,
    /// Transition range in dB (from ref to max compensation)
    transition_db: f64,
    /// Cached linear pre-gain.
    pre_gain_linear: f64,
    /// Sample rate
    sample_rate: f64,
    /// Number of channels
    channels: usize,
    /// Current loudness factor (0.0 = full volume, 1.0 = max compensation)
    current_loudness_factor: f64,
    /// User strength multiplier (0.0 - 1.0)
    strength: f64,
    /// Enabled flag
    enabled: bool,
}

impl DynamicLoudness {
    /// Create a new DynamicLoudness processor
    pub fn new(channels: usize, sample_rate: f64) -> Self {
        let filters: Vec<[BiquadFilter; LOUDNESS_BANDS_N]> = (0..channels)
            .map(|_| Self::build_channel_filters(sample_rate))
            .collect();

        let smoothers: Vec<ParameterSmoother> = LOUDNESS_BANDS
            .iter()
            .map(|_| ParameterSmoother::new(50.0, sample_rate)) // 50ms smoothing
            .collect();

        let max_gains = LOUDNESS_BANDS.map(|(_, max_gain, _)| max_gain);

        Self {
            filters,
            smoothers,
            last_applied_gains: [f64::NAN; LOUDNESS_BANDS_N],
            active_bands: [false; LOUDNESS_BANDS_N],
            max_gains,
            ref_volume_db: -15.0, // Reference: ~50% perceived loudness
            transition_db: 25.0,  // Compensation starts below -15 dB, max at -40 dB
            // Headroom for bass boost (-3 dB).
            pre_gain_linear: 10.0_f64.powf(-3.0 / 20.0),
            sample_rate,
            channels,
            current_loudness_factor: 0.0,
            strength: 1.0,
            enabled: true,
        }
    }

    fn build_channel_filters(sample_rate: f64) -> [BiquadFilter; LOUDNESS_BANDS_N] {
        std::array::from_fn(|idx| {
            let (freq, _max_gain, q) = LOUDNESS_BANDS[idx];
            if q == 0.0 && freq < 1000.0 {
                BiquadFilter::low_shelf(freq, 0.0, sample_rate)
            } else if q == 0.0 {
                BiquadFilter::high_shelf(freq, 0.0, sample_rate)
            } else {
                BiquadFilter::peaking(freq, 0.0, q, sample_rate)
            }
        })
    }

    fn calculate_band_coeffs(&self, band: usize, gain_db: f64) -> BiquadCoeffs {
        let filter = &self.filters[0][band];
        match filter.filter_type {
            FilterType::Peaking => BiquadFilter::calc_peaking_coeffs(&filter.geometry, gain_db),
            FilterType::LowShelf => BiquadFilter::calc_low_shelf_coeffs(&filter.geometry, gain_db),
            FilterType::HighShelf => {
                BiquadFilter::calc_high_shelf_coeffs(&filter.geometry, gain_db)
            }
        }
    }

    fn apply_band_gain_if_changed(&mut self, band: usize, gain_db: f64) {
        let should_be_active = gain_db.abs() >= BAND_ACTIVE_EPSILON_DB;
        if (gain_db - self.last_applied_gains[band]).abs() < GAIN_UPDATE_EPSILON_DB
            && self.active_bands[band] == should_be_active
        {
            return;
        }

        let coeffs = self.calculate_band_coeffs(band, gain_db);
        for ch_filters in &mut self.filters {
            ch_filters[band].coeffs = coeffs;
        }
        self.last_applied_gains[band] = gain_db;
        self.active_bands[band] = should_be_active;
    }

    fn refresh_smoother_targets(&mut self) {
        for (i, smoother) in self.smoothers.iter_mut().enumerate() {
            let target_gain = self.max_gains[i] * self.current_loudness_factor * self.strength;
            smoother.set_target(target_gain);
        }
    }

    fn can_bypass_for_zero_strength(&self) -> bool {
        self.strength < 0.0001
            && self.active_bands.iter().all(|&active| !active)
            && self
                .smoothers
                .iter()
                .all(|smoother| smoother.samples_remaining == 0)
    }

    /// Set user volume as linear value (0.0 - 1.0)
    /// This is the main control input
    pub fn set_volume(&mut self, linear_volume: f64) {
        let volume_db = if linear_volume > 0.0 {
            20.0 * linear_volume.log10()
        } else {
            f64::NEG_INFINITY
        };

        self.update_loudness_factor(volume_db);
    }

    /// Set user volume as percentage (0 - 100)
    pub fn set_volume_percent(&mut self, percent: f64) {
        self.set_volume(percent / 100.0);
    }

    /// Set user volume as dB
    pub fn set_volume_db(&mut self, volume_db: f64) {
        self.update_loudness_factor(volume_db);
    }

    /// Update loudness factor based on volume
    fn update_loudness_factor(&mut self, volume_db: f64) {
        // Calculate loudness factor (0 at ref_volume, 1 at ref_volume - transition_db)
        let factor = if volume_db >= self.ref_volume_db {
            0.0
        } else {
            ((self.ref_volume_db - volume_db) / self.transition_db).min(1.0)
        };

        // Update if changed significantly
        if (self.current_loudness_factor - factor).abs() > 0.0001 {
            self.current_loudness_factor = factor;
            self.refresh_smoother_targets();
        }
    }

    /// Set strength (0.0 - 1.0, scales all compensation)
    pub fn set_strength(&mut self, strength: f64) {
        let strength = strength.clamp(0.0, 1.0);
        if (self.strength - strength).abs() > 0.0001 {
            self.strength = strength;
            self.refresh_smoother_targets();
        }
    }

    /// Set reference volume level in dB
    pub fn set_reference_volume_db(&mut self, ref_db: f64) {
        self.ref_volume_db = ref_db.clamp(-30.0, 0.0);
    }

    /// Set transition range in dB
    pub fn set_transition_db(&mut self, transition_db: f64) {
        self.transition_db = transition_db.clamp(10.0, 40.0);
    }

    /// Enable or disable processing
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled && !enabled {
            // Disabling: reset all filters
            for ch_filters in &mut self.filters {
                for filter in ch_filters {
                    filter.reset();
                }
            }
            for smoother in &mut self.smoothers {
                smoother.reset();
            }
            self.active_bands = [false; LOUDNESS_BANDS_N];
            self.last_applied_gains = [f64::NAN; LOUDNESS_BANDS_N];
        }
        self.enabled = enabled;
    }

    /// Update sample rate
    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        if (self.sample_rate - sample_rate).abs() > 1.0 {
            self.sample_rate = sample_rate;

            // Update all filters
            for ch_filters in &mut self.filters {
                for filter in ch_filters {
                    filter.set_sample_rate(sample_rate);
                }
            }
            self.last_applied_gains = [f64::NAN; LOUDNESS_BANDS_N];
            self.active_bands = [false; LOUDNESS_BANDS_N];

            // Update smoothers
            for smoother in &mut self.smoothers {
                *smoother = ParameterSmoother::new(50.0, sample_rate);
            }
        }
    }

    /// Process interleaved audio buffer
    ///
    /// Coefficient ramps now track the per-block smoother *within* the buffer:
    /// each `BLOCK_SIZE`-frame chunk advances the smoothers, applies any changed
    /// band gain, then filters only that chunk's frames. This avoids the
    /// end-of-buffer "zipper" that occurred when the whole buffer was filtered
    /// with only the final block's coefficients. Total smoother advancement is
    /// unchanged (Σ chunk_frames == frames), so end-of-buffer state matches the
    /// previous behavior; buffers ≤ BLOCK_SIZE are filtered as a single block.
    pub fn process(&mut self, buffer: &mut [f64]) {
        if !self.enabled || self.can_bypass_for_zero_strength() {
            return;
        }

        let frames = buffer.len() / self.channels;
        if frames == 0 {
            return;
        }

        // Interleave per-block coefficient updates with filtering so the gain
        // ramp is applied as it is computed.
        for chunk_start in (0..frames).step_by(BLOCK_SIZE) {
            let chunk_end = (chunk_start + BLOCK_SIZE).min(frames);
            let chunk_frames = chunk_end - chunk_start;

            for i in 0..self.smoothers.len() {
                let gain = self.smoothers[i].next_block(chunk_frames);
                self.apply_band_gain_if_changed(i, gain);
            }

            self.process_samples_range(buffer, chunk_start, chunk_end);
        }
    }

    /// Internal: apply pre-gain and active-band filtering to `[start_frame, end_frame)`.
    ///
    /// When no band is active the band loop is skipped, so this reduces to
    /// applying pre-gain only.
    fn process_samples_range(&mut self, buffer: &mut [f64], start_frame: usize, end_frame: usize) {
        for frame in start_frame..end_frame {
            for ch in 0..self.channels {
                let idx = frame * self.channels + ch;
                let mut sample = buffer[idx] * self.pre_gain_linear;

                let ch_filters = &mut self.filters[ch];
                for (band, filter) in ch_filters.iter_mut().enumerate() {
                    if self.active_bands[band] {
                        sample = filter.process(sample);
                    }
                }

                buffer[idx] = sample;
            }
        }
    }

    /// Reset all filter states
    pub fn reset(&mut self) {
        for ch_filters in &mut self.filters {
            for filter in ch_filters {
                filter.reset();
            }
        }
        for smoother in &mut self.smoothers {
            smoother.reset();
        }
        self.current_loudness_factor = 0.0;
        self.last_applied_gains = [f64::NAN; LOUDNESS_BANDS_N];
        self.active_bands = [false; LOUDNESS_BANDS_N];
    }

    /// Get current loudness factor (for display)
    pub fn loudness_factor(&self) -> f64 {
        self.current_loudness_factor
    }

    /// Get current band gains (for display/metering)
    pub fn get_band_gains(&self) -> [f64; LOUDNESS_BANDS_N] {
        let mut gains = [0.0; LOUDNESS_BANDS_N];
        for (i, smoother) in self.smoothers.iter().enumerate() {
            gains[i] = smoother.current;
        }
        gains
    }

    /// Check if enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get strength
    pub fn strength(&self) -> f64 {
        self.strength
    }
}

// ============================================================================
// Atomic State for Thread-Safe Control
// ============================================================================

/// Thread-safe state for DynamicLoudness control from UI thread
pub struct AtomicDynamicLoudnessState {
    /// Linear volume (0.0 - 1.0)
    pub volume: AtomicF32,
    /// Strength (0.0 - 1.0)
    pub strength: AtomicF32,
    /// Enabled flag
    pub enabled: AtomicBool,
}

impl AtomicDynamicLoudnessState {
    pub fn new() -> Self {
        Self {
            volume: AtomicF32::new(1.0),
            strength: AtomicF32::new(1.0),
            enabled: AtomicBool::new(true),
        }
    }

    /// Set volume (call from UI thread)
    pub fn set_volume(&self, volume: f32) {
        self.volume.store(volume.clamp(0.0, 1.0), Ordering::Relaxed);
    }

    /// Set strength (call from UI thread)
    pub fn set_strength(&self, strength: f32) {
        self.strength
            .store(strength.clamp(0.0, 1.0), Ordering::Relaxed);
    }

    /// Set enabled (call from UI thread)
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Sync to processor (call from audio thread)
    pub fn sync_to_processor(&self, processor: &mut DynamicLoudness) {
        let volume = self.volume.load(Ordering::Relaxed) as f64;
        let strength = self.strength.load(Ordering::Relaxed) as f64;
        let enabled = self.enabled.load(Ordering::Relaxed);

        processor.set_volume(volume);
        processor.set_strength(strength);
        processor.set_enabled(enabled);
    }
}

impl Default for AtomicDynamicLoudnessState {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_tracks_block_coefficient_ramp_within_buffer() {
        let make = || {
            let mut dl = DynamicLoudness::new(2, 44_100.0);
            dl.set_strength(1.0);
            dl.set_volume(0.05);
            dl
        };
        let frames = BLOCK_SIZE * 4 + 17;
        let input: Vec<f64> = (0..frames * 2)
            .map(|i| ((i as f64) * 0.013).sin() * 0.3)
            .collect();
        let mut whole = make();
        let mut wbuf = input.clone();
        whole.process(&mut wbuf);
        let mut chunked = make();
        let mut cbuf = input.clone();
        for cs in (0..frames).step_by(BLOCK_SIZE) {
            let ce = (cs + BLOCK_SIZE).min(frames);
            chunked.process(&mut cbuf[cs * 2..ce * 2]);
        }
        for (w, c) in wbuf.iter().zip(&cbuf) {
            assert!((w - c).abs() < 1e-9, "{} vs {}", w, c);
        }
    }

    fn legacy_peaking_coeffs(freq: f64, gain_db: f64, q: f64, sample_rate: f64) -> BiquadCoeffs {
        if gain_db.abs() < 0.0001 {
            return BiquadCoeffs::default();
        }

        let a = 10.0_f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        BiquadCoeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    fn legacy_low_shelf_coeffs(freq: f64, gain_db: f64, sample_rate: f64) -> BiquadCoeffs {
        if gain_db.abs() < 0.0001 {
            return BiquadCoeffs::default();
        }

        let a = 10.0_f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / std::f64::consts::SQRT_2;
        let beta = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + beta * sin_w0);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - beta * sin_w0);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + beta * sin_w0;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - beta * sin_w0;

        BiquadCoeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    fn legacy_high_shelf_coeffs(freq: f64, gain_db: f64, sample_rate: f64) -> BiquadCoeffs {
        if gain_db.abs() < 0.0001 {
            return BiquadCoeffs::default();
        }

        let a = 10.0_f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / std::f64::consts::SQRT_2;
        let beta = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + beta * sin_w0);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - beta * sin_w0);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + beta * sin_w0;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - beta * sin_w0;

        BiquadCoeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    fn assert_coeffs_bit_equal(actual: &BiquadCoeffs, expected: &BiquadCoeffs) {
        assert_eq!(actual.b0.to_bits(), expected.b0.to_bits(), "b0");
        assert_eq!(actual.b1.to_bits(), expected.b1.to_bits(), "b1");
        assert_eq!(actual.b2.to_bits(), expected.b2.to_bits(), "b2");
        assert_eq!(actual.a1.to_bits(), expected.a1.to_bits(), "a1");
        assert_eq!(actual.a2.to_bits(), expected.a2.to_bits(), "a2");
    }

    #[test]
    fn test_cached_geometry_coefficients_match_legacy_formulas() {
        let cases = [
            (FilterType::LowShelf, 40.0, 0.7, 12.0, 192_000.0),
            (FilterType::Peaking, 100.0, 0.9, -12.0, 44_100.0),
            (FilterType::Peaking, 3000.0, 0.9, 20.0, 48_000.0),
            (FilterType::HighShelf, 12000.0, 0.7, -20.0, 44_100.0),
        ];

        for (filter_type, freq, q, gain, sample_rate) in cases {
            let mut filter = match filter_type {
                FilterType::Peaking => BiquadFilter::peaking(freq, 0.0, q, sample_rate),
                FilterType::LowShelf => BiquadFilter::low_shelf(freq, 0.0, sample_rate),
                FilterType::HighShelf => BiquadFilter::high_shelf(freq, 0.0, sample_rate),
            };
            filter.set_gain_db(gain);

            let expected = match filter_type {
                FilterType::Peaking => legacy_peaking_coeffs(freq, gain, q, sample_rate),
                FilterType::LowShelf => legacy_low_shelf_coeffs(freq, gain, sample_rate),
                FilterType::HighShelf => legacy_high_shelf_coeffs(freq, gain, sample_rate),
            };
            assert_coeffs_bit_equal(&filter.coeffs, &expected);
        }
    }

    #[test]
    fn test_cached_geometry_rebuilds_on_sample_rate_change() {
        let mut filter = BiquadFilter::peaking(1000.0, 6.0, 1.0, 44_100.0);
        filter.set_sample_rate(96_000.0);
        filter.set_gain_db(6.0);

        let expected = legacy_peaking_coeffs(1000.0, 6.0, 1.0, 96_000.0);
        assert_coeffs_bit_equal(&filter.coeffs, &expected);
        assert_eq!(filter.geometry.sample_rate, 96_000.0);
    }

    #[test]
    fn test_cached_geometry_extreme_gains_stay_finite() {
        for gain in [-20.0, -12.0, 0.0, 12.0, 20.0] {
            for mut filter in [
                BiquadFilter::low_shelf(40.0, 0.0, 192_000.0),
                BiquadFilter::peaking(1000.0, 0.0, 1.0, 48_000.0),
                BiquadFilter::high_shelf(12000.0, 0.0, 44_100.0),
            ] {
                filter.set_gain_db(gain);
                assert!(filter.coeffs.b0.is_finite());
                assert!(filter.coeffs.b1.is_finite());
                assert!(filter.coeffs.b2.is_finite());
                assert!(filter.coeffs.a1.is_finite());
                assert!(filter.coeffs.a2.is_finite());
            }
        }
    }

    #[test]
    fn test_band_gain_update_uses_last_applied_epsilon() {
        let mut dl = DynamicLoudness::new(2, 48_000.0);

        dl.apply_band_gain_if_changed(0, GAIN_UPDATE_EPSILON_DB * 2.0);
        assert_eq!(dl.last_applied_gains[0], GAIN_UPDATE_EPSILON_DB * 2.0);

        dl.apply_band_gain_if_changed(0, GAIN_UPDATE_EPSILON_DB * 2.5);
        assert_eq!(dl.last_applied_gains[0], GAIN_UPDATE_EPSILON_DB * 2.0);

        dl.apply_band_gain_if_changed(0, GAIN_UPDATE_EPSILON_DB * 3.5);
        assert_eq!(dl.last_applied_gains[0], GAIN_UPDATE_EPSILON_DB * 3.5);
    }

    #[test]
    fn test_band_gain_update_broadcasts_coefficients_to_channels() {
        let mut dl = DynamicLoudness::new(2, 48_000.0);
        dl.apply_band_gain_if_changed(0, 3.0);

        let left = dl.filters[0][0].coeffs;
        let right = dl.filters[1][0].coeffs;
        assert_coeffs_bit_equal(&left, &right);
    }

    #[test]
    fn test_identity_bands_are_inactive_and_skipped() {
        let mut dl = DynamicLoudness::new(2, 48_000.0);
        dl.set_volume_db(-40.0);
        let mut buffer = vec![0.25; BLOCK_SIZE * 2];

        dl.process(&mut buffer);

        assert!(dl.active_bands[0]);
        assert!(!dl.active_bands[3]);
        assert_eq!(dl.filters[0][3].state.z1, 0.0);
        assert_eq!(dl.filters[0][3].state.z2, 0.0);
        assert_eq!(dl.filters[1][3].state.z1, 0.0);
        assert_eq!(dl.filters[1][3].state.z2, 0.0);
    }

    #[test]
    fn test_first_process_applies_band_activity_state() {
        let mut dl = DynamicLoudness::new(2, 48_000.0);
        dl.set_volume_db(-40.0);
        let mut buffer = vec![0.25; BLOCK_SIZE * 2];

        dl.process(&mut buffer);

        assert!(dl.last_applied_gains.iter().all(|gain| gain.is_finite()));
        assert!(!dl.active_bands[3]);
        assert!(dl
            .active_bands
            .iter()
            .enumerate()
            .any(|(band, &active)| band != 3 && active));
    }

    #[test]
    fn test_identity_path_applies_pregain_without_touching_filters() {
        let mut dl = DynamicLoudness::new(2, 48_000.0);
        dl.set_volume_db(-15.0);
        let input = vec![0.25, -0.5, 0.125, -0.25];
        let mut buffer = input.clone();

        dl.process(&mut buffer);

        for (actual, original) in buffer.iter().zip(input.iter()) {
            assert!((actual - original * dl.pre_gain_linear).abs() < 1.0e-12);
        }
        assert!(dl.active_bands.iter().all(|&active| !active));
        assert!(dl
            .filters
            .iter()
            .flatten()
            .all(|filter| filter.state.z1 == 0.0 && filter.state.z2 == 0.0));
    }

    #[test]
    fn test_strength_zero_lets_active_bands_decay_to_inactive() {
        let mut dl = DynamicLoudness::new(2, 48_000.0);
        dl.set_volume_db(-40.0);
        let mut buffer = vec![0.25; BLOCK_SIZE * 2];

        dl.process(&mut buffer);
        assert!(dl.active_bands[0]);

        dl.set_strength(0.0);
        dl.process(&mut buffer);
        assert!(
            dl.active_bands[0],
            "strength changes should not clear active filters before smoothing catches up"
        );

        for _ in 0..512 {
            dl.process(&mut buffer);
        }

        assert!(dl.active_bands.iter().all(|&active| !active));
        assert!(dl.get_band_gains().iter().all(|gain| gain.abs() < 0.0001));
    }

    #[test]
    fn test_biquad_peaking() {
        let mut filter = BiquadFilter::peaking(1000.0, 6.0, 1.0, 44100.0);

        // Process some samples
        let input = vec![0.5; 100];
        let mut output: Vec<f64> = Vec::new();

        for &sample in &input {
            output.push(filter.process(sample));
        }

        // Output should be boosted around the center frequency
        // At steady state, gain should be approximately 6 dB
        let steady_state = output.last().unwrap();
        assert!(steady_state > &0.5, "Peaking filter should boost");
    }

    #[test]
    fn test_loudness_factor_calculation() {
        let mut dl = DynamicLoudness::new(2, 44100.0);

        // At reference volume (-15 dB), factor should be 0
        dl.set_volume_db(-15.0);
        assert!((dl.loudness_factor() - 0.0).abs() < 0.01);

        // Below reference
        dl.set_volume_db(-25.0); // 10 dB below ref, transition is 25 dB
        assert!((dl.loudness_factor() - 0.4).abs() < 0.05);

        // Far below reference
        dl.set_volume_db(-50.0);
        assert!((dl.loudness_factor() - 1.0).abs() < 0.01);

        // Above reference
        dl.set_volume_db(-10.0);
        assert!((dl.loudness_factor() - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_strength_scaling() {
        let mut dl = DynamicLoudness::new(2, 44100.0);
        dl.set_strength(0.5);
        dl.set_volume_db(-40.0); // Max compensation
        let mut buffer = vec![0.25; BLOCK_SIZE * 2];
        dl.process(&mut buffer);

        // With 50% strength, max low shelf boost should be 6 dB (12 * 0.5)
        let gains = dl.get_band_gains();
        assert!(
            gains[0] > 0.0,
            "Expected smoother to start moving, got {}",
            gains[0]
        );
        assert!(
            gains[0] <= 6.0 + 0.1,
            "Expected gain to stay within target, got {}",
            gains[0]
        );
    }

    #[test]
    fn test_process_no_crash() {
        let mut dl = DynamicLoudness::new(2, 44100.0);
        dl.set_volume(0.1); // Low volume

        // Process some audio
        let mut buffer = vec![0.5; 1024];
        dl.process(&mut buffer);

        // Should not crash or produce NaN/Inf
        for &sample in &buffer {
            assert!(sample.is_finite());
        }
    }

    #[test]
    fn test_parameter_smoother() {
        let mut smoother = ParameterSmoother::new(50.0, 44100.0);

        smoother.set_target(10.0);

        // Should take some samples to reach target
        let mut current = 0.0_f64;
        for _ in 0..20000 {
            current = smoother.next_block(1);
        }

        // Should be close to target
        assert!((current - 10.0).abs() < 0.5);
    }

    #[test]
    fn test_disabled_bypass() {
        let mut dl = DynamicLoudness::new(2, 44100.0);
        dl.set_enabled(false);
        dl.set_volume(0.1);

        let input = vec![0.5; 100];
        let mut buffer = input.clone();
        dl.process(&mut buffer);

        // When disabled, output should equal input
        for (i, o) in input.iter().zip(buffer.iter()) {
            assert!((i - o).abs() < 0.0001);
        }
    }

    #[test]
    fn test_fixed_filter_banks_are_allocated_per_channel() {
        for channels in [1, 2, 6, 8] {
            let dl = DynamicLoudness::new(channels, 48_000.0);
            assert_eq!(dl.filters.len(), channels);
            assert!(dl.filters.iter().all(|bank| bank.len() == LOUDNESS_BANDS_N));
        }
    }

    #[test]
    fn test_reset_clears_all_filter_bank_state() {
        let mut dl = DynamicLoudness::new(2, 48_000.0);
        dl.set_volume(0.1);

        let mut buffer = vec![0.25; 256];
        dl.process(&mut buffer);

        assert!(dl
            .filters
            .iter()
            .flatten()
            .any(|filter| filter.state.z1 != 0.0 || filter.state.z2 != 0.0));

        dl.reset();

        assert!(dl
            .filters
            .iter()
            .flatten()
            .all(|filter| filter.state.z1 == 0.0 && filter.state.z2 == 0.0));
    }

    #[test]
    fn test_biquad_flushes_denormals_with_audio_thread_init() {
        crate::runtime::audio_thread_init();
        if !crate::runtime::audio_thread_float_mode_is_enabled() {
            return;
        }

        let mut filter = BiquadFilter::peaking(1000.0, 0.0, 1.0, 44100.0);
        let subnormal = f64::from_bits(1);
        filter.state.z1 = subnormal;
        filter.state.z2 = -subnormal;
        let _ = filter.process(0.0);
        assert_eq!(filter.state.z1, 0.0);
        assert_eq!(filter.state.z2, 0.0);
    }

    #[test]
    fn test_biquad_sustained_subnormal_input_flushes_to_zero() {
        crate::runtime::audio_thread_init();
        if !crate::runtime::audio_thread_float_mode_is_enabled() {
            return;
        }

        let mut filter = BiquadFilter::peaking(1000.0, 6.0, 1.0, 44100.0);
        let subnormal = f64::from_bits(1);

        for _ in 0..1024 {
            assert_eq!(filter.process(subnormal), 0.0);
            assert_eq!(filter.process(-subnormal), 0.0);
        }

        assert_eq!(filter.state.z1, 0.0);
        assert_eq!(filter.state.z2, 0.0);
    }
}
