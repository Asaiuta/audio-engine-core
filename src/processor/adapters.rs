//! Processor Adapters
//!
//! Wraps existing processors with the [`StreamingProcessor`] trait, enabling
//! lock-free parameter passing and unified DSP chain management.
//!
//! Each adapter:
//! - Owns the actual processor (audio thread exclusive)
//! - References lock-free parameters (shared with main thread)
//! - Synchronizes parameters before processing

#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(test)]
use super::convolver::FFTConvolver;
use super::crossfeed::Crossfeed;
use super::dsp::NoiseShaper;
use super::dynamic_loudness::DynamicLoudness;
use super::eq::Equalizer;
use super::lockfree_params::*;
use super::loudness::PeakLimiter;
use super::saturation::Saturation;
#[cfg(test)]
use super::traits::TailSpec;
use super::traits::{
    AudioBlockMut, FrameDuration, ProcessBufferParts, ProcessBuffers, ProcessError,
    ProcessProgress, ProcessState, StreamingProcessor,
};

#[derive(Default)]
struct FixedLifecycle {
    finishing: bool,
    finished: bool,
}

impl FixedLifecycle {
    fn ensure_processing(&self, processor: &'static str) -> Result<(), ProcessError> {
        if self.finishing || self.finished {
            Err(ProcessError::AlreadyFinished { processor })
        } else {
            Ok(())
        }
    }

    fn begin_finish(&mut self) {
        self.finishing = true;
    }

    fn is_finished(&self) -> bool {
        self.finished
    }

    fn finish(&mut self) -> ProcessProgress {
        self.finishing = true;
        self.finished = true;
        ProcessProgress::finished(0)
    }

    fn reset(&mut self) {
        self.finishing = false;
        self.finished = false;
    }
}

fn validate_channels(
    processor: &'static str,
    expected_channels: Option<usize>,
    actual_channels: usize,
) -> Result<(), ProcessError> {
    if let Some(expected_channels) = expected_channels {
        if expected_channels != actual_channels {
            return Err(ProcessError::ChannelCountMismatch {
                processor,
                expected_channels,
                actual_channels,
            });
        }
    }
    Ok(())
}

fn validate_sample_rate(processor: &'static str, sample_rate_hz: u32) -> Result<f64, ProcessError> {
    if sample_rate_hz == 0 {
        Err(ProcessError::InvalidSampleRate {
            processor,
            sample_rate_hz,
        })
    } else {
        Ok(sample_rate_hz as f64)
    }
}

fn process_fixed_1_to_1<F>(
    processor: &'static str,
    enabled: bool,
    expected_channels: Option<usize>,
    buffers: ProcessBuffers<'_>,
    process: F,
) -> Result<ProcessProgress, ProcessError>
where
    F: FnOnce(&mut [f64], usize) -> Result<(), ProcessError>,
{
    let channels = buffers.channels();
    validate_channels(processor, expected_channels, channels)?;

    match buffers.into_parts() {
        ProcessBufferParts::InPlace(mut block) => {
            let frames = block.frames();
            if enabled && frames > 0 {
                process(block.samples_mut(), channels)?;
            }
            Ok(
                ProcessProgress::new(frames, frames, ProcessState::NeedInput)
                    .with_bypassed(!enabled),
            )
        }
        ProcessBufferParts::OutOfPlace { input, mut output } => {
            let frames = input.frames().min(output.frames());
            let samples = frames * channels;
            output.samples_mut()[..samples].copy_from_slice(&input.samples()[..samples]);
            if enabled && frames > 0 {
                process(&mut output.samples_mut()[..samples], channels)?;
            }
            let state = if frames < input.frames() {
                ProcessState::NeedOutput
            } else {
                ProcessState::NeedInput
            };
            Ok(ProcessProgress::new(frames, frames, state).with_bypassed(!enabled))
        }
    }
}

