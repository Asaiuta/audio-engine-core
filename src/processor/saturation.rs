//! Tube Saturation / Soft Clipping Processor
//!
//! Provides analog-style warmth through non-linear waveshaping.
//! Uses tanh-based soft clipping to add harmonics without harsh distortion.
//!
//! # Design
//!
//! - Threshold-based: only affects samples above threshold
//! - Tanh waveshaping: smooth, musical saturation curve
//! - Drive control: intensity of the effect
//! - Mix control: blend between dry and saturated signal
//! - High-pass mode: only saturate high frequencies (exciter mode)
//!
//! # Use Cases
//!
//! - Add warmth to digital recordings
//! - Restore transient energy lost in limiting
//! - Simulate analog console coloration
//! - High-frequency exciter for presence boost

/// Saturation type / character
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum SaturationType {
    #[default]
    Tape, // Warm, gentle compression
    Tube,       // Rich even harmonics
    Transistor, // Edgy, odd harmonics
}

/// Saturation processing quality.
///
/// `Direct` preserves the legacy source-rate waveshaper. The oversampled modes
/// spend bounded CPU on interpolated nonlinear processing plus fixed FIR
/// decimation to reduce high-frequency aliasing products.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SaturationQuality {
    #[default]
    Direct,
    Oversampled2x,
    Oversampled4x,
}

const OVERSAMPLING_MAX_FILTER_TAPS: usize = 33;
/// Width of the C1 transition from identity to the selected waveshaper.
///
/// A fixed 5% full-scale knee keeps the threshold local while remaining
/// continuous even when callers intentionally process samples above 0 dBFS.
const SATURATION_SOFT_KNEE_WIDTH: f64 = 0.05;
const OVERSAMPLING_2X_FILTER: [f64; 17] = [
    5.251027135038586e-19,
    -3.0197042000540625e-4,
    2.851588800210736e-3,
    7.746002998672272e-3,
    -1.590176357619914e-2,
    -5.244242465496063e-2,
    3.804086149277052e-2,
    2.950296948494157e-1,
    4.499560210201921e-1,
    2.950296948494157e-1,
    3.804086149277053e-2,
    -5.244242465496067e-2,
    -1.5901763576199147e-2,
    7.746002998672269e-3,
    2.8515888002107396e-3,
    -3.019704200054068e-4,
    5.251027135038586e-19,
];
const OVERSAMPLING_4X_FILTER: [f64; OVERSAMPLING_MAX_FILTER_TAPS] = [
    2.625498272061368e-19,
    -6.89589757263199e-5,
    -1.5098433040796195e-4,
    1.9935259364948403e-4,
    1.4257860938530118e-3,
    3.2191159563376977e-3,
    3.872978936386087e-3,
    6.89620329927397e-4,
    -7.950835468636832e-3,
    -1.961391240495165e-2,
    -2.622105957052836e-2,
    -1.6252179284702004e-2,
    1.9020319939036252e-2,
    7.836872883124728e-2,
    1.4751398804726262e-1,
    2.034596893795545e-1,
    2.2497669985539734e-1,
    2.034596893795545e-1,
    1.4751398804726262e-1,
    7.836872883124729e-2,
    1.9020319939036256e-2,
    -1.6252179284702004e-2,
    -2.622105957052838e-2,
    -1.961391240495166e-2,
    -7.950835468636836e-3,
    6.89620329927397e-4,
    3.872978936386086e-3,
    3.219115956337702e-3,
    1.4257860938530135e-3,
    1.9935259364948398e-4,
    -1.5098433040796222e-4,
    -6.895897572632045e-5,
    2.625498272061368e-19,
];

impl SaturationQuality {
    #[inline]
    fn ratio(self) -> usize {
        match self {
            Self::Direct => 1,
            Self::Oversampled2x => 2,
            Self::Oversampled4x => 4,
        }
    }

    #[inline]
    fn decimation_filter(self) -> &'static [f64] {
        match self {
            Self::Direct => &[],
            Self::Oversampled2x => &OVERSAMPLING_2X_FILTER,
            Self::Oversampled4x => &OVERSAMPLING_4X_FILTER,
        }
    }
}

#[derive(Clone)]
struct OversamplingChannelState {
    previous_input: f64,
    initialized: bool,
    filter_history: [f64; OVERSAMPLING_MAX_FILTER_TAPS],
    filter_index: usize,
}

