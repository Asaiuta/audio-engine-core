//! Processor Adapters
//!
//! Wraps existing processors with the [`StreamingProcessor`] trait, enabling
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
use super::traits::{
    AudioBlockMut, FrameDuration, ProcessBufferParts, ProcessBuffers, ProcessError,
    ProcessProgress, ProcessState, StreamingProcessor, TailSpec,
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

// ============================================================================
// Convolver Adapter
// ============================================================================

/// FFT convolver processor with wait-free kernel swap-in.
///
/// Producer contract: publish a **uniquely-owned** `Arc<FFTConvolver>` into the
/// swap slot and drop your own handle; the audio thread adopts it only once it
/// is the sole owner (skip-and-retry otherwise — it never deep-clones).
/// Retired kernels are handed back through [`ConvolverProcessor::disposal_slot`];
/// drain that slot from a control thread (e.g. right before publishing a new
/// kernel) so large deallocations never happen on the audio thread. Without
/// draining, at most two retired kernels are parked and further kernel
/// adoptions are deferred until the slot is drained.
pub struct ConvolverProcessor {
    /// Active kernel. Held as a uniquely-owned `Arc` so retirement is a
    /// pointer hand-off instead of a reallocation.
    owned: Option<Arc<FFTConvolver>>,
    /// Kernel taken from `swap` but not yet adoptable (producer still holds a
    /// handle, or retirement stages are full).
    incoming: Option<Arc<FFTConvolver>>,
    /// Retired kernel waiting for the disposal slot to free up.
    pending_retire: Option<Arc<FFTConvolver>>,
    swap: Arc<ArcSwapOption<FFTConvolver>>,
    enabled: Arc<AtomicBool>,
    /// Single-slot hand-off of retired kernels to the control side.
    retired: Arc<ArcSwapOption<FFTConvolver>>,
    lifecycle: FixedLifecycle,
    sample_rate_hz: u32,
    finish_remaining_frames: Option<usize>,
}

impl ConvolverProcessor {
    pub fn new(swap: Arc<ArcSwapOption<FFTConvolver>>, enabled: Arc<AtomicBool>) -> Self {
        Self {
            owned: None,
            incoming: None,
            pending_retire: None,
            swap,
            enabled,
            retired: Arc::new(ArcSwapOption::empty()),
            lifecycle: FixedLifecycle::default(),
            sample_rate_hz: 44_100,
            finish_remaining_frames: None,
        }
    }

    /// Slot receiving kernels retired by the audio thread. Drain it (e.g.
    /// `slot.swap(None)`) from a control thread; dropping the drained `Arc`
    /// there performs the large deallocation off the audio path.
    pub fn disposal_slot(&self) -> Arc<ArcSwapOption<FFTConvolver>> {
        Arc::clone(&self.retired)
    }

    /// Move `pending_retire` into the disposal slot when it is free.
    /// Audio thread is the only writer of `retired`; the control side only
    /// takes, so an observed-empty slot cannot be concurrently filled.
    fn try_flush_retired(&mut self) {
        if let Some(arc) = self.pending_retire.take() {
            if self.retired.load().is_none() {
                self.retired.store(Some(arc));
            } else {
                self.pending_retire = Some(arc);
            }
        }
    }

    fn sync_convolver(&mut self) {
        self.try_flush_retired();

        // Withdraw any newly published kernel exactly once; keep it parked in
        // `incoming` until it is adoptable.
        if self.incoming.is_none() {
            self.incoming = self.swap.swap(None);
        }

        if !self.enabled.load(Ordering::Acquire) {
            // Retire everything we hold, one stage per block if needed, without
            // deallocating on the audio thread.
            if self.pending_retire.is_none() {
                if let Some(arc) = self.owned.take().or_else(|| self.incoming.take()) {
                    self.pending_retire = Some(arc);
                    self.try_flush_retired();
                }
            }
            return;
        }

        let Some(mut arc) = self.incoming.take() else {
            return;
        };
        if Arc::get_mut(&mut arc).is_none() {
            // Producer still holds a handle; retry next block instead of
            // deep-cloning multi-MB kernel state on the audio thread.
            self.incoming = Some(arc);
            return;
        }
        match self.owned.take() {
            None => self.owned = Some(arc),
            Some(old) => {
                if self.pending_retire.is_none() {
                    self.owned = Some(arc);
                    self.pending_retire = Some(old);
                    self.try_flush_retired();
                } else {
                    // Both retirement stages occupied: keep the old kernel and
                    // defer adoption until the control side drains.
                    self.owned = Some(old);
                    self.incoming = Some(arc);
                }
            }
        }
    }
}

impl StreamingProcessor for ConvolverProcessor {
    fn name(&self) -> &'static str {
        "Convolver"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.lifecycle.ensure_processing("Convolver")?;
        self.sync_convolver();

        if !self.enabled.load(Ordering::Acquire) {
            return process_fixed_1_to_1("Convolver", false, None, buffers, |_, _| Ok(()));
        }
        let Some(arc) = self.owned.as_mut() else {
            return process_fixed_1_to_1("Convolver", false, None, buffers, |_, _| Ok(()));
        };
        let Some(convolver) = Arc::get_mut(arc) else {
            return Err(ProcessError::Backend {
                processor: "Convolver",
                operation: "process",
                message: "owned kernel is not uniquely held",
            });
        };
        let channels = convolver.channels();
        process_fixed_1_to_1(
            "Convolver",
            true,
            Some(channels),
            buffers,
            |buffer, _channels| {
                convolver.process_inplace(buffer);
                Ok(())
            },
        )
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        let channels = self.owned.as_ref().map(|convolver| convolver.channels());
        validate_channels("Convolver", channels, output.channels())?;
        if self.lifecycle.is_finished() {
            return Ok(ProcessProgress::finished(0));
        }

        self.lifecycle.begin_finish();
        let initial_remaining = if self.enabled.load(Ordering::Acquire) {
            self.owned
                .as_ref()
                .map(|convolver| convolver.ir_length().saturating_sub(1))
                .unwrap_or(0)
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
        let samples = frames * output.channels();
        output.samples_mut()[..samples].fill(0.0);
        if frames > 0 {
            let Some(arc) = self.owned.as_mut() else {
                return Err(ProcessError::Backend {
                    processor: "Convolver",
                    operation: "finish",
                    message: "active kernel disappeared during finish",
                });
            };
            let Some(convolver) = Arc::get_mut(arc) else {
                return Err(ProcessError::Backend {
                    processor: "Convolver",
                    operation: "finish",
                    message: "owned kernel is not uniquely held",
                });
            };
            convolver.process_inplace(&mut output.samples_mut()[..samples]);
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
        if let Some(arc) = self.owned.as_mut() {
            let Some(convolver) = Arc::get_mut(arc) else {
                return Err(ProcessError::Backend {
                    processor: "Convolver",
                    operation: "reset",
                    message: "owned kernel is not uniquely held",
                });
            };
            convolver.reset();
        }
        self.lifecycle.reset();
        self.finish_remaining_frames = None;
        Ok(())
    }

    fn tail(&self) -> TailSpec {
        if !self.enabled.load(Ordering::Acquire) {
            return TailSpec::None;
        }
        let frames = self
            .owned
            .as_ref()
            .map(|convolver| convolver.ir_length().saturating_sub(1))
            .unwrap_or(0);
        TailSpec::finite(frames, self.sample_rate_hz).unwrap_or(TailSpec::Unknown)
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn set_enabled(&mut self, enabled: bool) {
        // Kernel teardown is handled by `sync_convolver`'s disabled path so the
        // audio thread never deallocates it here.
        self.enabled.store(enabled, Ordering::Release);
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        validate_sample_rate("Convolver", sample_rate_hz)?;
        self.sample_rate_hz = sample_rate_hz;
        self.lifecycle.reset();
        self.finish_remaining_frames = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processor::loudness::LimiterMode;
    use crate::processor::traits::AudioBlockRef;

    struct TestProgress(ProcessProgress);

    impl TestProgress {
        fn is_bypassed(&self) -> bool {
            self.0.is_bypassed()
        }
    }

    macro_rules! impl_test_process_block {
        ($($processor:ty),+ $(,)?) => {
            $(
                impl $processor {
                    fn process(
                        &mut self,
                        buffer: &mut [f64],
                        channels: usize,
                    ) -> TestProgress {
                        let block = AudioBlockMut::new(buffer, channels).unwrap();
                        TestProgress(
                            super::super::traits::process_checked(
                                self,
                                ProcessBuffers::in_place(block),
                            )
                            .unwrap(),
                        )
                    }
                }
            )+
        };
    }

    impl_test_process_block!(
        EqProcessor,
        SaturationProcessor,
        CrossfeedProcessor,
        PeakLimiterProcessor,
        VolumeProcessor,
        ConvolverProcessor,
        NoiseShaperProcessor,
    );

    #[test]
    fn test_convolver_processor_swaps_in_and_processes() {
        let swap = Arc::new(ArcSwapOption::empty());
        let enabled = Arc::new(AtomicBool::new(false));
        let mut proc = ConvolverProcessor::new(Arc::clone(&swap), Arc::clone(&enabled));
        let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

        assert!(proc.process(&mut buffer, 1).is_bypassed());

        swap.store(Some(Arc::new(FFTConvolver::new(&[0.5], 1))));
        enabled.store(true, Ordering::Release);
        assert!(!proc.process(&mut buffer, 1).is_bypassed());
        assert_eq!(buffer, vec![0.5, 1.0, 1.5, 2.0]);
    }

    #[test]
    fn test_convolver_processor_clear_disables_owned_convolver() {
        let swap = Arc::new(ArcSwapOption::empty());
        let enabled = Arc::new(AtomicBool::new(true));
        let mut proc = ConvolverProcessor::new(Arc::clone(&swap), Arc::clone(&enabled));
        let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

        swap.store(Some(Arc::new(FFTConvolver::new(&[0.5], 1))));
        assert!(!proc.process(&mut buffer, 1).is_bypassed());

        enabled.store(false, Ordering::Release);
        let mut bypassed = vec![1.0, 2.0, 3.0, 4.0];
        assert!(proc.process(&mut bypassed, 1).is_bypassed());
        assert_eq!(bypassed, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn convolver_processor_skips_shared_kernel_and_adopts_once_unique() {
        let swap = Arc::new(ArcSwapOption::empty());
        let enabled = Arc::new(AtomicBool::new(true));
        let mut proc = ConvolverProcessor::new(Arc::clone(&swap), Arc::clone(&enabled));
        let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

        // Producer still holds a handle: the kernel must NOT be adopted (and
        // must never be deep-cloned on the audio side).
        let kernel = Arc::new(FFTConvolver::new(&[0.5], 1));
        swap.store(Some(Arc::clone(&kernel)));
        assert!(proc.process(&mut buffer, 1).is_bypassed());
        assert_eq!(buffer, vec![1.0, 2.0, 3.0, 4.0]);

        // Producer drops its handle: the parked kernel is adopted next block.
        drop(kernel);
        assert!(!proc.process(&mut buffer, 1).is_bypassed());
        assert_eq!(buffer, vec![0.5, 1.0, 1.5, 2.0]);
    }

    #[test]
    fn convolver_processor_defers_adoption_until_disposal_slot_drained() {
        let swap = Arc::new(ArcSwapOption::empty());
        let enabled = Arc::new(AtomicBool::new(true));
        let mut proc = ConvolverProcessor::new(Arc::clone(&swap), Arc::clone(&enabled));
        let slot = proc.disposal_slot();

        let process_gain = |proc: &mut ConvolverProcessor| {
            let mut buffer = vec![1.0, 1.0, 1.0, 1.0];
            proc.process(&mut buffer, 1);
            buffer[0]
        };

        // A adopted; B retires A into the slot; C parks B in pending_retire.
        swap.store(Some(Arc::new(FFTConvolver::new(&[1.0], 1))));
        assert_eq!(process_gain(&mut proc), 1.0);
        swap.store(Some(Arc::new(FFTConvolver::new(&[0.5], 1))));
        assert_eq!(process_gain(&mut proc), 0.5);
        assert!(slot.load().is_some());
        swap.store(Some(Arc::new(FFTConvolver::new(&[0.25], 1))));
        assert_eq!(process_gain(&mut proc), 0.25);

        // Both retirement stages occupied: D's adoption is deferred and the
        // current kernel keeps processing.
        swap.store(Some(Arc::new(FFTConvolver::new(&[0.125], 1))));
        assert_eq!(process_gain(&mut proc), 0.25);

        // Control side drains the slot: the deferred kernel lands.
        assert!(slot.swap(None).is_some());
        assert_eq!(process_gain(&mut proc), 0.125);
    }

    #[test]
    fn convolver_processor_kernel_swap_is_allocation_free_on_audio_side() {
        let swap = Arc::new(ArcSwapOption::empty());
        let enabled = Arc::new(AtomicBool::new(true));
        let mut proc = ConvolverProcessor::new(Arc::clone(&swap), Arc::clone(&enabled));
        let slot = proc.disposal_slot();
        let mut buffer = vec![0.3; 512];

        for _ in 0..8 {
            // Control side: publishing allocates (allowed).
            swap.store(Some(Arc::new(FFTConvolver::new(&[0.5, 0.25], 1))));
            // Audio side: swap-in, retirement hand-off, and processing must not
            // allocate or deallocate.
            assert_no_alloc::assert_no_alloc(|| {
                proc.process(&mut buffer, 1);
            });
            // Control side: draining performs the large deallocation.
            drop(slot.swap(None));
        }
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

        assert!(!result.is_bypassed());
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
    fn test_volume_processor_muted_fade_is_frame_coherent() {
        // The muted fade must decay per frame, not per sample: both channels of
        // a stereo frame must receive the identical gain. A per-sample decay
        // would give L and R different gains (inter-channel skew) and halve the
        // fade time constant.
        let params = Arc::new(AtomicVolumeParams::new());
        let mut proc = VolumeProcessor::new(Arc::clone(&params));

        params.set_muted(true);

        let channels = 2;
        let mut buffer = vec![1.0; channels * 512];
        proc.process(&mut buffer, channels);

        for frame in buffer.chunks_exact(channels) {
            assert_eq!(
                frame[0], frame[1],
                "L and R of the same frame must share one gain"
            );
        }
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
        proc.reset().unwrap();

        let mut buffer = vec![0.25, -0.5, 0.75, -1.0];
        let original = buffer.clone();

        assert!(!proc.process(&mut buffer, 2).is_bypassed());
        assert_eq!(buffer, original);
        assert_eq!(proc.current_volume, 1.0);
    }

    #[test]
    fn test_volume_processor_steady_state_fast_path_applies_target() {
        let params = Arc::new(AtomicVolumeParams::new());
        params.set_volume(0.5);
        let mut proc = VolumeProcessor::new(Arc::clone(&params));
        proc.sync_params();
        proc.reset().unwrap();

        let mut buffer = vec![0.25, -0.5, 0.75, -1.0];

        assert!(!proc.process(&mut buffer, 2).is_bypassed());
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

    #[test]
    fn crossfeed_processor_mix_change_preserves_filter_history() {
        let params = Arc::new(AtomicCrossfeedParams::new());
        let mut proc = CrossfeedProcessor::new(48_000.0, Arc::clone(&params));
        let mut reference = Crossfeed::with_params(48_000.0, 700.0, 0.35);
        let mut reset_reference = Crossfeed::with_params(48_000.0, 700.0, 0.35);

        let warm = hard_panned_sine(2048, 0, 48_000.0, 997.0);
        let mut proc_warm = warm.clone();
        let mut ref_warm = warm.clone();
        let mut reset_warm = warm;
        proc.process(&mut proc_warm, 2);
        reference.process(&mut ref_warm, 2);
        reset_reference.process(&mut reset_warm, 2);

        params.set_mix(0.7);
        reference.set_mix(0.7);
        reset_reference.set_mix(0.7);
        reset_reference.set_sample_rate(48_000.0, 700.0);

        let next = hard_panned_sine(256, 2048, 48_000.0, 997.0);
        let mut proc_next = next.clone();
        let mut ref_next = next.clone();
        let mut reset_next = next;
        assert!(!proc.process(&mut proc_next, 2).is_bypassed());
        reference.process(&mut ref_next, 2);
        reset_reference.process(&mut reset_next, 2);

        let max_reference_delta = max_abs_delta(&proc_next, &ref_next);
        let max_reset_delta = max_abs_delta(&proc_next, &reset_next);
        assert!(
            max_reference_delta <= 1.0e-12,
            "mix change should preserve Bauer filter state, max_reference_delta={max_reference_delta:.3e}"
        );
        assert!(
            max_reset_delta > 1.0e-4,
            "test signal should distinguish reset history, max_reset_delta={max_reset_delta:.3e}"
        );
    }

    #[test]
    fn crossfeed_processor_cutoff_change_preserves_filter_history() {
        let params = Arc::new(AtomicCrossfeedParams::new());
        let mut proc = CrossfeedProcessor::new(48_000.0, Arc::clone(&params));
        let mut reference = Crossfeed::with_params(48_000.0, 700.0, 0.35);
        let mut reset_reference = Crossfeed::with_params(48_000.0, 700.0, 0.35);

        let warm = hard_panned_sine(2048, 0, 48_000.0, 431.0);
        let mut proc_warm = warm.clone();
        let mut reference_warm = warm.clone();
        let mut reset_warm = warm;
        proc.process(&mut proc_warm, 2);
        reference.process(&mut reference_warm, 2);
        reset_reference.process(&mut reset_warm, 2);

        params.set_cutoff(1_100.0);
        reference.set_cutoff(1_100.0);
        reset_reference.set_sample_rate(48_000.0, 1_100.0);

        let next = hard_panned_sine(512, 2048, 48_000.0, 431.0);
        let mut proc_next = next.clone();
        let mut reference_next = next.clone();
        let mut reset_next = next;
        proc.process(&mut proc_next, 2);
        reference.process(&mut reference_next, 2);
        reset_reference.process(&mut reset_next, 2);

        let max_reference_delta = max_abs_delta(&proc_next, &reference_next);
        let max_reset_delta = max_abs_delta(&proc_next, &reset_next);
        assert!(
            max_reference_delta <= 1.0e-12,
            "cutoff change should preserve and ramp Bauer state, max_reference_delta={max_reference_delta:.3e}"
        );
        assert!(
            max_reset_delta > 1.0e-4,
            "test signal should distinguish reset history, max_reset_delta={max_reset_delta:.3e}"
        );
    }

    #[test]
    fn crossfeed_processor_steady_state_process_is_allocation_free() {
        let params = Arc::new(AtomicCrossfeedParams::new());
        let mut proc = CrossfeedProcessor::new(48_000.0, Arc::clone(&params));
        let mut buffer = hard_panned_sine(512, 0, 48_000.0, 997.0);

        proc.process(&mut buffer, 2);

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..200 {
                proc.process(&mut buffer, 2);
            }
        });
    }

    #[test]
    fn noise_shaper_bits_change_does_not_reset_unchanged_curve_history() {
        let params = Arc::new(AtomicNoiseShaperParams::new());
        let mut processor = NoiseShaperProcessor::new(2, 48_000, Arc::clone(&params));
        let mut reference = NoiseShaper::new(2, 48_000, 24);
        reference.set_curve(params.curve());

        let mut warm = hard_panned_sine(2048, 0, 48_000.0, 997.0);
        let mut reference_warm = warm.clone();
        processor.process(&mut warm, 2);
        reference.process(&mut reference_warm, 2);
        assert_eq!(warm, reference_warm);

        params.set_bits(16);
        reference.set_bits(16);
        let mut next = hard_panned_sine(512, 2048, 48_000.0, 997.0);
        let mut reference_next = next.clone();
        processor.process(&mut next, 2);
        reference.process(&mut reference_next, 2);

        assert_eq!(next, reference_next);
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

    fn hard_panned_sine(
        frames: usize,
        start_frame: usize,
        sample_rate: f64,
        frequency: f64,
    ) -> Vec<f64> {
        let mut out = Vec::with_capacity(frames * 2);
        let omega = std::f64::consts::TAU * frequency / sample_rate;
        for frame in start_frame..start_frame + frames {
            out.push((omega * frame as f64).sin() * 0.8);
            out.push(0.0);
        }
        out
    }

    fn max_abs_delta(left: &[f64], right: &[f64]) -> f64 {
        left.iter()
            .zip(right)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn fixed_bypass_copies_out_of_place_and_reports_backpressure() {
        let params = Arc::new(AtomicEqParams::new());
        params.write(&[0.0; EQ_BANDS], false);
        let mut proc = EqProcessor::new(2, 48_000.0, params);
        let input = [0.1, -0.2, 0.3, -0.4];
        let mut output = [9.0, 9.0];

        let buffers = ProcessBuffers::out_of_place(
            AudioBlockRef::new(&input, 2).unwrap(),
            AudioBlockMut::new(&mut output, 2).unwrap(),
        )
        .unwrap();
        let progress = super::super::traits::process_checked(&mut proc, buffers).unwrap();

        assert_eq!(progress.consumed_frames(), 1);
        assert_eq!(progress.produced_frames(), 1);
        assert_eq!(progress.state(), ProcessState::NeedOutput);
        assert!(progress.is_bypassed());
        assert_eq!(output, input[..2]);
    }

    #[test]
    fn fixed_out_of_place_matches_in_place_processing() {
        let params = Arc::new(AtomicVolumeParams::new());
        params.set_volume(0.5);
        let mut in_place = VolumeProcessor::new(Arc::clone(&params));
        let mut out_of_place = VolumeProcessor::new(params);
        in_place.reset().unwrap();
        out_of_place.reset().unwrap();

        let input = [0.25, -0.5, 0.75, -1.0];
        let mut expected = input;
        let _ = in_place.process(&mut expected, 2);
        let mut actual = [0.0; 4];
        let buffers = ProcessBuffers::out_of_place(
            AudioBlockRef::new(&input, 2).unwrap(),
            AudioBlockMut::new(&mut actual, 2).unwrap(),
        )
        .unwrap();
        let progress = super::super::traits::process_checked(&mut out_of_place, buffers).unwrap();

        assert_eq!(progress.consumed_frames(), 2);
        assert_eq!(progress.produced_frames(), 2);
        assert_eq!(progress.state(), ProcessState::NeedInput);
        assert!(!progress.is_bypassed());
        assert_eq!(actual, expected);
    }

    #[test]
    fn fixed_finish_requires_reset_before_more_input() {
        let params = Arc::new(AtomicVolumeParams::new());
        let mut proc = VolumeProcessor::new(params);
        let mut finish_output = [0.0; 2];
        let finished = super::super::traits::finish_checked(
            &mut proc,
            AudioBlockMut::new(&mut finish_output, 2).unwrap(),
        )
        .unwrap();
        assert_eq!(finished.state(), ProcessState::Finished);

        let mut input = [0.25, -0.25];
        let block = AudioBlockMut::new(&mut input, 2).unwrap();
        assert_eq!(
            super::super::traits::process_checked(&mut proc, ProcessBuffers::in_place(block),),
            Err(ProcessError::AlreadyFinished {
                processor: "Volume",
            })
        );

        proc.reset().unwrap();
        let _ = proc.process(&mut input, 2);
    }

    #[test]
    fn configured_channel_count_is_validated_before_processing() {
        let params = Arc::new(AtomicNoiseShaperParams::new());
        let mut proc = NoiseShaperProcessor::new(2, 48_000, params);
        let mut mono = [0.25; 4];
        let block = AudioBlockMut::new(&mut mono, 1).unwrap();

        assert_eq!(
            super::super::traits::process_checked(&mut proc, ProcessBuffers::in_place(block),),
            Err(ProcessError::ChannelCountMismatch {
                processor: "NoiseShaper",
                expected_channels: 2,
                actual_channels: 1,
            })
        );
    }

    #[test]
    fn fixed_out_of_place_processing_is_allocation_free_after_setup() {
        let params = Arc::new(AtomicVolumeParams::new());
        params.set_volume(0.5);
        let mut proc = VolumeProcessor::new(params);
        proc.reset().unwrap();
        let input = [0.25; 512 * 2];
        let mut output = [0.0; 512 * 2];

        assert_no_alloc::assert_no_alloc(|| {
            let buffers = ProcessBuffers::out_of_place(
                AudioBlockRef::new(&input, 2).unwrap(),
                AudioBlockMut::new(&mut output, 2).unwrap(),
            )
            .unwrap();
            let _ = super::super::traits::process_checked(&mut proc, buffers).unwrap();
        });
    }

    #[test]
    fn peak_limiter_processor_defaults_to_true_peak_mode() {
        let params = Arc::new(AtomicPeakLimiterParams::new());
        let proc = PeakLimiterProcessor::new(2, 48_000, Arc::clone(&params));
        assert_eq!(proc.limiter.mode(), LimiterMode::TruePeak);
    }

    #[test]
    fn peak_limiter_processor_applies_mode_snapshot() {
        let params = Arc::new(AtomicPeakLimiterParams::new());
        let mut proc = PeakLimiterProcessor::new(2, 48_000, Arc::clone(&params));
        assert_eq!(proc.limiter.mode(), LimiterMode::TruePeak);

        // Control thread switches mode; the snapshot is applied on the next
        // process() sync.
        params.set_mode(LimiterMode::SamplePeak);
        let mut buffer = vec![0.25; 256 * 2];
        proc.process(&mut buffer, 2);
        assert_eq!(proc.limiter.mode(), LimiterMode::SamplePeak);

        params.set_mode(LimiterMode::TruePeak);
        proc.process(&mut buffer, 2);
        assert_eq!(proc.limiter.mode(), LimiterMode::TruePeak);
    }

    #[test]
    fn peak_limiter_processor_mode_switch_is_allocation_free_in_process() {
        let params = Arc::new(AtomicPeakLimiterParams::new());
        let mut proc = PeakLimiterProcessor::new(2, 48_000, Arc::clone(&params));
        let mut buffer = vec![0.3; 256 * 2];
        // Warm up the cached generation so the first asserted block is steady.
        proc.process(&mut buffer, 2);

        // Flipping the atomic mode is a control-plane call (its rcu publish
        // allocates a fresh snapshot), so it stays outside the no-alloc guard.
        // Consuming the flip and processing on the audio side must not
        // allocate: the limiter switches in place.
        for i in 0..200 {
            let mode = if i % 2 == 0 {
                LimiterMode::SamplePeak
            } else {
                LimiterMode::TruePeak
            };
            params.set_mode(mode);
            assert_no_alloc::assert_no_alloc(|| {
                proc.process(&mut buffer, 2);
            });
        }
    }

    #[test]
    fn peak_limiter_processor_disabled_bypasses() {
        let params = Arc::new(AtomicPeakLimiterParams::new());
        let mut proc = PeakLimiterProcessor::new(2, 48_000, Arc::clone(&params));

        params.set_enabled(false);
        let mut buffer = vec![1.5; 256 * 2];
        let original = buffer.clone();
        let result = proc.process(&mut buffer, 2);

        assert!(result.is_bypassed());
        assert_eq!(buffer, original);
    }

    #[test]
    fn dynamic_loudness_sample_rate_change_preserves_published_controls() {
        let params = Arc::new(AtomicDynamicLoudnessParams::new());
        params.set_ref_volume_db(-30.0);
        params.set_strength(0.37);
        let telemetry = Arc::new(AtomicDynamicLoudnessTelemetry::new());
        let mut proc = DynamicLoudnessProcessor::new(2, 48_000, params, telemetry);
        let factor = proc.dynamic_loudness.loudness_factor();

        proc.set_sample_rate(96_000).unwrap();

        assert_eq!(proc.sample_rate, 96_000);
        assert_eq!(proc.dynamic_loudness.strength(), 0.37);
        assert_eq!(proc.dynamic_loudness.loudness_factor(), factor);
    }

    #[test]
    fn peak_limiter_finish_releases_exact_algorithmic_delay() {
        let params = Arc::new(AtomicPeakLimiterParams::new());
        let mut proc = PeakLimiterProcessor::new(1, 48_000, params);
        let latency_frames = proc.limiter.delay_frames();
        let mut input = vec![0.0; 64];
        input[63] = 0.5;
        let _ = proc.process(&mut input, 1);
        assert!(input.iter().all(|sample| *sample == 0.0));
        assert_eq!(proc.latency().frames(), latency_frames);
        assert_eq!(proc.tail(), TailSpec::None);

        let mut drained = Vec::new();
        let mut scratch = vec![0.0; 37];
        loop {
            let progress = super::super::traits::finish_checked(
                &mut proc,
                AudioBlockMut::new(&mut scratch, 1).unwrap(),
            )
            .unwrap();
            drained.extend_from_slice(&scratch[..progress.produced_frames()]);
            if progress.state() == ProcessState::Finished {
                break;
            }
        }

        assert_eq!(drained.len(), latency_frames);
        assert!((drained[latency_frames - 1] - 0.5).abs() <= 1.0e-12);
        assert_eq!(
            super::super::traits::finish_checked(
                &mut proc,
                AudioBlockMut::new(&mut scratch, 1).unwrap(),
            )
            .unwrap(),
            ProcessProgress::finished(0)
        );
    }

    #[test]
    fn convolver_finish_preserves_last_frame_impulse_tail() {
        let swap = Arc::new(ArcSwapOption::empty());
        let enabled = Arc::new(AtomicBool::new(true));
        swap.store(Some(Arc::new(FFTConvolver::new(&[1.0, 0.5, 0.25], 1))));
        let mut proc = ConvolverProcessor::new(swap, enabled);
        proc.set_sample_rate(48_000).unwrap();

        let mut input = vec![0.0, 0.0, 0.0, 1.0];
        let _ = proc.process(&mut input, 1);
        assert!((input[3] - 1.0).abs() <= 1.0e-12);
        assert_eq!(proc.tail(), TailSpec::finite(2, 48_000).unwrap());

        let mut scratch = [0.0; 1];
        let first = super::super::traits::finish_checked(
            &mut proc,
            AudioBlockMut::new(&mut scratch, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(first.state(), ProcessState::NeedOutput);
        assert!((scratch[0] - 0.5).abs() <= 1.0e-12);

        let second = super::super::traits::finish_checked(
            &mut proc,
            AudioBlockMut::new(&mut scratch, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(second.state(), ProcessState::Finished);
        assert!((scratch[0] - 0.25).abs() <= 1.0e-12);
    }

    #[test]
    fn finite_finish_paths_are_allocation_free_after_processing() {
        let limiter_params = Arc::new(AtomicPeakLimiterParams::new());
        let mut limiter = PeakLimiterProcessor::new(1, 48_000, limiter_params);
        let mut limiter_input = vec![0.25; 64];
        let _ = limiter.process(&mut limiter_input, 1);
        let mut limiter_output = vec![0.0; limiter.limiter.delay_frames()];

        let swap = Arc::new(ArcSwapOption::empty());
        let enabled = Arc::new(AtomicBool::new(true));
        swap.store(Some(Arc::new(FFTConvolver::new(&[1.0, 0.5, 0.25], 1))));
        let mut convolver = ConvolverProcessor::new(swap, enabled);
        let mut convolver_input = [1.0, 0.0];
        let _ = convolver.process(&mut convolver_input, 1);
        let mut convolver_output = [0.0; 2];

        assert_no_alloc::assert_no_alloc(|| {
            let limiter_progress = super::super::traits::finish_checked(
                &mut limiter,
                AudioBlockMut::new(&mut limiter_output, 1).unwrap(),
            )
            .unwrap();
            assert_eq!(limiter_progress.state(), ProcessState::Finished);

            let convolver_progress = super::super::traits::finish_checked(
                &mut convolver,
                AudioBlockMut::new(&mut convolver_output, 1).unwrap(),
            )
            .unwrap();
            assert_eq!(convolver_progress.state(), ProcessState::Finished);
        });
    }
}