fn finish_fixed(
    processor: &'static str,
    expected_channels: Option<usize>,
    lifecycle: &mut FixedLifecycle,
    output: AudioBlockMut<'_>,
) -> Result<ProcessProgress, ProcessError> {
    validate_channels(processor, expected_channels, output.channels())?;
    Ok(lifecycle.finish())
}
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
    cached_generation: u64,
    /// Local parameter cache
    cached: EqParamsSnapshot,
    /// Sample rate for coefficient recalculation
    sample_rate: f64,
    lifecycle: FixedLifecycle,
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
            cached_generation,
            cached,
            sample_rate,
            lifecycle: FixedLifecycle::default(),
        }
    }

    /// Synchronize parameters from lock-free storage
    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_generation = generation;

            // Apply to internal EQ
            self.eq.set_all_bands(&self.cached.gains, self.sample_rate);
            self.eq.set_enabled(self.cached.enabled);
        }
    }
}

impl StreamingProcessor for EqProcessor {
    fn name(&self) -> &'static str {
        "Equalizer"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.lifecycle.ensure_processing("Equalizer")?;
        self.sync_params();

        process_fixed_1_to_1(
            "Equalizer",
            self.cached.enabled,
            Some(self.channels),
            buffers,
            |buffer, _channels| {
                self.eq.process(buffer);
                Ok(())
            },
        )
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        finish_fixed(
            "Equalizer",
            Some(self.channels),
            &mut self.lifecycle,
            output,
        )
    }

    fn reset(&mut self) -> Result<(), ProcessError> {
        self.eq.reset();
        self.lifecycle.reset();
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        let sample_rate = validate_sample_rate("Equalizer", sample_rate_hz)?;
        self.sample_rate = sample_rate;
        self.eq = Equalizer::new(self.channels, sample_rate);
        self.eq.set_all_bands(&self.cached.gains, sample_rate);
        self.eq.set_enabled(self.cached.enabled);
        Ok(())
    }
}

// ============================================================================
// Saturation Adapter
// ============================================================================

/// Saturation processor adapter
pub struct SaturationProcessor {
    saturation: Saturation,
    channels: usize,
    params: Arc<AtomicSaturationParams>,
    cached_generation: u64,
    cached: SaturationParamsSnapshot,
    sample_rate: f64,
    lifecycle: FixedLifecycle,
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
        saturation.set_quality(super::saturation::SaturationQuality::from(cached.quality));
        Self {
            saturation,
            channels,
            params,
            cached_generation,
            cached,
            sample_rate: 44100.0,
            lifecycle: FixedLifecycle::default(),
        }
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
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
            self.saturation
                .set_quality(super::saturation::SaturationQuality::from(
                    self.cached.quality,
                ));
        }
    }
}

impl StreamingProcessor for SaturationProcessor {
    fn name(&self) -> &'static str {
        "Saturation"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.lifecycle.ensure_processing("Saturation")?;
        self.sync_params();

        process_fixed_1_to_1(
            "Saturation",
            self.cached.enabled,
            Some(self.channels),
            buffers,
            |buffer, channels| {
                self.saturation.process_with_channels(buffer, channels);
                Ok(())
            },
        )
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        finish_fixed(
            "Saturation",
            Some(self.channels),
            &mut self.lifecycle,
            output,
        )
    }

    fn reset(&mut self) -> Result<(), ProcessError> {
        self.saturation.reset();
        self.lifecycle.reset();
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        let sample_rate = validate_sample_rate("Saturation", sample_rate_hz)?;
        self.sample_rate = sample_rate;
        self.saturation.set_sample_rate(sample_rate);
        Ok(())
    }
}

// ============================================================================
// Crossfeed Adapter
// ============================================================================

/// Crossfeed processor adapter
pub struct CrossfeedProcessor {
    crossfeed: Crossfeed,
    params: Arc<AtomicCrossfeedParams>,
    cached_generation: u64,
    cached: CrossfeedParamsSnapshot,
    sample_rate: f64,
    lifecycle: FixedLifecycle,
}