impl Default for OversamplingChannelState {
    fn default() -> Self {
        Self {
            previous_input: 0.0,
            initialized: false,
            filter_history: [0.0; OVERSAMPLING_MAX_FILTER_TAPS],
            filter_index: 0,
        }
    }
}

impl OversamplingChannelState {
    fn reset(&mut self) {
        self.previous_input = 0.0;
        self.initialized = false;
        self.filter_history.fill(0.0);
        self.filter_index = 0;
    }

    fn initialize(&mut self, input: f64, filtered_value: f64) {
        self.previous_input = input;
        self.initialized = true;
        self.filter_history.fill(filtered_value);
        self.filter_index = 0;
    }

    #[inline]
    fn lowpass(&mut self, sample: f64, coefficients: &[f64]) -> f64 {
        if coefficients.is_empty() {
            return sample;
        }

        let len = coefficients.len();
        self.filter_history[self.filter_index] = sample;

        let mut acc = 0.0;
        let mut history_index = self.filter_index;
        for &coefficient in coefficients {
            acc += coefficient * self.filter_history[history_index];
            history_index = if history_index == 0 {
                len - 1
            } else {
                history_index - 1
            };
        }

        self.filter_index += 1;
        if self.filter_index == len {
            self.filter_index = 0;
        }

        acc
    }
}

/// Tube Saturation processor with configurable drive and mix
///
/// When highpass_mode is enabled, only high frequencies (>4kHz) are saturated,
/// creating a more transparent "exciter" effect without muddying the low end.
///
/// Configuration is done through the `set_*` methods; current values can be read
/// back with [`Saturation::get_settings`]. For shared mutable access from another
/// thread, wrap this in `Arc<Mutex<Saturation>>`.
pub struct Saturation {
    /// Saturation type
    sat_type: SaturationType,
    /// Processing quality / antialiasing mode.
    quality: SaturationQuality,
    /// Drive amount (0.0 - 2.0, default 0.25)
    drive: f64,
    /// Threshold where saturation begins (linear, default 0.88)
    threshold: f64,
    /// Mix between dry and wet (0.0 - 1.0, default 0.2)
    mix: f64,
    /// Input gain (dB, applied before saturation, default 0.0)
    input_gain_db: f64,
    /// Output gain compensation (dB, default 0.0)
    output_gain_db: f64,
    /// Cached linear input gain.
    input_gain_linear: f64,
    /// Cached linear output gain.
    output_gain_linear: f64,
    /// Enable/disable
    enabled: bool,

    // High-pass mode for exciter functionality
    /// Enable high-pass separation (only saturate highs)
    highpass_mode: bool,
    /// HPF cutoff frequency in Hz (default: 4000)
    highpass_cutoff: f64,

    // Sample rate for HPF coefficient calculation
    sample_rate: f64,
    // Cached HPF coefficient (recalculated when sample_rate or cutoff changes)
    hpf_coef: f64,

    // P1-5 fix: Per-channel HPF state (supports arbitrary channel count, not just stereo)
    /// HPF filter state per channel (y[n-1])
    hpf_states: Vec<f64>,
    /// Previous input per channel (x[n-1])
    prev_inputs: Vec<f64>,
    /// Per-channel oversampling state, pre-sized during setup.
    oversampling_states: Vec<OversamplingChannelState>,
}

impl Saturation {
    /// Create a new saturation processor with default settings
    pub fn new() -> Self {
        let mut instance = Self {
            sat_type: SaturationType::Tube,
            quality: SaturationQuality::Direct,
            drive: 0.25,
            threshold: 0.88,
            mix: 0.2,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            input_gain_linear: 1.0,
            output_gain_linear: 1.0,
            enabled: true,
            highpass_mode: false,
            highpass_cutoff: 4000.0,
            sample_rate: 44100.0,
            hpf_coef: 0.0, // Will be calculated below
            // P1-5 fix: Initialize for 2 channels by default, grows on demand
            hpf_states: vec![0.0; 2],
            prev_inputs: vec![0.0; 2],
            oversampling_states: vec![OversamplingChannelState::default(); 2],
        };
        // Initialize HPF coefficient immediately (fixes MINOR-03)
        instance.update_hpf_coef();
        instance
    }

    /// Create with specific saturation type
    pub fn with_type(sat_type: SaturationType) -> Self {
        Self {
            sat_type,
            ..Self::new()
        }
    }

