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
/// Enabled Saturation keeps one fixed timeline across Direct and oversampled
/// quality modes. The oversampling filters' group delay is four source frames.
pub const SATURATION_LATENCY_FRAMES: usize = 4;
/// Width of the C1 transition from identity to the selected waveshaper.
///
/// A fixed 5% full-scale knee keeps the threshold local while remaining
/// continuous even when callers intentionally process samples above 0 dBFS.
const SATURATION_SOFT_KNEE_WIDTH: f64 = 0.05;
// Ten source frames rebuild either oversampling state: after the first replay
// frame establishes interpolation history, nine more frames push 18 samples
// through the 17-tap 2x FIR or 36 through the 33-tap 4x FIR.
const SATURATION_SOURCE_HISTORY_FRAMES: usize = 10;
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

#[derive(Clone, Copy)]
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

    fn initialize(&mut self, input: f64) {
        self.previous_input = input;
        self.initialized = true;
        self.filter_history.fill(0.0);
        self.filter_index = 0;
    }

    #[inline]
    fn push(&mut self, sample: f64, len: usize) {
        debug_assert!(len > 0 && len <= OVERSAMPLING_MAX_FILTER_TAPS);
        self.filter_history[self.filter_index] = sample;
        self.filter_index += 1;
        if self.filter_index == len {
            self.filter_index = 0;
        }
    }

    #[inline]
    fn evaluate(&self, coefficients: &[f64]) -> f64 {
        if coefficients.is_empty() {
            return 0.0;
        }

        let len = coefficients.len();
        let mut acc = 0.0;
        let mut history_index = if self.filter_index == 0 {
            len - 1
        } else {
            self.filter_index - 1
        };
        for &coefficient in coefficients {
            acc += coefficient * self.filter_history[history_index];
            history_index = if history_index == 0 {
                len - 1
            } else {
                history_index - 1
            };
        }

        acc
    }
}

#[derive(Clone, Copy)]
struct DelayChannelState {
    raw: [f64; SATURATION_LATENCY_FRAMES],
    dry: [f64; SATURATION_LATENCY_FRAMES],
    delta: [f64; SATURATION_LATENCY_FRAMES],
}

impl Default for DelayChannelState {
    fn default() -> Self {
        Self {
            raw: [0.0; SATURATION_LATENCY_FRAMES],
            dry: [0.0; SATURATION_LATENCY_FRAMES],
            delta: [0.0; SATURATION_LATENCY_FRAMES],
        }
    }
}

impl DelayChannelState {
    #[inline]
    fn push_raw(&mut self, sample: f64, index: usize) -> f64 {
        let delayed = self.raw[index];
        self.raw[index] = sample;
        delayed
    }

    #[inline]
    fn push_dry(&mut self, sample: f64, index: usize) -> f64 {
        let delayed = self.dry[index];
        self.dry[index] = sample;
        delayed
    }

    #[inline]
    fn push_delta(&mut self, sample: f64, index: usize) -> f64 {
        let delayed = self.delta[index];
        self.delta[index] = sample;
        delayed
    }

    fn reset(&mut self) {
        self.raw = [0.0; SATURATION_LATENCY_FRAMES];
        self.dry = [0.0; SATURATION_LATENCY_FRAMES];
        self.delta = [0.0; SATURATION_LATENCY_FRAMES];
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
#[derive(Clone)]
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
    /// Recent full-band or HPF samples at the nonlinear/oversampling boundary.
    source_history: Vec<[f64; SATURATION_SOURCE_HISTORY_FRAMES]>,
    source_history_index: usize,
    source_history_len: usize,
    /// Per-channel fixed timeline and Direct/high-pass delta delay state.
    delay_states: Vec<DelayChannelState>,
    delay_index: usize,
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
            source_history: vec![[0.0; SATURATION_SOURCE_HISTORY_FRAMES]; 2],
            source_history_index: 0,
            source_history_len: 0,
            delay_states: vec![DelayChannelState::default(); 2],
            delay_index: 0,
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

    /// Copy configuration and signal state into an already-sized instance.
    ///
    /// This is used only by the preallocated quality-transition bank. Both
    /// instances must have the same setup-time channel geometry.
    pub(crate) fn copy_from_preallocated(&mut self, source: &Self) {
        debug_assert_eq!(self.hpf_states.len(), source.hpf_states.len());
        debug_assert_eq!(self.prev_inputs.len(), source.prev_inputs.len());
        debug_assert_eq!(
            self.oversampling_states.len(),
            source.oversampling_states.len()
        );
        debug_assert_eq!(self.source_history.len(), source.source_history.len());
        debug_assert_eq!(self.delay_states.len(), source.delay_states.len());
        if self.hpf_states.len() != source.hpf_states.len()
            || self.prev_inputs.len() != source.prev_inputs.len()
            || self.oversampling_states.len() != source.oversampling_states.len()
            || self.source_history.len() != source.source_history.len()
            || self.delay_states.len() != source.delay_states.len()
        {
            return;
        }

        self.sat_type = source.sat_type;
        self.quality = source.quality;
        self.drive = source.drive;
        self.threshold = source.threshold;
        self.mix = source.mix;
        self.input_gain_db = source.input_gain_db;
        self.output_gain_db = source.output_gain_db;
        self.input_gain_linear = source.input_gain_linear;
        self.output_gain_linear = source.output_gain_linear;
        self.enabled = source.enabled;
        self.highpass_mode = source.highpass_mode;
        self.highpass_cutoff = source.highpass_cutoff;
        self.sample_rate = source.sample_rate;
        self.hpf_coef = source.hpf_coef;
        self.hpf_states.copy_from_slice(&source.hpf_states);
        self.prev_inputs.copy_from_slice(&source.prev_inputs);
        self.oversampling_states
            .copy_from_slice(&source.oversampling_states);
        self.source_history.copy_from_slice(&source.source_history);
        self.source_history_index = source.source_history_index;
        self.source_history_len = source.source_history_len;
        self.delay_states.copy_from_slice(&source.delay_states);
        self.delay_index = source.delay_index;
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
            self.prepare_nonlinear_state_from_history();
        }
    }