impl CrossfeedProcessor {
    pub fn new(sample_rate: f64, params: Arc<AtomicCrossfeedParams>) -> Self {
        let (cached_params, cached_generation) = params.load_with_generation();
        let cached = *cached_params;
        let mut crossfeed = Crossfeed::with_params(sample_rate, cached.cutoff_hz, cached.mix);
        crossfeed.set_enabled(cached.enabled);
        Self {
            crossfeed,
            params,
            cached_generation,
            cached,
            sample_rate,
            lifecycle: FixedLifecycle::default(),
        }
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            let previous = self.cached;
            self.cached = *current;
            self.cached_generation = generation;
            if self.cached.mix != previous.mix {
                self.crossfeed.set_mix(self.cached.mix);
            }
            if self.cached.enabled != previous.enabled {
                self.crossfeed.set_enabled(self.cached.enabled);
            }
            if self.cached.cutoff_hz != previous.cutoff_hz {
                self.crossfeed.set_cutoff(self.cached.cutoff_hz);
            }
        }
    }
}

impl StreamingProcessor for CrossfeedProcessor {
    fn name(&self) -> &'static str {
        "Crossfeed"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.lifecycle.ensure_processing("Crossfeed")?;
        self.sync_params();
        let enabled = self.cached.enabled && buffers.channels() == 2;

        process_fixed_1_to_1("Crossfeed", enabled, None, buffers, |buffer, channels| {
            self.crossfeed.process(buffer, channels);
            Ok(())
        })
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        finish_fixed("Crossfeed", None, &mut self.lifecycle, output)
    }

    fn reset(&mut self) -> Result<(), ProcessError> {
        self.crossfeed.reset();
        self.lifecycle.reset();
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        let sample_rate = validate_sample_rate("Crossfeed", sample_rate_hz)?;
        self.sample_rate = sample_rate;
        self.crossfeed
            .set_sample_rate(sample_rate, self.cached.cutoff_hz);
        Ok(())
    }
}

// ============================================================================
// Peak Limiter Adapter
// ============================================================================

/// Peak limiter processor adapter
pub struct PeakLimiterProcessor {
    limiter: PeakLimiter,
    params: Arc<AtomicPeakLimiterParams>,
    cached_generation: u64,
    cached: PeakLimiterParamsSnapshot,
    sample_rate: u32,
    channels: usize,
    lifecycle: FixedLifecycle,
    finish_remaining_frames: Option<usize>,
}

impl PeakLimiterProcessor {
    pub fn new(channels: usize, sample_rate: u32, params: Arc<AtomicPeakLimiterParams>) -> Self {
        let (cached_params, cached_generation) = params.load_with_generation();
        let cached = *cached_params;
        Self {
            limiter: PeakLimiter::with_mode(
                channels,
                sample_rate,
                cached.threshold_db,
                10.0,
                cached.release_ms,
                cached.mode,
            ),
            params,
            cached_generation,
            cached,
            sample_rate,
            channels,
            lifecycle: FixedLifecycle::default(),
            finish_remaining_frames: None,
        }
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_generation = generation;

            // In-place updates only — NO PeakLimiter::new(), NO heap allocation.
            // set_mode is allocation-free (buffers pre-sized for the worst case)
            // and resets internal state when the active window changes.
            self.limiter.set_mode(self.cached.mode);
            self.limiter.set_threshold(self.cached.threshold_db);
            self.limiter.set_release_ms(self.cached.release_ms);
            // If enabled state changed, limiter reset may be needed
            if self.cached.enabled != self.limiter.is_enabled() {
                self.limiter.reset();
            }
        }
    }

    /// Current limiter gain reduction in dB.
    pub fn gain_reduction_db(&self) -> f64 {
        self.limiter.gain_reduction_db()
    }
}

impl StreamingProcessor for PeakLimiterProcessor {
    fn name(&self) -> &'static str {
        "PeakLimiter"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.lifecycle.ensure_processing("PeakLimiter")?;
        self.sync_params();

        process_fixed_1_to_1(
            "PeakLimiter",
            self.cached.enabled,
            Some(self.channels),
            buffers,
            |buffer, _channels| {
                self.limiter.process(buffer);
                Ok(())
            },
        )
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        validate_channels("PeakLimiter", Some(self.channels), output.channels())?;
        if self.lifecycle.is_finished() {
            return Ok(ProcessProgress::finished(0));
        }