    /// Set drive amount (0.0 - 2.0)
    pub fn set_drive(&mut self, drive: f64) {
        self.drive = drive.clamp(0.0, 2.0);
    }

    /// Set threshold (0.0 - 1.0)
    pub fn set_threshold(&mut self, threshold: f64) {
        self.threshold = threshold.clamp(0.0, 1.0);
    }

    /// Set mix amount (0.0 - 1.0)
    pub fn set_mix(&mut self, mix: f64) {
        self.mix = mix.clamp(0.0, 1.0);
    }

    /// Set input gain (dB) - applied before saturation
    pub fn set_input_gain(&mut self, gain_db: f64) {
        self.input_gain_db = gain_db;
        self.input_gain_linear = db_to_linear(gain_db);
    }

    /// Set output gain (dB), applied after the dry/wet saturation blend.
    pub fn set_output_gain(&mut self, gain_db: f64) {
        self.output_gain_db = gain_db;
        self.output_gain_linear = db_to_linear(gain_db);
    }

    /// Enable/disable saturation
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Set saturation type
    pub fn set_type(&mut self, sat_type: SaturationType) {
        self.sat_type = sat_type;
    }

    /// Set processing quality / antialiasing mode.
    pub fn set_quality(&mut self, quality: SaturationQuality) {
        if self.quality != quality {
            self.quality = quality;
            self.reset_oversampling_states();
        }
    }

    /// Enable/disable high-pass mode (exciter mode)
    pub fn set_highpass_mode(&mut self, enabled: bool) {
        if self.highpass_mode != enabled {
            self.reset_oversampling_states();
        }
        self.highpass_mode = enabled;
    }

    /// Set high-pass cutoff frequency in Hz
    pub fn set_highpass_cutoff(&mut self, hz: f64) {
        self.highpass_cutoff = hz.clamp(1000.0, 12000.0);
        self.update_hpf_coef();
    }

    /// Update sample rate and recalculate HPF coefficient
    pub fn set_sample_rate(&mut self, sr: f64) {
        self.sample_rate = sr;
        self.update_hpf_coef();
    }

    /// Pre-size the per-channel HPF state for `channels`, off the audio thread.
    ///
    /// Call this during setup (when the processor is built for a stream) so
    /// `process_highpass` never resizes `hpf_states`/`prev_inputs` on the realtime
    /// audio thread. Defaults keep the stereo size when `channels == 0`.
    pub fn set_channel_count(&mut self, channels: usize) {
        let channels = channels.max(1);
        if self.hpf_states.len() != channels {
            self.hpf_states.resize(channels, 0.0);
            self.prev_inputs.resize(channels, 0.0);
            self.oversampling_states
                .resize(channels, OversamplingChannelState::default());
        }
    }

    fn reset_oversampling_states(&mut self) {
        for state in &mut self.oversampling_states {
            state.reset();
        }
    }

    /// Recalculate HPF coefficient based on current cutoff and sample rate
    fn update_hpf_coef(&mut self) {
        // Correct first-order RC HPF: α = fs / (fs + 2π·fc)
        // For difference equation y[n] = α·y[n-1] + α·(x[n] - x[n-1])
        // α close to 1.0 = low cutoff (passes more), α close to 0.0 = high cutoff
        self.hpf_coef =
            self.sample_rate / (self.sample_rate + std::f64::consts::TAU * self.highpass_cutoff);
    }

    /// Process interleaved f64 samples in-place
    pub fn process(&mut self, samples: &mut [f64]) {
        self.process_with_channels(samples, 2) // Default to stereo
    }

    /// Process interleaved f64 samples with specified channel count
    pub fn process_with_channels(&mut self, samples: &mut [f64], channels: usize) {
        if !self.enabled {
            return;
        }

        if self.highpass_mode {
            self.process_highpass(samples, channels);
        } else {
            self.process_fullband(samples, channels);
        }
    }

    /// Process with explicit sample rate (for cases where SR differs from cached value)
    pub fn process_with_sr(&mut self, samples: &mut [f64], channels: usize, sample_rate: f64) {
        if (self.sample_rate - sample_rate).abs() > 1.0 {
            self.set_sample_rate(sample_rate);
        }
        self.process_with_channels(samples, channels);
    }

