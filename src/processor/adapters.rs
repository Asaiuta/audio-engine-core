//! Processor Adapters
//!
//! Wraps existing processors with the AudioProcessor trait, enabling
//! lock-free parameter passing and unified DSP chain management.
//!
//! Each adapter:
//! - Owns the actual processor (audio thread exclusive)
//! - References lock-free parameters (shared with main thread)
//! - Synchronizes parameters before processing

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwapOption;

use super::convolver::FFTConvolver;
use super::crossfeed::Crossfeed;
use super::dsp::NoiseShaper;
use super::dynamic_loudness::DynamicLoudness;
use super::eq::Equalizer;
use super::lockfree_params::*;
use super::loudness::PeakLimiter;
use super::saturation::Saturation;
use super::traits::{AudioProcessor, ProcessResult};
// ============================================================================
// EQ Adapter
// ============================================================================

/// Equalizer processor adapter with lock-free parameters
pub struct EqProcessor {
    /// Internal EQ processor (audio thread exclusive)
    eq: Equalizer,
    /// Channel count for reinitialization
    channels: usize,
    /// Lock-free parameters reference
    params: Arc<AtomicEqParams>,
    cached_params: Arc<EqParamsSnapshot>,
    cached_generation: u64,
    /// Local parameter cache
    cached: EqParamsSnapshot,
    /// Sample rate for coefficient recalculation
    sample_rate: f64,
}

impl EqProcessor {
    /// Create new EQ processor with lock-free params
    pub fn new(channels: usize, sample_rate: f64, params: Arc<AtomicEqParams>) -> Self {
        let (cached_params, cached_generation) = params.load_with_generation();
        let cached = *cached_params;
        let mut eq = Equalizer::new(channels, sample_rate);
        eq.set_all_bands(&cached.gains, sample_rate);
        eq.set_enabled(cached.enabled);
        Self {
            eq,
            channels,
            params,
            cached_params,
            cached_generation,
            cached,
            sample_rate,
        }
    }

    /// Synchronize parameters from lock-free storage
    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_params = current;
            self.cached_generation = generation;

            // Apply to internal EQ
            self.eq.set_all_bands(&self.cached.gains, self.sample_rate);
            self.eq.set_enabled(self.cached.enabled);
        }
    }
}

impl AudioProcessor for EqProcessor {
    fn name(&self) -> &'static str {
        "Equalizer"
    }

    fn process(&mut self, buffer: &mut [f64], _channels: usize) -> ProcessResult {
        self.sync_params();

        if !self.cached.enabled {
            return ProcessResult::Bypassed;
        }

        self.eq.process(buffer);
        ProcessResult::Ok
    }

    fn reset(&mut self) {
        self.eq.reset();
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.eq = Equalizer::new(self.channels, sample_rate);
        self.eq.set_all_bands(&self.cached.gains, sample_rate);
        self.eq.set_enabled(self.cached.enabled);
    }
}

// ============================================================================
// Saturation Adapter
// ============================================================================

/// Saturation processor adapter
pub struct SaturationProcessor {
    saturation: Saturation,
    params: Arc<AtomicSaturationParams>,
    cached_params: Arc<SaturationParamsSnapshot>,
    cached_generation: u64,
    cached: SaturationParamsSnapshot,
    sample_rate: f64,
}

impl SaturationProcessor {
    pub fn new(channels: usize, params: Arc<AtomicSaturationParams>) -> Self {
        let (cached_params, cached_generation) = params.load_with_generation();
        let cached = *cached_params;
        let mut saturation = Saturation::new();
        // Pre-size per-channel HPF state off the audio thread so highpass-mode
        // processing never resizes on the realtime thread.
        saturation.set_channel_count(channels);
        saturation.set_drive(cached.drive);
        saturation.set_threshold(cached.threshold);
        saturation.set_mix(cached.mix);
        saturation.set_input_gain(cached.input_gain_db);
        saturation.set_output_gain(cached.output_gain_db);
        saturation.set_highpass_mode(cached.highpass_mode);
        saturation.set_highpass_cutoff(cached.highpass_cutoff);
        saturation.set_enabled(cached.enabled);
        saturation.set_type(super::saturation::SaturationType::from(cached.sat_type));
        Self {
            saturation,
            params,
            cached_params,
            cached_generation,
            cached,
            sample_rate: 44100.0,
        }
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_params = current;
            self.cached_generation = generation;

            // Apply to saturation processor
            self.saturation.set_drive(self.cached.drive);
            self.saturation.set_threshold(self.cached.threshold);
            self.saturation.set_mix(self.cached.mix);
            self.saturation.set_input_gain(self.cached.input_gain_db);
            self.saturation.set_output_gain(self.cached.output_gain_db);
            self.saturation.set_highpass_mode(self.cached.highpass_mode);
            self.saturation
                .set_highpass_cutoff(self.cached.highpass_cutoff);
            self.saturation.set_enabled(self.cached.enabled);

            // M-4 fix: use From trait for type-safe conversion
            self.saturation
                .set_type(super::saturation::SaturationType::from(
                    self.cached.sat_type,
                ));
        }
    }
}