        self.lifecycle.begin_finish();
        let initial_remaining = if self.cached.enabled {
            self.limiter.delay_frames()
        } else {
            0
        };
        let remaining = self
            .finish_remaining_frames
            .get_or_insert(initial_remaining);
        if *remaining == 0 {
            return Ok(self.lifecycle.finish());
        }

        let mut output = output;
        let frames = output.frames().min(*remaining);
        let samples = frames * self.channels;
        output.samples_mut()[..samples].fill(0.0);
        if frames > 0 {
            self.limiter.process(&mut output.samples_mut()[..samples]);
        }
        *remaining -= frames;

        if *remaining == 0 {
            let _ = self.lifecycle.finish();
            Ok(ProcessProgress::finished(frames))
        } else {
            Ok(ProcessProgress::new(0, frames, ProcessState::NeedOutput))
        }
    }

    fn reset(&mut self) -> Result<(), ProcessError> {
        self.limiter.reset();
        self.lifecycle.reset();
        self.finish_remaining_frames = None;
        Ok(())
    }

    fn latency(&self) -> FrameDuration {
        if !self.cached.enabled {
            return FrameDuration::ZERO;
        }
        FrameDuration::new(self.limiter.delay_frames(), self.sample_rate)
            .unwrap_or(FrameDuration::ZERO)
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        validate_sample_rate("PeakLimiter", sample_rate_hz)?;
        self.sample_rate = sample_rate_hz;
        self.limiter = PeakLimiter::with_mode(
            self.channels,
            self.sample_rate,
            self.cached.threshold_db,
            10.0,
            self.cached.release_ms,
            self.cached.mode,
        );
        self.lifecycle.reset();
        self.finish_remaining_frames = None;
        Ok(())
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
    lifecycle: FixedLifecycle,
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
            cached_generation,
            cached,
            current_volume: 1.0,
            smoothing_coeff,
            one_minus_smoothing_coeff,
            sample_rate: 44100.0,
            lifecycle: FixedLifecycle::default(),
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
            self.cached_generation = generation;
        }
    }
}

impl StreamingProcessor for VolumeProcessor {
    fn name(&self) -> &'static str {
        "Volume"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.lifecycle.ensure_processing("Volume")?;
        self.sync_params();

        process_fixed_1_to_1("Volume", true, None, buffers, |buffer, channels| {
            // Volume is always active. Muting is a smoothed gain transition,
            // not a transparent bypass.
            if self.cached.muted {
                // Decay once per frame so every channel receives the same gain.
                let coeff = self.smoothing_coeff;
                let mut current_volume = self.current_volume;
                for frame in buffer.chunks_exact_mut(channels) {
                    current_volume *= coeff;
                    for sample in frame.iter_mut() {
                        *sample *= current_volume;
                    }
                }
                self.current_volume = current_volume;
                return Ok(());
            }

            let target = self.cached.volume;
            if self.current_volume == target {
                if target != 1.0 {
                    for sample in buffer.iter_mut() {
                        *sample *= target;
                    }
                }
                return Ok(());
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

            Ok(())
        })
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        finish_fixed("Volume", None, &mut self.lifecycle, output)
    }

    fn reset(&mut self) -> Result<(), ProcessError> {
        self.current_volume = self.cached.volume;
        self.lifecycle.reset();
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        true // Volume is always active
    }

    fn set_enabled(&mut self, _enabled: bool) {
        // Use set_muted instead
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        let sample_rate = validate_sample_rate("Volume", sample_rate_hz)?;
        if (self.sample_rate - sample_rate).abs() > 1.0 {
            self.sample_rate = sample_rate;
            self.smoothing_coeff = Self::calc_smoothing_coeff(sample_rate);
            self.one_minus_smoothing_coeff = 1.0 - self.smoothing_coeff;
        }
        Ok(())
    }
}

// ============================================================================
// Noise Shaper Adapter
// ============================================================================