    /// Full-band saturation (original behavior)
    fn process_fullband(&mut self, samples: &mut [f64], channels: usize) {
        if self.quality != SaturationQuality::Direct {
            self.process_fullband_oversampled(samples, channels.max(1));
            return;
        }

        let input_gain = self.input_gain_linear;
        let output_gain = self.output_gain_linear;
        let threshold = self.threshold;
        let drive_plus1 = 1.0 + self.drive;
        let mix = self.mix;
        let sat_type = self.sat_type;

        for sample in samples.iter_mut() {
            let dry = *sample * input_gain;
            let wet = Self::apply_thresholded_saturation(sat_type, dry, threshold, drive_plus1);
            *sample = (dry + (wet - dry) * mix) * output_gain;
        }
    }

    fn process_fullband_oversampled(&mut self, samples: &mut [f64], channels: usize) {
        debug_assert!(
            self.oversampling_states.len() >= channels,
            "Saturation oversampling state undersized for {} channels (have {}); call set_channel_count during setup",
            channels,
            self.oversampling_states.len()
        );
        if self.oversampling_states.len() < channels {
            return;
        }

        let input_gain = self.input_gain_linear;
        let output_gain = self.output_gain_linear;
        let threshold = self.threshold;
        let drive_plus1 = 1.0 + self.drive;
        let mix = self.mix;
        let sat_type = self.sat_type;
        let ratio = self.quality.ratio();
        let filter = self.quality.decimation_filter();

        for (index, sample) in samples.iter_mut().enumerate() {
            let dry = *sample * input_gain;
            let state = &mut self.oversampling_states[index % channels];
            let wet = Self::process_oversampled_value(
                state,
                dry,
                ratio,
                filter,
                sat_type,
                threshold,
                drive_plus1,
            );

            *sample = (dry + (wet - dry) * mix) * output_gain;
        }
    }

    /// High-pass separated saturation (exciter mode)
    /// Only saturates frequencies above the cutoff.
    /// P1-5 fix: Supports arbitrary channel count (was hardcoded to L/R only).
    fn process_highpass(&mut self, samples: &mut [f64], channels: usize) {
        let input_gain = self.input_gain_linear;
        let output_gain = self.output_gain_linear;
        let alpha = self.hpf_coef;
        let threshold = self.threshold;
        let drive_plus1 = 1.0 + self.drive;
        let mix = self.mix;
        let sat_type = self.sat_type;

        // HPF state is sized off the audio thread via `set_channel_count`; never
        // resize here, which would allocate on the realtime audio thread. If this
        // fires, a caller processed more channels than it was set up for.
        debug_assert!(
            self.hpf_states.len() >= channels,
            "Saturation HPF state undersized for {} channels (have {}); call set_channel_count during setup",
            channels,
            self.hpf_states.len()
        );
        debug_assert!(
            self.oversampling_states.len() >= channels,
            "Saturation oversampling state undersized for {} channels (have {}); call set_channel_count during setup",
            channels,
            self.oversampling_states.len()
        );
        if self.hpf_states.len() < channels
            || self.prev_inputs.len() < channels
            || self.oversampling_states.len() < channels
        {
            return;
        }

        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                if idx >= samples.len() {
                    break;
                }

                let input = samples[idx] * input_gain;

                // First-order HPF: y[n] = α·y[n-1] + α·(x[n] - x[n-1])
                let high = alpha * self.hpf_states[ch] + alpha * (input - self.prev_inputs[ch]);
                self.hpf_states[ch] = high;
                self.prev_inputs[ch] = input;
                #[cfg(not(any(
                    target_arch = "x86",
                    target_arch = "x86_64",
                    target_arch = "aarch64"
                )))]
                {
                    self.hpf_states[ch] =
                        crate::runtime::flush_subnormal_sample(self.hpf_states[ch]);
                    self.prev_inputs[ch] =
                        crate::runtime::flush_subnormal_sample(self.prev_inputs[ch]);
                }

                // Apply saturation to high frequencies only.
                let saturated_high = if self.quality == SaturationQuality::Direct {
                    Self::apply_thresholded_saturation(sat_type, high, threshold, drive_plus1)
                } else {
                    Self::process_oversampled_value(
                        &mut self.oversampling_states[ch],
                        high,
                        self.quality.ratio(),
                        self.quality.decimation_filter(),
                        sat_type,
                        threshold,
                        drive_plus1,
                    )
                };

                // Mix: input + (saturated_high - high) * mix
                samples[idx] = (input + (saturated_high - high) * mix) * output_gain;
            }
        }
    }

    #[inline(always)]
    fn apply_saturation_type(sat_type: SaturationType, x: f64) -> f64 {
        match sat_type {
            SaturationType::Tape => x.signum() * (1.0 - (-x.abs()).exp()),
            SaturationType::Tube => x.tanh(),
            SaturationType::Transistor => {
                // Piecewise cubic: x - x³/3 for |x| ≤ 1.5, then smoothly limited
                // Fix discontinuity: clamp to value at boundary (1.5 - 1.5³/3 = 0.375)
                if x.abs() <= 1.5 {
                    x - (x * x * x) / 3.0
                } else {
                    x.signum() * 0.375
                }
            }
        }
    }

    #[inline(always)]
    fn apply_thresholded_saturation(
        sat_type: SaturationType,
        input: f64,
        threshold: f64,
        drive_plus1: f64,
    ) -> f64 {
        let excess = input.abs() - threshold;
        if excess <= 0.0 {
            return input;
        }

        let shaped = Self::apply_saturation_type(sat_type, input * drive_plus1);
        let position = (excess / SATURATION_SOFT_KNEE_WIDTH).min(1.0);
        let weight = position * position * (3.0 - 2.0 * position);
        input + (shaped - input) * weight
    }

    #[inline]
    fn process_oversampled_value(
        state: &mut OversamplingChannelState,
        input: f64,
        ratio: usize,
        filter: &[f64],
        sat_type: SaturationType,
        threshold: f64,
        drive_plus1: f64,
    ) -> f64 {
        let first_wet = Self::apply_thresholded_saturation(sat_type, input, threshold, drive_plus1);
        if !state.initialized {
            state.initialize(input, first_wet);
            return first_wet;
        }

        let previous = state.previous_input;
        let delta = input - previous;
        let mut output = first_wet;
        for phase in 1..=ratio {
            let t = phase as f64 / ratio as f64;
            let interpolated = previous + delta * t;
            let wet =
                Self::apply_thresholded_saturation(sat_type, interpolated, threshold, drive_plus1);
            output = state.lowpass(wet, filter);
        }
        state.previous_input = input;
        output
    }

    /// Reset filter state
    pub fn reset(&mut self) {
        self.hpf_states.fill(0.0);
        self.prev_inputs.fill(0.0);
        self.reset_oversampling_states();
    }

    /// Get current settings as a struct
    pub fn get_settings(&self) -> SaturationSettings {
        SaturationSettings {
            sat_type: self.sat_type,
            quality: self.quality,
            drive: self.drive,
            threshold: self.threshold,
            mix: self.mix,
            input_gain_db: self.input_gain_db,
            output_gain_db: self.output_gain_db,
            enabled: self.enabled,
            highpass_mode: self.highpass_mode,
            highpass_cutoff: self.highpass_cutoff,
        }
    }
}