impl AudioProcessor for SaturationProcessor {
    fn name(&self) -> &'static str {
        "Saturation"
    }

    fn process(&mut self, buffer: &mut [f64], channels: usize) -> ProcessResult {
        self.sync_params();

        if !self.cached.enabled {
            return ProcessResult::Bypassed;
        }

        self.saturation.process_with_channels(buffer, channels);
        ProcessResult::Ok
    }

    fn reset(&mut self) {
        self.saturation.reset();
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.saturation.set_sample_rate(sample_rate);
    }
}

// ============================================================================
// Crossfeed Adapter
// ============================================================================

/// Crossfeed processor adapter
pub struct CrossfeedProcessor {
    crossfeed: Crossfeed,
    params: Arc<AtomicCrossfeedParams>,
    cached_params: Arc<CrossfeedParamsSnapshot>,
    cached_generation: u64,
    cached: CrossfeedParamsSnapshot,
    sample_rate: f64,
}

impl CrossfeedProcessor {
    pub fn new(sample_rate: f64, params: Arc<AtomicCrossfeedParams>) -> Self {
        let (cached_params, cached_generation) = params.load_with_generation();
        let cached = *cached_params;
        let mut crossfeed = Crossfeed::new(sample_rate);
        crossfeed.set_mix(cached.mix);
        crossfeed.set_enabled(cached.enabled);
        crossfeed.set_sample_rate(sample_rate, cached.cutoff_hz);
        Self {
            crossfeed,
            params,
            cached_params,
            cached_generation,
            cached,
            sample_rate,
        }
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_params = current;
            self.cached_generation = generation;
            self.crossfeed.set_mix(self.cached.mix);
            self.crossfeed.set_enabled(self.cached.enabled);
            // Cutoff change requires sample rate update
            self.crossfeed
                .set_sample_rate(self.sample_rate, self.cached.cutoff_hz);
        }
    }
}

impl AudioProcessor for CrossfeedProcessor {
    fn name(&self) -> &'static str {
        "Crossfeed"
    }

    fn process(&mut self, buffer: &mut [f64], channels: usize) -> ProcessResult {
        self.sync_params();

        if !self.cached.enabled {
            return ProcessResult::Bypassed;
        }

        self.crossfeed.process(buffer, channels);
        ProcessResult::Ok
    }

    fn reset(&mut self) {
        self.crossfeed.reset();
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.crossfeed
            .set_sample_rate(sample_rate, self.cached.cutoff_hz);
    }
}

// ============================================================================
// Peak Limiter Adapter
// ============================================================================

/// Peak limiter processor adapter
pub struct PeakLimiterProcessor {
    limiter: PeakLimiter,
    params: Arc<AtomicPeakLimiterParams>,
    cached_params: Arc<PeakLimiterParamsSnapshot>,
    cached_generation: u64,
    cached: PeakLimiterParamsSnapshot,
    sample_rate: u32,
    channels: usize,
}

impl PeakLimiterProcessor {
    pub fn new(channels: usize, sample_rate: u32, params: Arc<AtomicPeakLimiterParams>) -> Self {
        let (cached_params, cached_generation) = params.load_with_generation();
        let cached = *cached_params;
        Self {
            limiter: PeakLimiter::new(
                channels,
                sample_rate,
                cached.threshold_db,
                10.0,
                cached.release_ms,
            ),
            params,
            cached_params,
            cached_generation,
            cached,
            sample_rate,
            channels,
        }
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_params = current;
            self.cached_generation = generation;

            // In-place update — NO PeakLimiter::new(), NO heap allocation
            self.limiter.set_threshold(self.cached.threshold_db);
            self.limiter.set_release_ms(self.cached.release_ms);
            // If enabled state changed, limiter reset may be needed
            if self.cached.enabled != self.limiter.is_enabled() {
                self.limiter.reset();
            }
        }
    }
}