/// Noise shaper processor adapter
pub struct NoiseShaperProcessor {
    noise_shaper: NoiseShaper,
    params: Arc<AtomicNoiseShaperParams>,
    cached_generation: u64,
    cached: NoiseShaperParamsSnapshot,
    sample_rate: u32,
    channels: usize,
    lifecycle: FixedLifecycle,
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
            cached_generation,
            cached,
            sample_rate,
            channels,
            lifecycle: FixedLifecycle::default(),
        }
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            let previous = self.cached;
            self.cached = *current;
            self.cached_generation = generation;
            if self.cached.enabled != previous.enabled {
                self.noise_shaper.set_enabled(self.cached.enabled);
            }
            if self.cached.bits != previous.bits {
                self.noise_shaper.set_bits(self.cached.bits);
            }
            if self.cached.curve != previous.curve {
                self.noise_shaper.set_curve(self.cached.curve);
            }
        }
    }
}

impl StreamingProcessor for NoiseShaperProcessor {
    fn name(&self) -> &'static str {
        "NoiseShaper"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.lifecycle.ensure_processing("NoiseShaper")?;
        self.sync_params();

        process_fixed_1_to_1(
            "NoiseShaper",
            self.cached.enabled,
            Some(self.channels),
            buffers,
            |buffer, _channels| {
                self.noise_shaper.process(buffer, self.channels);
                Ok(())
            },
        )
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        finish_fixed(
            "NoiseShaper",
            Some(self.channels),
            &mut self.lifecycle,
            output,
        )
    }

    fn reset(&mut self) -> Result<(), ProcessError> {
        self.noise_shaper.reset();
        self.lifecycle.reset();
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        validate_sample_rate("NoiseShaper", sample_rate_hz)?;
        self.sample_rate = sample_rate_hz;
        self.noise_shaper = NoiseShaper::new(self.channels, self.sample_rate, self.cached.bits);
        self.noise_shaper.set_enabled(self.cached.enabled);
        self.noise_shaper.set_curve(self.cached.curve);
        Ok(())
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
    cached_generation: u64,
    cached: DynamicLoudnessParamsSnapshot,
    sample_rate: u32,
    channels: usize,
    lifecycle: FixedLifecycle,
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
            cached_generation,
            cached,
            sample_rate,
            channels,
            lifecycle: FixedLifecycle::default(),
        }
    }

    fn sync_params(&mut self) {
        if let Some((current, generation)) =
            self.params.load_if_changed_since(self.cached_generation)
        {
            self.cached = *current;
            self.cached_generation = generation;
            self.dynamic_loudness.set_volume(self.cached.volume);
            self.dynamic_loudness.set_strength(self.cached.strength);
        }
    }
}

impl StreamingProcessor for DynamicLoudnessProcessor {
    fn name(&self) -> &'static str {
        "DynamicLoudness"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.lifecycle.ensure_processing("DynamicLoudness")?;
        self.sync_params();

        if !self.cached.enabled {
            self.telemetry.update(0.0, [0.0; 7]);
        }

        process_fixed_1_to_1(
            "DynamicLoudness",
            self.cached.enabled,
            Some(self.channels),
            buffers,
            |buffer, _channels| {
                self.dynamic_loudness.process(buffer);
                self.telemetry.update(
                    self.dynamic_loudness.loudness_factor(),
                    self.dynamic_loudness.get_band_gains(),
                );
                Ok(())
            },
        )
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        finish_fixed(
            "DynamicLoudness",
            Some(self.channels),
            &mut self.lifecycle,
            output,
        )
    }

    fn reset(&mut self) -> Result<(), ProcessError> {
        self.dynamic_loudness.reset();
        self.lifecycle.reset();
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.cached.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.params.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        validate_sample_rate("DynamicLoudness", sample_rate_hz)?;
        self.sample_rate = sample_rate_hz;
        self.dynamic_loudness
            .set_sample_rate(self.sample_rate as f64);
        Ok(())
    }
}

mod convolver;

#[cfg(test)]
use convolver::ConvolverDropProbe;
pub use convolver::{ConvolverControl, ConvolverProcessor, ConvolverStatus};
#[cfg(test)]
mod tests;