impl Default for Saturation {
    fn default() -> Self {
        Self::new()
    }
}

/// Settings struct for API responses
#[derive(Debug, Clone, serde::Serialize)]
pub struct SaturationSettings {
    pub sat_type: SaturationType,
    pub quality: SaturationQuality,
    pub drive: f64,
    pub threshold: f64,
    pub mix: f64,
    pub input_gain_db: f64,
    pub output_gain_db: f64,
    pub enabled: bool,
    pub highpass_mode: bool,
    pub highpass_cutoff: f64,
}

// P1-4 fix: Use centralized db_to_linear from dsp module instead of local duplicate
use super::dsp::db_to_linear;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tube_saturation() {
        let mut sat = Saturation::with_type(SaturationType::Tube);
        sat.set_enabled(true);
        sat.set_mix(1.0); // 100% wet for testing

        // Test that loud signals are compressed
        let mut samples = vec![0.9, -0.9, 0.5, -0.5];
        sat.process(&mut samples);

        // tanh(0.9) ≈ 0.716
        assert!(samples[0].abs() < 0.9);
        assert!(samples[1].abs() < 0.9);

        // Lower signals should pass through relatively unchanged
        // tanh(0.5) ≈ 0.462, which is close to 0.5
        assert!((samples[2].abs() - 0.5).abs() < 0.1);
    }

    #[test]
    fn test_disabled() {
        let mut sat = Saturation::new();
        sat.set_enabled(false);

        let mut samples = vec![0.9, -0.9, 0.5, -0.5];
        sat.process(&mut samples);

        // Should pass through unchanged when disabled
        assert!((samples[0] - 0.9).abs() < 1e-10);
        assert!((samples[1] - (-0.9)).abs() < 1e-10);
    }

    #[test]
    fn test_cached_linear_gains_update_with_db_setters() {
        let mut sat = Saturation::new();

        sat.set_input_gain(6.0);
        sat.set_output_gain(-3.0);

        assert!((sat.input_gain_linear - db_to_linear(6.0)).abs() < 1e-12);
        assert!((sat.output_gain_linear - db_to_linear(-3.0)).abs() < 1e-12);
        assert_eq!(sat.input_gain_db, 6.0);
        assert_eq!(sat.output_gain_db, -3.0);
    }

    #[test]
    fn test_threshold() {
        let mut sat = Saturation::with_type(SaturationType::Tube);
        sat.set_enabled(true);
        sat.set_threshold(0.8);
        sat.set_mix(1.0);

        // Below threshold should pass unchanged
        let mut samples = vec![0.5];
        sat.process(&mut samples);
        assert!((samples[0] - 0.5).abs() < 1e-10);

        // Above threshold should be saturated
        let mut samples = vec![0.9];
        sat.process(&mut samples);
        assert!(samples[0].abs() < 0.9);
    }

    fn transfer_at(
        sat_type: SaturationType,
        threshold: f64,
        drive: f64,
        output_gain_db: f64,
        input: f64,
    ) -> f64 {
        let mut saturation = Saturation::with_type(sat_type);
        saturation.set_threshold(threshold);
        saturation.set_drive(drive);
        saturation.set_mix(1.0);
        saturation.set_output_gain(output_gain_db);
        let mut sample = [input];
        saturation.process_with_channels(&mut sample, 1);
        sample[0]
    }

    #[test]
    fn threshold_transfer_is_c1_for_every_saturation_type() {
        let threshold = 0.8;
        let epsilon = 1.0e-6;
        let expected_slope = db_to_linear(-3.0);

        for sat_type in [
            SaturationType::Tape,
            SaturationType::Tube,
            SaturationType::Transistor,
        ] {
            for sign in [-1.0, 1.0] {
                let center = sign * threshold;
                let outside = center + sign * epsilon;
                let inside = center - sign * epsilon;
                let center_out = transfer_at(sat_type, threshold, 1.3, -3.0, center);
                let outside_out = transfer_at(sat_type, threshold, 1.3, -3.0, outside);
                let inside_out = transfer_at(sat_type, threshold, 1.3, -3.0, inside);
                let jump = (outside_out - inside_out).abs();
                let inside_slope = (center_out - inside_out) / (center - inside);
                let outside_slope = (outside_out - center_out) / (outside - center);

                assert!(
                    jump <= 2.0e-6,
                    "{sat_type:?} sign={sign} threshold jump {jump:e}"
                );
                assert!(
                    (inside_slope - expected_slope).abs() <= 1.0e-9,
                    "{sat_type:?} sign={sign} inside slope {inside_slope:e}"
                );
                assert!(
                    (outside_slope - inside_slope).abs() <= 1.0e-3,
                    "{sat_type:?} sign={sign} slope mismatch inside={inside_slope:e} outside={outside_slope:e}"
                );
            }
        }
    }

    #[test]
    fn output_gain_is_consistent_below_and_above_threshold() {
        let mut saturation = Saturation::with_type(SaturationType::Tube);
        saturation.set_threshold(0.8);
        saturation.set_mix(1.0);
        saturation.set_output_gain(-6.0);
        let mut samples = [0.5, 0.8, 0.9];

        saturation.process_with_channels(&mut samples, 1);

        let output_gain = db_to_linear(-6.0);
        assert!((samples[0] - 0.5 * output_gain).abs() <= 1.0e-12);
        assert!((samples[1] - 0.8 * output_gain).abs() <= 1.0e-12);
        assert!(samples[2] < 0.9 * output_gain);
    }

    #[test]
    fn test_mix() {
        let mut sat = Saturation::with_type(SaturationType::Tube);
        sat.set_enabled(true);
        sat.set_drive(0.0); // No drive for this test
        sat.set_mix(0.5);

        let mut samples = vec![1.0];
        sat.process(&mut samples);

        // Mix of tanh(1) ≈ 0.762 and 1.0
        // Result should be between the two
        let expected = (1.0 + 1.0_f64.tanh()) * 0.5;
        assert!((samples[0] - expected).abs() < 0.01);
    }

    #[test]
    fn test_hpf_coefficient() {
        let mut sat = Saturation::new();
        sat.set_sample_rate(44100.0);
        sat.set_highpass_cutoff(4000.0);

        // Correct HPF coefficient: fs/(fs + 2π*fc) ≈ 0.637 (old) -> 0.637 (same formula value)
        // Actually: 44100 / (44100 + 2π*4000) = 44100 / 69231.9 ≈ 0.637
        // Wait - the old formula 1/(1 + 2π*fc/fs) = 1/(1 + 2π*4000/44100) = 1/(1.5697) = 0.6371
        // The new formula fs/(fs + 2π*fc) = 44100/(44100 + 25131.9) = 44100/69231.9 = 0.6371
        // These are algebraically identical! The fix is about the comment and usage context.
        let expected = 44100.0 / (44100.0 + std::f64::consts::TAU * 4000.0);
        assert!((sat.hpf_coef - expected).abs() < 0.001);
    }

    #[test]
    fn test_hpf_dc_rejection() {
        let mut sat = Saturation::new();
        sat.set_highpass_mode(true);
        sat.set_highpass_cutoff(4000.0);
        sat.set_sample_rate(44100.0);
        sat.set_mix(0.5); // With mix
        sat.set_threshold(2.0); // Don't trigger saturation

        // DC signal - HPF should reject DC, so high component → 0
        // Output should be close to input (low freq passes through)
        let mut samples: Vec<f64> = vec![0.0; 200]; // 100 stereo samples
        for i in 0..100 {
            samples[i * 2] = 1.0; // L = 1.0 (DC)
            samples[i * 2 + 1] = 1.0; // R = 1.0 (DC)
        }
        sat.process_with_channels(&mut samples, 2);

        // For DC input: high freq → 0, low freq ≈ input
        // Output ≈ input because low passes through and high is near 0
        // After initial transient, output should be close to DC input (1.0)
        let last_l: f64 = samples.iter().skip(180).step_by(2).take(10).sum::<f64>() / 10.0;
        let last_r: f64 = samples.iter().skip(181).step_by(2).take(10).sum::<f64>() / 10.0;

        // DC should pass through (high freq blocked, low freq = DC)
        assert!(
            (last_l - 1.0).abs() < 0.1,
            "L output should be close to 1.0, got {}",
            last_l
        );
        assert!(
            (last_r - 1.0).abs() < 0.1,
            "R output should be close to 1.0, got {}",
            last_r
        );
    }

    #[test]
    fn test_highpass_flushes_denormals_with_audio_thread_init() {
        crate::runtime::audio_thread_init();
        if !crate::runtime::audio_thread_float_mode_is_enabled() {
            return;
        }

        let mut sat = Saturation::new();
        sat.set_highpass_mode(true);
        let subnormal = f64::from_bits(1);
        sat.hpf_states[0] = subnormal;
        sat.prev_inputs[0] = -subnormal;
        let mut samples = vec![0.0, 0.0];
        sat.process_with_channels(&mut samples, 2);
        assert_eq!(sat.hpf_states[0], 0.0);
        assert_eq!(sat.prev_inputs[0], 0.0);
    }

    #[test]
    fn test_highpass_multichannel_after_set_channel_count_does_not_panic() {
        let mut sat = Saturation::new();
        sat.set_highpass_mode(true);
        sat.set_channel_count(6);

        // 6-channel interleaved buffer (8 frames). Before the fix this would
        // resize hpf_states/prev_inputs on the (would-be) audio thread; now the
        // state is pre-sized and process_highpass must not resize.
        let mut samples = vec![0.5; 6 * 8];
        sat.process_with_channels(&mut samples, 6);

        assert_eq!(sat.hpf_states.len(), 6);
        assert_eq!(sat.prev_inputs.len(), 6);
    }

    #[test]
    fn test_set_channel_count_resizes_state_off_rt() {
        let mut sat = Saturation::new();
        assert_eq!(sat.hpf_states.len(), 2);
        sat.set_channel_count(8);
        assert_eq!(sat.hpf_states.len(), 8);
        assert_eq!(sat.prev_inputs.len(), 8);
        assert_eq!(sat.oversampling_states.len(), 8);
        // Zero channels falls back to a mono-safe size rather than emptying state.
        sat.set_channel_count(0);
        assert_eq!(sat.hpf_states.len(), 1);
        assert_eq!(sat.oversampling_states.len(), 1);
    }

    #[test]
    fn test_quality_modes_cover_all_saturation_types() {
        let qualities = [
            SaturationQuality::Direct,
            SaturationQuality::Oversampled2x,
            SaturationQuality::Oversampled4x,
        ];
        let types = [
            SaturationType::Tape,
            SaturationType::Tube,
            SaturationType::Transistor,
        ];

        for quality in qualities {
            for sat_type in types {
                let mut sat = Saturation::with_type(sat_type);
                sat.set_quality(quality);
                sat.set_channel_count(2);
                sat.set_threshold(0.0);
                sat.set_drive(1.0);
                sat.set_mix(1.0);

                let mut samples = vec![0.8, -0.8, 0.2, -0.2, 0.9, -0.9];
                let original = samples.clone();
                sat.process_with_channels(&mut samples, 2);

                assert!(
                    samples.iter().all(|sample| sample.is_finite()),
                    "{quality:?}/{sat_type:?} produced non-finite output: {samples:?}"
                );
                assert!(
                    samples
                        .iter()
                        .zip(original.iter())
                        .any(|(processed, input)| (processed - input).abs() > 1.0e-6),
                    "{quality:?}/{sat_type:?} should process the waveform"
                );
                assert!(
                    samples.iter().all(|sample| sample.abs() <= 1.2),
                    "{quality:?}/{sat_type:?} should remain bounded: {samples:?}"
                );
            }
        }
    }

    #[test]
    fn test_oversampled_highpass_multichannel_after_setup() {
        let mut sat = Saturation::new();
        sat.set_quality(SaturationQuality::Oversampled4x);
        sat.set_highpass_mode(true);
        sat.set_channel_count(6);
        sat.set_threshold(0.0);
        sat.set_drive(1.2);
        sat.set_mix(0.75);

        let mut samples = vec![0.4; 6 * 16];
        sat.process_with_channels(&mut samples, 6);

        assert_eq!(sat.hpf_states.len(), 6);
        assert_eq!(sat.prev_inputs.len(), 6);
        assert_eq!(sat.oversampling_states.len(), 6);
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn test_oversampled_reset_clears_state() {
        let mut sat = Saturation::new();
        sat.set_quality(SaturationQuality::Oversampled2x);
        sat.set_channel_count(2);
        sat.set_threshold(0.0);
        let mut samples = vec![0.8, -0.8, 0.7, -0.7];

        sat.process_with_channels(&mut samples, 2);
        assert!(sat
            .oversampling_states
            .iter()
            .all(|state| state.initialized));

        sat.reset();

        assert!(sat
            .oversampling_states
            .iter()
            .all(|state| !state.initialized));
        assert!(sat
            .oversampling_states
            .iter()
            .all(|state| state.filter_history.iter().all(|sample| *sample == 0.0)));
    }

    #[test]
    fn test_oversampled_sample_rate_change_remains_bounded() {
        let mut sat = Saturation::new();
        sat.set_quality(SaturationQuality::Oversampled4x);
        sat.set_highpass_mode(true);
        sat.set_channel_count(2);
        sat.set_sample_rate(96_000.0);
        sat.set_highpass_cutoff(8_000.0);
        sat.set_threshold(0.0);
        sat.set_drive(1.0);

        let mut samples = vec![0.0; 512 * 2];
        for frame in 0..512 {
            let sample = (std::f64::consts::TAU * frame as f64 / 8.0).sin() * 0.8;
            samples[frame * 2] = sample;
            samples[frame * 2 + 1] = -sample;
        }

        sat.process_with_sr(&mut samples, 2, 96_000.0);

        assert!(samples.iter().all(|sample| sample.is_finite()));
        assert!(samples.iter().all(|sample| sample.abs() <= 2.0));
    }

    #[test]
    fn test_oversampled_processing_is_allocation_free_after_setup() {
        let mut sat = Saturation::new();
        sat.set_quality(SaturationQuality::Oversampled4x);
        sat.set_channel_count(2);
        sat.set_threshold(0.0);
        sat.set_drive(1.2);
        sat.set_mix(0.8);

        let mut samples = vec![0.0; 512 * 2];
        for frame in 0..512 {
            let sample = (std::f64::consts::TAU * frame as f64 / 11.0).sin() * 0.9;
            samples[frame * 2] = sample;
            samples[frame * 2 + 1] = -sample;
        }

        sat.process_with_channels(&mut samples, 2);

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..32 {
                sat.process_with_channels(&mut samples, 2);
            }
        });
    }
}