    /// Enable/disable high-pass mode (exciter mode)
    pub fn set_highpass_mode(&mut self, enabled: bool) {
        if self.highpass_mode != enabled {
            self.reset_oversampling_states();
            self.reset_source_history();
        }
        self.highpass_mode = enabled;
    }

    /// Set high-pass cutoff frequency in Hz
    pub fn set_highpass_cutoff(&mut self, hz: f64) {
        self.highpass_cutoff = hz.clamp(1000.0, 12000.0);
        self.update_hpf_coef();
    }

    /// Update sample rate and recalculate HPF coefficient.
    ///
    /// A sample-rate change starts a new timing domain. Discarding delay,
    /// oversampling, and HPF history prevents samples produced at the old
    /// rate from leaking into the new stream. Invalid rates are ignored by
    /// this infallible standalone setter; callback adapters validate before
    /// calling it.
    pub fn set_sample_rate(&mut self, sr: f64) {
        if !sr.is_finite() || sr <= 0.0 {
            return;
        }
        if self.sample_rate == sr {
            self.update_hpf_coef();
            return;
        }
        self.sample_rate = sr;
        self.update_hpf_coef();
        self.reset();
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
            self.source_history
                .resize(channels, [0.0; SATURATION_SOURCE_HISTORY_FRAMES]);
            self.delay_states
                .resize(channels, DelayChannelState::default());
            self.reset();
        }
    }

    fn reset_oversampling_states(&mut self) {
        for state in &mut self.oversampling_states {
            state.reset();
        }
    }

    fn reset_source_history(&mut self) {
        for history in &mut self.source_history {
            history.fill(0.0);
        }
        self.source_history_index = 0;
        self.source_history_len = 0;
    }

    #[inline]
    fn record_source_sample(&mut self, channel: usize, sample: f64) {
        self.source_history[channel][self.source_history_index] = sample;
    }

    #[inline]
    fn advance_source_history(&mut self) {
        self.source_history_index =
            (self.source_history_index + 1) % SATURATION_SOURCE_HISTORY_FRAMES;
        self.source_history_len =
            (self.source_history_len + 1).min(SATURATION_SOURCE_HISTORY_FRAMES);
    }

    /// Rebuild only quality-dependent nonlinear state from recent source
    /// samples. HPF and timeline state remain copied and aligned across slots.
    pub(crate) fn prepare_nonlinear_state_from_history(&mut self) {
        self.reset_oversampling_states();
        for delay in &mut self.delay_states {
            delay.delta.fill(0.0);
        }

        let history_len = self.source_history_len;
        let history_start = (self.source_history_index + SATURATION_SOURCE_HISTORY_FRAMES
            - history_len)
            % SATURATION_SOURCE_HISTORY_FRAMES;
        let sat_type = self.sat_type;
        let threshold = self.threshold;
        let drive_plus1 = 1.0 + self.drive;

        for channel in 0..self.source_history.len() {
            let history = self.source_history[channel];
            if self.quality == SaturationQuality::Direct {
                let retained = history_len.min(SATURATION_LATENCY_FRAMES);
                for retained_offset in 0..retained {
                    let history_offset = history_len - retained + retained_offset;
                    let sample = history
                        [(history_start + history_offset) % SATURATION_SOURCE_HISTORY_FRAMES];
                    let shaped = Self::apply_thresholded_saturation(
                        sat_type,
                        sample,
                        threshold,
                        drive_plus1,
                    );
                    let delay_position = (self.delay_index + SATURATION_LATENCY_FRAMES - retained
                        + retained_offset)
                        % SATURATION_LATENCY_FRAMES;
                    self.delay_states[channel].delta[delay_position] = shaped - sample;
                }
                continue;
            }

            let ratio = self.quality.ratio();
            let filter = self.quality.decimation_filter();
            for history_offset in 0..history_len {
                let sample =
                    history[(history_start + history_offset) % SATURATION_SOURCE_HISTORY_FRAMES];
                Self::advance_oversampled_state(
                    &mut self.oversampling_states[channel],
                    sample,
                    ratio,
                    filter,
                    sat_type,
                    threshold,
                    drive_plus1,
                );
            }
        }
    }

    fn reset_delay_states(&mut self) {
        for state in &mut self.delay_states {
            state.reset();
        }
        self.delay_index = 0;
    }

    fn has_channel_capacity(&self, channels: usize) -> bool {
        self.hpf_states.len() >= channels
            && self.prev_inputs.len() >= channels
            && self.oversampling_states.len() >= channels
            && self.source_history.len() >= channels
            && self.delay_states.len() >= channels
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
        self.process_with_channels_mix(samples, channels, 1.0);
    }

    /// Process with an additional effect-enable weight.
    ///
    /// The state advances exactly once for every source frame. A zero weight
    /// still emits the fixed delayed dry timeline, which lets an adapter ramp
    /// enable/disable without running a second copy of the DSP state.
    pub fn process_with_channels_mix(
        &mut self,
        samples: &mut [f64],
        channels: usize,
        effect_weight: f64,
    ) {
        if !self.enabled {
            return;
        }

        if channels == 0
            || !samples.len().is_multiple_of(channels)
            || !self.has_channel_capacity(channels)
        {
            return;
        }

        if self.highpass_mode {
            self.process_highpass(samples, channels, effect_weight);
        } else {
            self.process_fullband(samples, channels, effect_weight);
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
    fn process_fullband(&mut self, samples: &mut [f64], channels: usize, effect_weight: f64) {
        if self.quality != SaturationQuality::Direct {
            self.process_fullband_oversampled(samples, channels, effect_weight);
            return;
        }

        let input_gain = self.input_gain_linear;
        let output_gain = self.output_gain_linear;
        let threshold = self.threshold;
        let drive_plus1 = 1.0 + self.drive;
        let mix = self.mix;
        let sat_type = self.sat_type;

        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let index = frame * channels + ch;
                let raw = samples[index];
                let dry = raw * input_gain;
                let delayed_raw = self.delay_states[ch].push_raw(raw, self.delay_index);
                let delayed_dry = self.delay_states[ch].push_dry(dry, self.delay_index);
                let wet = Self::apply_thresholded_saturation(sat_type, dry, threshold, drive_plus1);
                let delayed_delta = self.delay_states[ch].push_delta(wet - dry, self.delay_index);
                let processed = (delayed_dry + delayed_delta * mix) * output_gain;
                samples[index] = delayed_raw + (processed - delayed_raw) * effect_weight;
                self.record_source_sample(ch, dry);
            }
            self.delay_index = (self.delay_index + 1) % SATURATION_LATENCY_FRAMES;
            self.advance_source_history();
        }
    }

    fn process_fullband_oversampled(
        &mut self,
        samples: &mut [f64],
        channels: usize,
        effect_weight: f64,
    ) {
        debug_assert!(
            self.oversampling_states.len() >= channels,
            "Saturation oversampling state undersized for {} channels (have {}); call set_channel_count during setup",
            channels,
            self.oversampling_states.len()
        );
        if self.oversampling_states.len() < channels {
            return;
        }

        match self.quality {
            SaturationQuality::Oversampled2x => self.process_fullband_oversampled_fixed::<2, 17>(
                samples,
                channels,
                effect_weight,
                &OVERSAMPLING_2X_FILTER,
            ),
            SaturationQuality::Oversampled4x => self
                .process_fullband_oversampled_fixed::<4, OVERSAMPLING_MAX_FILTER_TAPS>(
                    samples,
                    channels,
                    effect_weight,
                    &OVERSAMPLING_4X_FILTER,
                ),
            SaturationQuality::Direct => {}
        }
    }

    #[inline]
    fn process_fullband_oversampled_fixed<const RATIO: usize, const TAPS: usize>(
        &mut self,
        samples: &mut [f64],
        channels: usize,
        effect_weight: f64,
        filter: &[f64; TAPS],
    ) {
        debug_assert!(RATIO > 1);
        debug_assert!(TAPS > 0 && TAPS <= OVERSAMPLING_MAX_FILTER_TAPS);

        let input_gain = self.input_gain_linear;
        let output_gain = self.output_gain_linear;
        let threshold = self.threshold;
        let drive_plus1 = 1.0 + self.drive;
        let mix = self.mix;
        let sat_type = self.sat_type;

        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let index = frame * channels + ch;
                let raw = samples[index];
                let dry = raw * input_gain;
                let delayed_raw = self.delay_states[ch].push_raw(raw, self.delay_index);
                let delayed_dry = self.delay_states[ch].push_dry(dry, self.delay_index);
                let delta = Self::process_oversampled_delta_fixed::<RATIO, TAPS>(
                    &mut self.oversampling_states[ch],
                    dry,
                    filter,
                    sat_type,
                    threshold,
                    drive_plus1,
                );
                let processed = (delayed_dry + delta * mix) * output_gain;
                samples[index] = delayed_raw + (processed - delayed_raw) * effect_weight;
                self.record_source_sample(ch, dry);
            }
            self.delay_index = (self.delay_index + 1) % SATURATION_LATENCY_FRAMES;
            self.advance_source_history();
        }
    }

    /// High-pass separated saturation (exciter mode)
    /// Only saturates frequencies above the cutoff.
    /// P1-5 fix: Supports arbitrary channel count (was hardcoded to L/R only).
    fn process_highpass(&mut self, samples: &mut [f64], channels: usize, effect_weight: f64) {
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

        match self.quality {
            SaturationQuality::Direct => {
                self.process_highpass_fixed::<1, 0>(samples, channels, effect_weight, &[])
            }
            SaturationQuality::Oversampled2x => self.process_highpass_fixed::<2, 17>(
                samples,
                channels,
                effect_weight,
                &OVERSAMPLING_2X_FILTER,
            ),
            SaturationQuality::Oversampled4x => self
                .process_highpass_fixed::<4, OVERSAMPLING_MAX_FILTER_TAPS>(
                    samples,
                    channels,
                    effect_weight,
                    &OVERSAMPLING_4X_FILTER,
                ),
        }
    }

    #[inline]
    fn process_highpass_fixed<const RATIO: usize, const TAPS: usize>(
        &mut self,
        samples: &mut [f64],
        channels: usize,
        effect_weight: f64,
        filter: &[f64; TAPS],
    ) {
        debug_assert!(
            (RATIO == 1 && TAPS == 0)
                || (RATIO > 1 && TAPS > 0 && TAPS <= OVERSAMPLING_MAX_FILTER_TAPS)
        );

        let input_gain = self.input_gain_linear;
        let output_gain = self.output_gain_linear;
        let alpha = self.hpf_coef;
        let threshold = self.threshold;
        let drive_plus1 = 1.0 + self.drive;
        let mix = self.mix;
        let sat_type = self.sat_type;

        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let raw = samples[idx];
                let input = raw * input_gain;

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
                let delta = if TAPS == 0 {
                    let saturated =
                        Self::apply_thresholded_saturation(sat_type, high, threshold, drive_plus1);
                    self.delay_states[ch].push_delta(saturated - high, self.delay_index)
                } else {
                    Self::process_oversampled_delta_fixed::<RATIO, TAPS>(
                        &mut self.oversampling_states[ch],
                        high,
                        filter,
                        sat_type,
                        threshold,
                        drive_plus1,
                    )
                };

                let delayed_raw = self.delay_states[ch].push_raw(raw, self.delay_index);
                let delayed_input = self.delay_states[ch].push_dry(input, self.delay_index);
                // Mix: delayed input + delayed/filtered nonlinear residual.
                let processed = (delayed_input + delta * mix) * output_gain;
                samples[idx] = delayed_raw + (processed - delayed_raw) * effect_weight;
                self.record_source_sample(ch, high);
            }
            self.delay_index = (self.delay_index + 1) % SATURATION_LATENCY_FRAMES;
            self.advance_source_history();
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

    #[inline(always)]
    fn process_oversampled_delta_fixed<const RATIO: usize, const TAPS: usize>(
        state: &mut OversamplingChannelState,
        input: f64,
        filter: &[f64; TAPS],
        sat_type: SaturationType,
        threshold: f64,
        drive_plus1: f64,
    ) -> f64 {
        Self::advance_oversampled_state_fixed::<RATIO, TAPS>(
            state,
            input,
            sat_type,
            threshold,
            drive_plus1,
        );
        state.evaluate(filter)
    }

    #[inline(always)]
    fn advance_oversampled_state_fixed<const RATIO: usize, const TAPS: usize>(
        state: &mut OversamplingChannelState,
        input: f64,
        sat_type: SaturationType,
        threshold: f64,
        drive_plus1: f64,
    ) {
        if !state.initialized {
            state.initialize(input);
        }

        let previous = state.previous_input;
        let delta = input - previous;
        for phase in 1..=RATIO {
            let t = phase as f64 / RATIO as f64;
            let interpolated = previous + delta * t;
            let shaped =
                Self::apply_thresholded_saturation(sat_type, interpolated, threshold, drive_plus1);
            state.push(shaped - interpolated, TAPS);
        }
        state.previous_input = input;
    }

    #[inline]
    fn advance_oversampled_state(
        state: &mut OversamplingChannelState,
        input: f64,
        ratio: usize,
        filter: &[f64],
        sat_type: SaturationType,
        threshold: f64,
        drive_plus1: f64,
    ) -> f64 {
        if !state.initialized {
            state.initialize(input);
        }

        let previous = state.previous_input;
        let delta = input - previous;
        let mut final_delta = 0.0;
        for phase in 1..=ratio {
            let t = phase as f64 / ratio as f64;
            let interpolated = previous + delta * t;
            let shaped =
                Self::apply_thresholded_saturation(sat_type, interpolated, threshold, drive_plus1);
            let nonlinear_delta = shaped - interpolated;
            if filter.is_empty() {
                final_delta = nonlinear_delta;
            } else {
                state.push(nonlinear_delta, filter.len());
            }
        }
        state.previous_input = input;
        final_delta
    }

    /// Reset filter state
    pub fn reset(&mut self) {
        self.hpf_states.fill(0.0);
        self.prev_inputs.fill(0.0);
        self.reset_oversampling_states();
        self.reset_source_history();
        self.reset_delay_states();
    }

    /// Fixed enabled-stage latency used by all quality modes.
    pub const fn latency_frames(&self) -> usize {
        if self.enabled {
            SATURATION_LATENCY_FRAMES
        } else {
            0
        }
    }

    /// Number of source frames needed to flush the fixed full-band state.
    /// High-pass mode is asymptotic and is handled by the unknown-tail driver.
    pub fn finite_tail_frames(&self) -> usize {
        if !self.enabled || self.highpass_mode {
            return 0;
        }
        let filter_tail = if self.quality == SaturationQuality::Direct {
            0
        } else {
            let ratio = self.quality.ratio();
            let taps = self.quality.decimation_filter().len();
            taps.saturating_sub(1).div_ceil(ratio)
        };
        SATURATION_LATENCY_FRAMES.max(filter_tail)
    }

    /// Finite effect tail beyond the fixed algorithmic delay.
    pub fn semantic_tail_frames(&self) -> usize {
        self.finite_tail_frames()
            .saturating_sub(self.latency_frames())
    }

    /// Process a transparently delayed bypass for an armed stage.
    ///
    /// This is distinct from [`Saturation::set_enabled`], whose disabled state
    /// remains the direct zero-latency hard bypass for standalone callers.
    pub fn process_delayed_bypass(&mut self, samples: &mut [f64], channels: usize) {
        if channels == 0
            || !samples.len().is_multiple_of(channels)
            || !self.has_channel_capacity(channels)
        {
            return;
        }
        debug_assert!(self.delay_states.len() >= channels);
        debug_assert!(self.source_history.len() >= channels);
        debug_assert!(self.hpf_states.len() >= channels);
        if self.delay_states.len() < channels
            || self.source_history.len() < channels
            || self.hpf_states.len() < channels
            || self.prev_inputs.len() < channels
        {
            return;
        }

        let input_gain = self.input_gain_linear;
        let alpha = self.hpf_coef;
        let frames = samples.len() / channels;
        for frame in 0..frames {
            for ch in 0..channels {
                let index = frame * channels + ch;
                let raw = samples[index];
                let gained = raw * input_gain;
                let nonlinear_source = if self.highpass_mode {
                    let high =
                        alpha * self.hpf_states[ch] + alpha * (gained - self.prev_inputs[ch]);
                    self.hpf_states[ch] = high;
                    self.prev_inputs[ch] = gained;
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
                    high
                } else {
                    gained
                };
                self.record_source_sample(ch, nonlinear_source);
                samples[index] = self.delay_states[ch].push_raw(raw, self.delay_index);
                let _ = self.delay_states[ch].push_dry(gained, self.delay_index);
                let _ = self.delay_states[ch].push_delta(0.0, self.delay_index);
            }
            self.delay_index = (self.delay_index + 1) % SATURATION_LATENCY_FRAMES;
            self.advance_source_history();
        }
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
        samples.extend_from_slice(&[0.0; SATURATION_LATENCY_FRAMES]);
        sat.process_with_channels(&mut samples, 1);

        // tanh(0.9) ≈ 0.716
        assert!(samples[SATURATION_LATENCY_FRAMES].abs() < 0.9);
        assert!(samples[SATURATION_LATENCY_FRAMES + 1].abs() < 0.9);

        // Lower signals should pass through relatively unchanged
        // tanh(0.5) ≈ 0.462, which is close to 0.5
        assert!((samples[SATURATION_LATENCY_FRAMES + 2].abs() - 0.5).abs() < 0.1);
    }

    #[test]
    fn test_disabled() {
        let mut sat = Saturation::new();
        sat.set_enabled(false);

        let mut samples = vec![0.9, -0.9, 0.5, -0.5];
        sat.process_with_channels(&mut samples, 1);

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
        let mut samples = vec![0.5; SATURATION_LATENCY_FRAMES + 1];
        sat.process_with_channels(&mut samples, 1);
        assert!((samples[SATURATION_LATENCY_FRAMES] - 0.5).abs() < 1e-10);

        // Above threshold should be saturated
        let mut samples = vec![0.9; SATURATION_LATENCY_FRAMES + 1];
        sat.process_with_channels(&mut samples, 1);
        assert!(samples[SATURATION_LATENCY_FRAMES].abs() < 0.9);
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
        let mut sample = vec![input; SATURATION_LATENCY_FRAMES + 1];
        saturation.process_with_channels(&mut sample, 1);
        sample[SATURATION_LATENCY_FRAMES]
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
        let mut samples = vec![0.5, 0.8, 0.9];
        samples.extend_from_slice(&[0.0; SATURATION_LATENCY_FRAMES]);

        saturation.process_with_channels(&mut samples, 1);

        let output_gain = db_to_linear(-6.0);
        assert!((samples[SATURATION_LATENCY_FRAMES] - 0.5 * output_gain).abs() <= 1.0e-12);
        assert!((samples[SATURATION_LATENCY_FRAMES + 1] - 0.8 * output_gain).abs() <= 1.0e-12);
        assert!(samples[SATURATION_LATENCY_FRAMES + 2] < 0.9 * output_gain);
    }

    #[test]
    fn partial_mix_matches_analytic_tube_transfer_at_steady_state() {
        let input = 0.8_f64;
        let drive = 0.8_f64;
        let mix = 0.37_f64;
        let expected = input + mix * ((input * (1.0 + drive)).tanh() - input);

        for quality in [
            SaturationQuality::Direct,
            SaturationQuality::Oversampled2x,
            SaturationQuality::Oversampled4x,
        ] {
            let mut saturation = Saturation::with_type(SaturationType::Tube);
            saturation.set_channel_count(1);
            saturation.set_quality(quality);
            saturation.set_threshold(0.0);
            saturation.set_drive(drive);
            saturation.set_mix(mix);
            let mut samples = vec![input; 64];

            saturation.process_with_channels(&mut samples, 1);

            let error = (samples[63] - expected).abs();
            assert!(
                error <= 1.0e-12,
                "quality={quality:?}: actual={} expected={expected} error={error:e}",
                samples[63]
            );
        }
    }

    #[test]
    fn direct_highpass_exciter_matches_independent_topology_oracle() {
        const SAMPLE_RATE: f64 = 48_000.0;
        const CUTOFF_HZ: f64 = 4_000.0;
        const DRIVE: f64 = 0.5;
        const MIX: f64 = 0.6;
        let input = (0..64)
            .map(|frame| if frame % 2 == 0 { 0.8 } else { -0.8 })
            .collect::<Vec<_>>();
        let alpha = SAMPLE_RATE / (SAMPLE_RATE + std::f64::consts::TAU * CUTOFF_HZ);
        let mut expected = Vec::with_capacity(input.len());
        let mut previous_input = 0.0;
        let mut previous_high = 0.0;
        let mut dry_delay = [0.0; SATURATION_LATENCY_FRAMES];
        let mut delta_delay = [0.0; SATURATION_LATENCY_FRAMES];
        let mut delay_index = 0;
        for &sample in &input {
            let high = alpha * previous_high + alpha * (sample - previous_input);
            previous_input = sample;
            previous_high = high;
            let nonlinear_delta = (high * (1.0 + DRIVE)).tanh() - high;
            let delayed_dry = dry_delay[delay_index];
            dry_delay[delay_index] = sample;
            let delayed_delta = delta_delay[delay_index];
            delta_delay[delay_index] = nonlinear_delta;
            expected.push(delayed_dry + MIX * delayed_delta);
            delay_index = (delay_index + 1) % SATURATION_LATENCY_FRAMES;
        }

        let mut saturation = Saturation::with_type(SaturationType::Tube);
        saturation.set_channel_count(1);
        saturation.set_sample_rate(SAMPLE_RATE);
        saturation.set_highpass_cutoff(CUTOFF_HZ);
        saturation.set_highpass_mode(true);
        saturation.set_quality(SaturationQuality::Direct);
        saturation.set_threshold(0.0);
        saturation.set_drive(DRIVE);
        saturation.set_mix(MIX);
        let mut actual = input;

        saturation.process_with_channels(&mut actual, 1);

        for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
            assert!(
                (actual - expected).abs() <= 1.0e-12,
                "sample={index}: actual={actual} expected={expected}"
            );
        }
    }

    #[test]
    fn tube_transfer_has_odd_harmonic_spectrum() {
        const FRAMES: usize = 4_096;
        const CYCLES: usize = 64;
        let omega = std::f64::consts::TAU * CYCLES as f64 / FRAMES as f64;
        let mut samples = (0..FRAMES + SATURATION_LATENCY_FRAMES)
            .map(|frame| (omega * frame as f64).sin() * 0.8)
            .collect::<Vec<_>>();
        let mut saturation = Saturation::with_type(SaturationType::Tube);
        saturation.set_channel_count(1);
        saturation.set_quality(SaturationQuality::Direct);
        saturation.set_threshold(0.0);
        saturation.set_drive(1.0);
        saturation.set_mix(1.0);

        saturation.process_with_channels(&mut samples, 1);
        let signal = &samples[SATURATION_LATENCY_FRAMES..];
        let harmonic_amplitude = |harmonic: usize| {
            let mut cosine = 0.0;
            let mut sine = 0.0;
            for (frame, &sample) in signal.iter().enumerate() {
                let phase = omega * harmonic as f64 * frame as f64;
                cosine += sample * phase.cos();
                sine += sample * phase.sin();
            }
            2.0 * cosine.hypot(sine) / signal.len() as f64
        };
        let fundamental = harmonic_amplitude(1);
        let second = harmonic_amplitude(2);
        let third = harmonic_amplitude(3);

        assert!(fundamental > 0.5, "fundamental={fundamental:e}");
        assert!(third > 1.0e-2, "third harmonic={third:e}");
        assert!(
            third < fundamental,
            "fundamental={fundamental:e} third={third:e}"
        );
        assert!(
            second <= fundamental * 1.0e-12,
            "symmetric Tube transfer emitted an even harmonic: fundamental={fundamental:e} second={second:e}"
        );
    }

    #[test]
    fn test_mix() {
        let mut sat = Saturation::with_type(SaturationType::Tube);
        sat.set_enabled(true);
        sat.set_drive(0.0); // No drive for this test
        sat.set_mix(0.5);

        let mut samples = vec![1.0; SATURATION_LATENCY_FRAMES + 1];
        sat.process_with_channels(&mut samples, 1);

        // Mix of tanh(1) ≈ 0.762 and 1.0
        // Result should be between the two
        let expected = (1.0 + 1.0_f64.tanh()) * 0.5;
        assert!((samples[SATURATION_LATENCY_FRAMES] - expected).abs() < 0.01);
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
    fn unconfigured_direct_multichannel_geometry_is_a_deterministic_bypass() {
        let mut saturation = Saturation::new();
        let mut samples = [0.9, -0.8, 0.7, -0.6, 0.5, -0.4];
        let original = samples;

        saturation.process_with_channels(&mut samples, 3);

        assert_eq!(samples, original);
    }

    #[test]
    fn oversampled_finite_tail_ends_on_the_last_nonzero_support_frame() {
        for quality in [
            SaturationQuality::Oversampled2x,
            SaturationQuality::Oversampled4x,
        ] {
            let mut saturation = Saturation::new();
            saturation.set_channel_count(1);
            saturation.set_quality(quality);
            saturation.set_threshold(0.0);
            saturation.set_drive(2.0);
            saturation.set_mix(1.0);

            let mut impulse = [1.0];
            saturation.process_with_channels(&mut impulse, 1);
            let declared = saturation.finite_tail_frames();
            assert_eq!(declared, 8, "quality={quality:?}");

            let mut tail = vec![0.0; declared + 1];
            saturation.process_with_channels(&mut tail, 1);
            assert!(
                tail[declared - 1].abs() > 1.0e-12,
                "last declared support was silent for {quality:?}: {tail:?}"
            );
            assert_eq!(tail[declared], 0.0, "quality={quality:?}");
        }
    }

    fn process_in_frame_chunks(
        saturation: &mut Saturation,
        samples: &mut [f64],
        channels: usize,
        chunk_frames: &[usize],
    ) {
        let mut frame = 0usize;
        let mut chunk = 0usize;
        let frames = samples.len() / channels;
        while frame < frames {
            let count = chunk_frames[chunk % chunk_frames.len()].min(frames - frame);
            let start = frame * channels;
            let end = (frame + count) * channels;
            saturation.process_with_channels(&mut samples[start..end], channels);
            frame += count;
            chunk += 1;
        }
    }

    #[test]
    fn below_threshold_is_bit_exact_delayed_dry_for_all_mixes_and_chunks() {
        let channels = 2;
        let frames = 257;
        let mut program = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            let phase = frame as f64 * 0.071;
            program.push(phase.sin() * 0.2);
            program.push(phase.cos() * 0.18);
        }
        let mut expected = vec![0.0; SATURATION_LATENCY_FRAMES * channels];
        expected.extend_from_slice(&program);

        for quality in [
            SaturationQuality::Direct,
            SaturationQuality::Oversampled2x,
            SaturationQuality::Oversampled4x,
        ] {
            for mix in [0.0, 0.25, 0.5, 1.0] {
                let mut saturation = Saturation::new();
                saturation.set_channel_count(channels);
                saturation.set_quality(quality);
                saturation.set_threshold(0.8);
                saturation.set_drive(2.0);
                saturation.set_mix(mix);
                let mut actual = program.clone();
                actual.resize(expected.len(), 0.0);

                process_in_frame_chunks(&mut saturation, &mut actual, channels, &[1, 7, 31, 2, 64]);

                assert_eq!(actual, expected, "quality={quality:?} mix={mix}");
            }
        }
    }

    #[test]
    fn oversampled_partial_mix_is_affine_from_delayed_dry_to_full_wet() {
        let channels = 2;
        let frames = 512;
        let mix = 0.37;
        let mut input = Vec::with_capacity((frames + 8) * channels);
        for frame in 0..frames {
            let phase = frame as f64 * 0.19;
            input.push((phase.sin() * 0.92).clamp(-0.95, 0.95));
            input.push((phase.cos() * 0.87).clamp(-0.95, 0.95));
        }
        input.resize((frames + 8) * channels, 0.0);

        for quality in [
            SaturationQuality::Oversampled2x,
            SaturationQuality::Oversampled4x,
        ] {
            let render = |wet_mix: f64| {
                let mut saturation = Saturation::new();
                saturation.set_channel_count(channels);
                saturation.set_quality(quality);
                saturation.set_type(SaturationType::Tube);
                saturation.set_threshold(0.3);
                saturation.set_drive(1.5);
                saturation.set_mix(wet_mix);
                let mut output = input.clone();
                process_in_frame_chunks(&mut saturation, &mut output, channels, &[3, 1, 29, 64, 5]);
                output
            };

            let dry = render(0.0);
            let wet = render(1.0);
            let partial = render(mix);
            for (index, ((dry, wet), partial)) in dry.iter().zip(&wet).zip(&partial).enumerate() {
                let expected = dry + (wet - dry) * mix;
                assert!(
                    (partial - expected).abs() <= 1.0e-12,
                    "quality={quality:?} sample={index} actual={partial} expected={expected}"
                );
            }
        }
    }

    #[test]
    fn highpass_exciter_nonlinear_residual_is_high_frequency_selective() {
        fn residual_rms(frequency_hz: f64) -> f64 {
            let sample_rate = 48_000.0;
            let frames = 8_192;
            let mut input = Vec::with_capacity(frames);
            for frame in 0..frames {
                input.push(
                    (std::f64::consts::TAU * frequency_hz * frame as f64 / sample_rate).sin() * 0.9,
                );
            }

            let render = |mix: f64| {
                let mut saturation = Saturation::new();
                saturation.set_channel_count(1);
                saturation.set_sample_rate(sample_rate);
                saturation.set_quality(SaturationQuality::Oversampled4x);
                saturation.set_highpass_mode(true);
                saturation.set_highpass_cutoff(4_000.0);
                saturation.set_threshold(0.05);
                saturation.set_drive(1.5);
                saturation.set_mix(mix);
                let mut output = input.clone();
                saturation.process_with_channels(&mut output, 1);
                output
            };

            let dry = render(0.0);
            let wet = render(1.0);
            let start = 1_024;
            let power = dry[start..]
                .iter()
                .zip(&wet[start..])
                .map(|(dry, wet)| (wet - dry).powi(2))
                .sum::<f64>()
                / (frames - start) as f64;
            power.sqrt()
        }

        let low = residual_rms(200.0);
        let high = residual_rms(8_000.0);
        assert!(high > low * 3.0, "low={low} high={high}");
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
    fn sample_rate_change_resets_standalone_signal_history() {
        let mut saturation = Saturation::new();
        saturation.set_channel_count(1);
        saturation.set_quality(SaturationQuality::Oversampled4x);
        saturation.set_threshold(0.0);
        saturation.set_drive(1.0);
        saturation.set_mix(1.0);

        let mut impulse = [1.0];
        saturation.process_with_channels(&mut impulse, 1);
        saturation.set_sample_rate(96_000.0);

        let mut after_rate_change = [0.0; SATURATION_LATENCY_FRAMES + 8];
        saturation.process_with_channels(&mut after_rate_change, 1);
        assert!(
            after_rate_change
                .iter()
                .all(|sample| sample.to_bits() == 0.0_f64.to_bits()),
            "old-rate history leaked after sample-rate change: {after_rate_change:?}"
        );

        // Invalid standalone updates are non-mutating and must not poison the
        // coefficient/state domain.
        saturation.set_sample_rate(f64::NAN);
        assert_eq!(saturation.sample_rate, 96_000.0);
        assert!(saturation.hpf_coef.is_finite());
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

    fn assert_fixed_oversampling_matches_dynamic<const RATIO: usize, const TAPS: usize>(
        filter: &[f64; TAPS],
    ) {
        let inputs = [0.0, 0.91, -0.83, 0.12, 0.76, -0.38, 0.97, -0.99, 0.44, 0.0];

        for sat_type in [
            SaturationType::Tape,
            SaturationType::Tube,
            SaturationType::Transistor,
        ] {
            let mut dynamic = OversamplingChannelState::default();
            let mut fixed = OversamplingChannelState::default();
            for input in inputs {
                Saturation::advance_oversampled_state(
                    &mut dynamic,
                    input,
                    RATIO,
                    filter,
                    sat_type,
                    0.3,
                    1.92,
                );
                Saturation::advance_oversampled_state_fixed::<RATIO, TAPS>(
                    &mut fixed, input, sat_type, 0.3, 1.92,
                );

                assert_eq!(
                    fixed.evaluate(filter).to_bits(),
                    dynamic.evaluate(filter).to_bits(),
                    "ratio={RATIO} taps={TAPS} type={sat_type:?} input={input}"
                );
                assert_eq!(
                    fixed.previous_input.to_bits(),
                    dynamic.previous_input.to_bits()
                );
                assert_eq!(fixed.filter_index, dynamic.filter_index);
                assert_eq!(fixed.filter_history, dynamic.filter_history);
            }
        }
    }

    #[test]
    fn fixed_oversampling_kernels_match_dynamic_reference_bit_for_bit() {
        assert_fixed_oversampling_matches_dynamic::<2, 17>(&OVERSAMPLING_2X_FILTER);
        assert_fixed_oversampling_matches_dynamic::<4, OVERSAMPLING_MAX_FILTER_TAPS>(
            &OVERSAMPLING_4X_FILTER,
        );
    }
}