impl AudioProcessor for PeakLimiterProcessor {
    fn name(&self) -> &'static str {
        "PeakLimiter"
    }

    fn process(&mut self, buffer: &mut [f64], _channels: usize) -> ProcessResult {
        self.sync_params();

        if !self.cached.enabled {
            return ProcessResult::Bypassed;
        }

        self.limiter.process(buffer);
        ProcessResult::Ok
    }

    fn reset(&mut self) {
        self.limiter.reset();
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate as u32;
        self.limiter = PeakLimiter::new(
            self.channels,
            self.sample_rate,
            self.cached.threshold_db,
            10.0,
            self.cached.release_ms,
        );
    }
}

// ============================================================================
// Volume Adapter (P1-3 fix: anti-zipper smoothing)
// ============================================================================

/// Volume processor with exponential smoothing to prevent zipper noise.
///
/// Uses ~5ms smoothing time constant to ensure click-free volume transitions.
/// Previous implementation directly multiplied buffer by target volume,
/// causing audible clicks/zips on rapid volume changes.
pub struct VolumeProcessor {
    params: Arc<AtomicVolumeParams>,
    cached_params: Arc<VolumeParamsSnapshot>,
    cached_generation: u64,
    cached: VolumeParamsSnapshot,
    /// Current smoothed volume (exponentially approaches target)
    current_volume: f64,
    /// Smoothing coefficient per sample (calculated from sample rate)
    smoothing_coeff: f64,
    /// Cached `1.0 - smoothing_coeff`
    one_minus_smoothing_coeff: f64,
    /// Sample rate for smoothing calculation
    sample_rate: f64,
}

impl VolumeProcessor {
    const SETTLE_EPSILON: f64 = 1.0e-6;

    pub fn new(params: Arc<AtomicVolumeParams>) -> Self {
        let smoothing_coeff = Self::calc_smoothing_coeff(44100.0);
        let one_minus_smoothing_coeff = 1.0 - smoothing_coeff;
        let (cached_params, cached_generation) = params.load_with_generation();
        let cached = *cached_params;
        Self {
            params,
            cached_params,
            cached_generation,
            cached,
            current_volume: 1.0,
            smoothing_coeff,
            one_minus_smoothing_coeff,
            sample_rate: 44100.0,
        }
    }

    /// Calculate smoothing coefficient for ~5ms time constant
    fn calc_smoothing_coeff(sample_rate: f64) -> f64 {
        let smoothing_time_ms = 5.0;
        let smoothing_samples = (smoothing_time_ms / 1000.0) * sample_rate;
        (-1.0 / smoothing_samples).exp()
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_params = current;
            self.cached_generation = generation;
        }
    }
}

impl AudioProcessor for VolumeProcessor {
    fn name(&self) -> &'static str {
        "Volume"
    }

    fn process(&mut self, buffer: &mut [f64], channels: usize) -> ProcessResult {
        self.sync_params();

        // Volume is always "enabled" - just applies gain
        // Check for mute
        if self.cached.muted {
            // Smooth fade to zero to avoid click
            let coeff = self.smoothing_coeff;
            let mut current_volume = self.current_volume;
            for sample in buffer.iter_mut() {
                current_volume *= coeff;
                *sample *= current_volume;
            }
            self.current_volume = current_volume;
            return ProcessResult::Ok;
        }

        // Apply volume with anti-zipper smoothing
        let target = self.cached.volume;
        if self.current_volume == target {
            if target != 1.0 {
                for sample in buffer.iter_mut() {
                    *sample *= target;
                }
            }
            return ProcessResult::Ok;
        }

        let one_minus_coeff = self.one_minus_smoothing_coeff;
        let mut current_volume = self.current_volume;
        let frames = buffer.len() / channels;
        let mut frame = 0;

        while frame < frames {
            if (target - current_volume).abs() <= Self::SETTLE_EPSILON {
                current_volume = target;
                break;
            }

            // Exponential smoothing: current = current + (target - current) * (1 - coeff)
            current_volume += (target - current_volume) * one_minus_coeff;
            for ch in 0..channels {
                buffer[frame * channels + ch] *= current_volume;
            }
            frame += 1;
        }

        if frame < frames && target != 1.0 {
            for sample in &mut buffer[(frame * channels)..] {
                *sample *= target;
            }
        }
        self.current_volume = current_volume;

        ProcessResult::Ok
    }

    fn reset(&mut self) {
        self.current_volume = self.cached.volume;
    }

    fn is_enabled(&self) -> bool {
        true // Volume is always active
    }

    fn set_enabled(&mut self, _enabled: bool) {
        // Use set_muted instead
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        if (self.sample_rate - sample_rate).abs() > 1.0 {
            self.sample_rate = sample_rate;
            self.smoothing_coeff = Self::calc_smoothing_coeff(sample_rate);
            self.one_minus_smoothing_coeff = 1.0 - self.smoothing_coeff;
        }
    }
}

// ============================================================================
// Noise Shaper Adapter
// ============================================================================

/// Noise shaper processor adapter
pub struct NoiseShaperProcessor {
    noise_shaper: NoiseShaper,
    params: Arc<AtomicNoiseShaperParams>,
    cached_params: Arc<NoiseShaperParamsSnapshot>,
    cached_generation: u64,
    cached: NoiseShaperParamsSnapshot,
    sample_rate: u32,
    channels: usize,
}

impl NoiseShaperProcessor {
    pub fn new(channels: usize, sample_rate: u32, params: Arc<AtomicNoiseShaperParams>) -> Self {
        let (cached_params, cached_generation) = params.load_with_generation();
        let cached = *cached_params;
        let mut noise_shaper = NoiseShaper::new(channels, sample_rate, cached.bits);
        noise_shaper.set_enabled(cached.enabled);
        noise_shaper.set_curve(cached.curve);

        Self {
            noise_shaper,
            params,
            cached_params,
            cached_generation,
            cached,
            sample_rate,
            channels,
        }
    }

    pub fn refresh_is_enabled(&mut self) -> bool {
        self.sync_params();
        self.cached.enabled
    }

    pub fn process_cached(&mut self, buffer: &mut [f64], _channels: usize) -> ProcessResult {
        if !self.cached.enabled {
            return ProcessResult::Bypassed;
        }

        self.noise_shaper.process(buffer, self.channels);
        ProcessResult::Ok
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_params = current;
            self.cached_generation = generation;
            self.noise_shaper.set_enabled(self.cached.enabled);
            self.noise_shaper.set_bits(self.cached.bits);
            self.noise_shaper.set_curve(self.cached.curve);
        }
    }
}

impl AudioProcessor for NoiseShaperProcessor {
    fn name(&self) -> &'static str {
        "NoiseShaper"
    }

    fn process(&mut self, buffer: &mut [f64], _channels: usize) -> ProcessResult {
        self.sync_params();

        if !self.cached.enabled {
            return ProcessResult::Bypassed;
        }

        self.noise_shaper.process(buffer, self.channels);
        ProcessResult::Ok
    }

    fn reset(&mut self) {
        self.noise_shaper.reset();
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate as u32;
        self.noise_shaper = NoiseShaper::new(self.channels, self.sample_rate, self.cached.bits);
        self.noise_shaper.set_enabled(self.cached.enabled);
        self.noise_shaper.set_curve(self.cached.curve);
    }
}

// ============================================================================
// Dynamic Loudness Adapter
// ============================================================================

/// Dynamic loudness compensation processor
pub struct DynamicLoudnessProcessor {
    dynamic_loudness: DynamicLoudness,
    params: Arc<AtomicDynamicLoudnessParams>,
    telemetry: Arc<AtomicDynamicLoudnessTelemetry>,
    cached_params: Arc<DynamicLoudnessParamsSnapshot>,
    cached_generation: u64,
    cached: DynamicLoudnessParamsSnapshot,
    sample_rate: u32,
    channels: usize,
}

impl DynamicLoudnessProcessor {
    pub fn new(
        channels: usize,
        sample_rate: u32,
        params: Arc<AtomicDynamicLoudnessParams>,
        telemetry: Arc<AtomicDynamicLoudnessTelemetry>,
    ) -> Self {
        let (cached_params, cached_generation) = params.load_with_generation();
        let cached = *cached_params;
        let mut dynamic_loudness = DynamicLoudness::new(channels, sample_rate as f64);
        dynamic_loudness.set_volume(cached.volume);
        dynamic_loudness.set_strength(cached.strength);
        Self {
            dynamic_loudness,
            params,
            telemetry,
            cached_params,
            cached_generation,
            cached,
            sample_rate,
            channels,
        }
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_params = current;
            self.cached_generation = generation;
            self.dynamic_loudness.set_volume(self.cached.volume);
            self.dynamic_loudness.set_strength(self.cached.strength);
        }
    }
}

impl AudioProcessor for DynamicLoudnessProcessor {
    fn name(&self) -> &'static str {
        "DynamicLoudness"
    }

    fn process(&mut self, buffer: &mut [f64], _channels: usize) -> ProcessResult {
        self.sync_params();

        if !self.cached.enabled {
            self.telemetry.update(0.0, [0.0; 7]);
            return ProcessResult::Bypassed;
        }

        self.dynamic_loudness.process(buffer);
        self.telemetry.update(
            self.dynamic_loudness.loudness_factor(),
            self.dynamic_loudness.get_band_gains(),
        );
        ProcessResult::Ok
    }

    fn reset(&mut self) {
        self.dynamic_loudness.reset();
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate as u32;
        self.dynamic_loudness = DynamicLoudness::new(self.channels, self.sample_rate as f64);
    }
}

// ============================================================================
// Convolver Adapter
// ============================================================================

/// FFT convolver processor with wait-free kernel swap-in.
pub struct ConvolverProcessor {
    owned: Option<FFTConvolver>,
    swap: Arc<ArcSwapOption<FFTConvolver>>,
    enabled: Arc<AtomicBool>,
}

impl ConvolverProcessor {
    pub fn new(swap: Arc<ArcSwapOption<FFTConvolver>>, enabled: Arc<AtomicBool>) -> Self {
        Self {
            owned: None,
            swap,
            enabled,
        }
    }

    fn sync_convolver(&mut self) {
        if !self.enabled.load(Ordering::Acquire) {
            self.owned = None;
            let _ = self.swap.swap(None);
            return;
        }

        let new_conv = self.swap.swap(None);
        if let Some(arc_conv) = new_conv {
            match Arc::try_unwrap(arc_conv) {
                Ok(conv) => self.owned = Some(conv),
                Err(arc) => self.owned = Some((*arc).clone()),
            }
        }
    }
}

impl AudioProcessor for ConvolverProcessor {
    fn name(&self) -> &'static str {
        "Convolver"
    }

    fn process(&mut self, buffer: &mut [f64], _channels: usize) -> ProcessResult {
        self.sync_convolver();

        if let Some(ref mut convolver) = self.owned {
            convolver.process_inplace(buffer);
            ProcessResult::Ok
        } else {
            ProcessResult::Bypassed
        }
    }

    fn reset(&mut self) {
        if let Some(ref mut convolver) = self.owned {
            convolver.reset();
        }
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.owned = None;
        }
        self.enabled.store(enabled, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convolver_processor_swaps_in_and_processes() {
        let swap = Arc::new(ArcSwapOption::empty());
        let enabled = Arc::new(AtomicBool::new(false));
        let mut proc = ConvolverProcessor::new(Arc::clone(&swap), Arc::clone(&enabled));
        let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

        assert_eq!(proc.process(&mut buffer, 1), ProcessResult::Bypassed);

        swap.store(Some(Arc::new(FFTConvolver::new(&[0.5], 1))));
        enabled.store(true, Ordering::Release);
        assert_eq!(proc.process(&mut buffer, 1), ProcessResult::Ok);
        assert_eq!(buffer, vec![0.5, 1.0, 1.5, 2.0]);
    }

    #[test]
    fn test_convolver_processor_clear_disables_owned_convolver() {
        let swap = Arc::new(ArcSwapOption::empty());
        let enabled = Arc::new(AtomicBool::new(true));
        let mut proc = ConvolverProcessor::new(Arc::clone(&swap), Arc::clone(&enabled));
        let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

        swap.store(Some(Arc::new(FFTConvolver::new(&[0.5], 1))));
        assert_eq!(proc.process(&mut buffer, 1), ProcessResult::Ok);

        enabled.store(false, Ordering::Release);
        let mut bypassed = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(proc.process(&mut bypassed, 1), ProcessResult::Bypassed);
        assert_eq!(bypassed, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_eq_processor() {
        let params = Arc::new(AtomicEqParams::new());
        let mut proc = EqProcessor::new(2, 44100.0, Arc::clone(&params));

        // Set params from "main thread"
        let gains = [2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        params.write(&gains, true);

        // Process from "audio thread"
        let mut buffer = vec![0.5; 4096];
        let result = proc.process(&mut buffer, 2);

        assert_eq!(result, ProcessResult::Ok);
        // EQ gain smoothing may not boost the very first sample, but the block should change.
        assert!(buffer.iter().any(|&sample| (sample - 0.5).abs() > 1e-6));
    }

    #[test]
    fn test_volume_processor_muted() {
        let params = Arc::new(AtomicVolumeParams::new());
        let mut proc = VolumeProcessor::new(Arc::clone(&params));

        params.set_volume(0.5);
        params.set_muted(true);

        let mut buffer = vec![1.0; 4096];
        proc.process(&mut buffer, 2);

        // Muting uses a click-free exponential fade rather than an instant hard cut.
        assert!(buffer[0] < 1.0);
        assert!(buffer[buffer.len() - 1] < 0.001);
    }

    #[test]
    fn test_volume_processor_writes_back_smoothed_volume() {
        let params = Arc::new(AtomicVolumeParams::new());
        let mut proc = VolumeProcessor::new(Arc::clone(&params));

        params.set_volume(0.25);
        let mut buffer = vec![1.0; 128];
        proc.process(&mut buffer, 2);

        let first_pass_volume = proc.current_volume;
        assert!(first_pass_volume < 1.0);
        assert!(first_pass_volume > 0.25);

        proc.process(&mut buffer, 2);

        assert!(proc.current_volume < first_pass_volume);
        assert!(proc.current_volume > 0.25);
    }

    #[test]
    fn test_volume_processor_steady_state_fast_path_preserves_unity() {
        let params = Arc::new(AtomicVolumeParams::new());
        let mut proc = VolumeProcessor::new(Arc::clone(&params));
        proc.reset();

        let mut buffer = vec![0.25, -0.5, 0.75, -1.0];
        let original = buffer.clone();

        assert_eq!(proc.process(&mut buffer, 2), ProcessResult::Ok);
        assert_eq!(buffer, original);
        assert_eq!(proc.current_volume, 1.0);
    }

    #[test]
    fn test_volume_processor_steady_state_fast_path_applies_target() {
        let params = Arc::new(AtomicVolumeParams::new());
        params.set_volume(0.5);
        let mut proc = VolumeProcessor::new(Arc::clone(&params));
        proc.sync_params();
        proc.reset();

        let mut buffer = vec![0.25, -0.5, 0.75, -1.0];

        assert_eq!(proc.process(&mut buffer, 2), ProcessResult::Ok);
        assert_eq!(buffer, vec![0.125, -0.25, 0.375, -0.5]);
        assert_eq!(proc.current_volume, 0.5);
    }

    #[test]
    fn volume_lazy_settle_dc_null_residual_stays_below_snap_floor() {
        let input = vec![0.8; 32_768 * 2];

        assert_lazy_settle_residual_bounds("dc", &input, 2);
    }

    #[test]
    fn volume_lazy_settle_sweep_null_residual_stays_below_snap_floor() {
        let input = sweep_signal(32_768, 2);

        assert_lazy_settle_residual_bounds("sweep", &input, 2);
    }

    #[test]
    fn volume_lazy_settle_abrupt_step_null_residual_stays_below_snap_floor() {
        let input = abrupt_step_signal(32_768, 2);

        assert_lazy_settle_residual_bounds("abrupt_step", &input, 2);
    }

    #[test]
    fn test_saturation_processor() {
        let params = Arc::new(AtomicSaturationParams::new());
        let mut proc = SaturationProcessor::new(2, Arc::clone(&params));

        params.set_drive(1.0);
        params.set_mix(1.0);
        params.set_enabled(true);

        let mut buffer = vec![0.9, 0.9];
        proc.process(&mut buffer, 2);

        // tanh(0.9 * 2) ≈ 0.96, less than input
        assert!(buffer[0].abs() < 0.9 * 2.0);
    }

    fn assert_lazy_settle_residual_bounds(name: &str, input: &[f64], channels: usize) {
        const RESIDUAL_DELTA_LIMIT: f64 = 2.0e-6;
        const RESIDUAL_RMS_LIMIT: f64 = 2.0e-7;

        let mut exact = input.to_vec();
        let mut lazy = input.to_vec();
        process_volume_exact_kernel(&mut exact, channels, 48_000.0, 0.25);
        process_volume_lazy_settle_kernel(
            &mut lazy,
            channels,
            48_000.0,
            0.25,
            VolumeProcessor::SETTLE_EPSILON,
        );

        let mut max_abs = 0.0_f64;
        let mut sum_sq = 0.0_f64;
        let mut max_delta = 0.0_f64;
        let mut prev_residual = 0.0_f64;

        for (idx, (left, right)) in lazy.iter().zip(&exact).enumerate() {
            let residual = left - right;
            max_abs = max_abs.max(residual.abs());
            sum_sq += residual * residual;
            if idx > 0 {
                max_delta = max_delta.max((residual - prev_residual).abs());
            }
            prev_residual = residual;
        }

        let rms = (sum_sq / input.len() as f64).sqrt();
        assert!(
            max_abs <= VolumeProcessor::SETTLE_EPSILON,
            "{name} lazy-settle max residual {max_abs:.3e} exceeds {:.3e}",
            VolumeProcessor::SETTLE_EPSILON
        );
        assert!(
            max_delta <= RESIDUAL_DELTA_LIMIT,
            "{name} lazy-settle residual delta {max_delta:.3e} exceeds {RESIDUAL_DELTA_LIMIT:.3e}"
        );
        assert!(
            rms <= RESIDUAL_RMS_LIMIT,
            "{name} lazy-settle residual rms {rms:.3e} exceeds {RESIDUAL_RMS_LIMIT:.3e}"
        );
    }

    fn process_volume_exact_kernel(
        buffer: &mut [f64],
        channels: usize,
        sample_rate: f64,
        target: f64,
    ) -> f64 {
        let smoothing_coeff = VolumeProcessor::calc_smoothing_coeff(sample_rate);
        let one_minus_coeff = 1.0 - smoothing_coeff;
        let mut current_volume = 1.0;
        let frames = buffer.len() / channels;

        for frame in 0..frames {
            current_volume += (target - current_volume) * one_minus_coeff;
            for ch in 0..channels {
                buffer[frame * channels + ch] *= current_volume;
            }
        }

        current_volume
    }

    fn process_volume_lazy_settle_kernel(
        buffer: &mut [f64],
        channels: usize,
        sample_rate: f64,
        target: f64,
        settle_epsilon: f64,
    ) -> f64 {
        let smoothing_coeff = VolumeProcessor::calc_smoothing_coeff(sample_rate);
        let one_minus_coeff = 1.0 - smoothing_coeff;
        let mut current_volume = 1.0;
        let frames = buffer.len() / channels;
        let mut frame = 0;

        while frame < frames {
            if (target - current_volume).abs() <= settle_epsilon {
                current_volume = target;
                break;
            }

            current_volume += (target - current_volume) * one_minus_coeff;
            for ch in 0..channels {
                buffer[frame * channels + ch] *= current_volume;
            }
            frame += 1;
        }

        if frame < frames && target != 1.0 {
            for sample in &mut buffer[(frame * channels)..] {
                *sample *= target;
            }
        }

        current_volume
    }

    fn sweep_signal(frames: usize, channels: usize) -> Vec<f64> {
        let mut out = Vec::with_capacity(frames * channels);
        let sample_rate = 48_000.0;
        let start_hz = 20.0_f64;
        let end_hz = 20_000.0_f64;
        let mut phase = 0.0_f64;

        for frame in 0..frames {
            let progress = frame as f64 / frames.saturating_sub(1).max(1) as f64;
            let hz = start_hz * (end_hz / start_hz).powf(progress);
            phase += std::f64::consts::TAU * hz / sample_rate;
            let sample = phase.sin() * 0.9;
            for ch in 0..channels {
                out.push(sample * (1.0 - ch as f64 * 0.05));
            }
        }

        out
    }

    fn abrupt_step_signal(frames: usize, channels: usize) -> Vec<f64> {
        let mut out = Vec::with_capacity(frames * channels);

        for frame in 0..frames {
            let sample = match frame * 4 / frames.max(1) {
                0 => 0.0,
                1 => 1.0,
                2 => -1.0,
                _ => {
                    if frame % 2 == 0 {
                        1.0
                    } else {
                        -1.0
                    }
                }
            };
            for _ in 0..channels {
                out.push(sample);
            }
        }

        out
    }
}
