//! Processor Adapters
//!
//! Wraps existing processors with the [`StreamingProcessor`] trait, enabling
//! lock-free parameter passing and unified DSP chain management.
//!
//! Each adapter:
//! - Owns the actual processor (audio thread exclusive)
//! - References lock-free parameters (shared with main thread)
//! - Synchronizes parameters before processing

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

/// Allocation-free snapshot of dynamic convolver publication and reclamation.
///
/// Counter fields are monotonic. A snapshot taken concurrently with control or
/// audio work may observe a transient intermediate combination, but every field
/// converges without requiring a lock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConvolverStatus {
    /// Current block-boundary enable switch.
    pub enabled: bool,
    /// Most recently installed control-side publication generation.
    pub latest_published_generation: u64,
    /// Most recently adopted audio-side generation.
    pub latest_adopted_generation: u64,
    /// Publications adopted by the audio consumer.
    pub adopted_kernels: u64,
    /// Publications replaced before the audio consumer withdrew them.
    pub superseded_kernels: u64,
    /// Withdrawn publications discarded because the processor was disabled.
    pub discarded_kernels: u64,
    /// Kernels transferred into fixed audio-side retirement staging.
    pub retired_kernels: u64,
    /// Retired kernels destroyed by a control/offline caller.
    pub reclaimed_kernels: u64,
    /// Number of transitions into a deferred-adoption/backpressure episode.
    pub deferred_adoptions: u64,
    /// Published or withdrawn kernels not yet adopted, superseded, or discarded.
    pub pending_kernels: u64,
    /// Retired kernels still in the fixed hand-off or pending-retire slots.
    pub pending_reclamations: u64,
    /// Whether fixed retirement capacity currently delays lifecycle progress.
    pub backpressured: bool,
    /// Whether the audio consumer currently owns no kernel or pending hand-off.
    pub audio_idle: bool,
}

impl ConvolverStatus {
    /// True once a disabled audio consumer holds no kernel and control has
    /// reclaimed every retired kernel. Stop concurrent publishers first; the
    /// chain may then be destroyed off RT while publication remains stopped.
    pub fn is_quiescent(self) -> bool {
        !self.enabled
            && self.audio_idle
            && self.pending_kernels == 0
            && self.pending_reclamations == 0
            && !self.backpressured
    }
}

struct PublishedConvolver {
    generation: u64,
    kernel: FFTConvolver,
    #[cfg(test)]
    _drop_probe: Option<ConvolverDropProbe>,
}

#[cfg(test)]
struct ConvolverDropProbe {
    audio_thread_id: std::thread::ThreadId,
    dropped_on_audio: Arc<AtomicBool>,
    drop_count: Arc<AtomicU64>,
}

#[cfg(test)]
impl Drop for ConvolverDropProbe {
    fn drop(&mut self) {
        if std::thread::current().id() == self.audio_thread_id {
            self.dropped_on_audio.store(true, Ordering::Release);
        }
        self.drop_count.fetch_add(1, Ordering::AcqRel);
    }
}

struct ConvolverControlInner {
    control_gate: Mutex<()>,
    published: ArcSwapOption<PublishedConvolver>,
    enabled: AtomicBool,
    retired: ArcSwapOption<PublishedConvolver>,
    latest_published_generation: AtomicU64,
    latest_adopted_generation: AtomicU64,
    adopted_kernels: AtomicU64,
    superseded_kernels: AtomicU64,
    discarded_kernels: AtomicU64,
    retired_kernels: AtomicU64,
    reclaimed_kernels: AtomicU64,
    deferred_adoptions: AtomicU64,
    backpressured: AtomicBool,
    audio_idle: AtomicBool,
}

/// Control-thread handle for dynamic convolver publication and reclamation.
///
/// One handle may have multiple control-side clones but exactly one live audio
/// consumer. Build kernels and call [`ConvolverControl::publish`] and
/// [`ConvolverControl::reclaim_retired`] off the realtime thread. Publication
/// is latest-wins until audio withdraws a kernel; once withdrawn, ownership is
/// never dropped or deep-cloned by the audio thread. Concurrent control-side
/// publishers/reclaimers are serialized internally; the audio path never
/// acquires that control-only lock.
#[derive(Clone)]
pub struct ConvolverControl {
    inner: Arc<ConvolverControlInner>,
}

impl ConvolverControl {
    /// Create a control handle with no published kernel.
    pub fn new(enabled: bool) -> Self {
        Self {
            inner: Arc::new(ConvolverControlInner {
                control_gate: Mutex::new(()),
                published: ArcSwapOption::empty(),
                enabled: AtomicBool::new(enabled),
                retired: ArcSwapOption::empty(),
                latest_published_generation: AtomicU64::new(0),
                latest_adopted_generation: AtomicU64::new(0),
                adopted_kernels: AtomicU64::new(0),
                superseded_kernels: AtomicU64::new(0),
                discarded_kernels: AtomicU64::new(0),
                retired_kernels: AtomicU64::new(0),
                reclaimed_kernels: AtomicU64::new(0),
                deferred_adoptions: AtomicU64::new(0),
                backpressured: AtomicBool::new(false),
                audio_idle: AtomicBool::new(true),
            }),
        }
    }

    /// Publish a uniquely-owned kernel and return its monotonic generation.
    ///
    /// This may allocate the wrapping `Arc` and destroy a superseded or retired
    /// kernel, so it is a control/offline-thread operation. A kernel that audio
    /// has not withdrawn is replaced latest-wins on this calling thread.
    pub fn publish(&self, kernel: FFTConvolver) -> u64 {
        #[cfg(test)]
        {
            self.publish_inner(kernel, None)
        }
        #[cfg(not(test))]
        {
            self.publish_inner(kernel)
        }
    }

    fn publish_inner(
        &self,
        kernel: FFTConvolver,
        #[cfg(test)] drop_probe: Option<ConvolverDropProbe>,
    ) -> u64 {
        let _control_guard = self
            .inner
            .control_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = self.reclaim_retired_unlocked();
        let generation = self
            .inner
            .latest_published_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.inner.audio_idle.store(false, Ordering::Release);

        let replaced = self.inner.published.swap(Some(Arc::new(PublishedConvolver {
            generation,
            kernel,
            #[cfg(test)]
            _drop_probe: drop_probe,
        })));
        if replaced.is_some() {
            self.inner
                .superseded_kernels
                .fetch_add(1, Ordering::Relaxed);
        }
        drop(replaced);

        // Close the common race where audio retired its previous kernel while
        // this control-side publication was being installed.
        let _ = self.reclaim_retired_unlocked();
        generation
    }

    #[cfg(test)]
    fn publish_with_drop_probe(&self, kernel: FFTConvolver, drop_probe: ConvolverDropProbe) -> u64 {
        self.publish_inner(kernel, Some(drop_probe))
    }

    /// Destroy one handed-off kernel on the calling control/offline thread.
    pub fn reclaim_retired(&self) -> bool {
        let _control_guard = self
            .inner
            .control_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.reclaim_retired_unlocked()
    }

    fn reclaim_retired_unlocked(&self) -> bool {
        let retired = self.inner.retired.swap(None);
        if retired.is_none() {
            return false;
        }
        self.inner.reclaimed_kernels.fetch_add(1, Ordering::Relaxed);
        drop(retired);
        true
    }

    /// Publish the block-boundary enable state from a control or setup thread.
    pub fn set_enabled(&self, enabled: bool) {
        self.inner.enabled.store(enabled, Ordering::Release);
    }

    /// Read the current block-boundary enable state.
    pub fn is_enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Acquire)
    }

    /// Read an allocation-free, eventually consistent lifecycle snapshot.
    pub fn status(&self) -> ConvolverStatus {
        let latest_published_generation = self
            .inner
            .latest_published_generation
            .load(Ordering::Acquire);
        let adopted_kernels = self.inner.adopted_kernels.load(Ordering::Acquire);
        let superseded_kernels = self.inner.superseded_kernels.load(Ordering::Acquire);
        let discarded_kernels = self.inner.discarded_kernels.load(Ordering::Acquire);
        let retired_kernels = self.inner.retired_kernels.load(Ordering::Acquire);
        let reclaimed_kernels = self.inner.reclaimed_kernels.load(Ordering::Acquire);
        let completed_publications = adopted_kernels
            .saturating_add(superseded_kernels)
            .saturating_add(discarded_kernels);

        ConvolverStatus {
            enabled: self.is_enabled(),
            latest_published_generation,
            latest_adopted_generation: self.inner.latest_adopted_generation.load(Ordering::Acquire),
            adopted_kernels,
            superseded_kernels,
            discarded_kernels,
            retired_kernels,
            reclaimed_kernels,
            deferred_adoptions: self.inner.deferred_adoptions.load(Ordering::Acquire),
            pending_kernels: latest_published_generation.saturating_sub(completed_publications),
            pending_reclamations: retired_kernels.saturating_sub(reclaimed_kernels),
            backpressured: self.inner.backpressured.load(Ordering::Acquire),
            audio_idle: self.inner.audio_idle.load(Ordering::Acquire),
        }
    }

    fn note_adopted(&self, generation: u64) {
        self.inner
            .latest_adopted_generation
            .store(generation, Ordering::Release);
        self.inner.adopted_kernels.fetch_add(1, Ordering::Relaxed);
    }

    fn note_discarded(&self) {
        self.inner.discarded_kernels.fetch_add(1, Ordering::Relaxed);
    }

    fn note_retired(&self) {
        self.inner.retired_kernels.fetch_add(1, Ordering::Relaxed);
    }

    fn mark_backpressured(&self) {
        if !self.inner.backpressured.swap(true, Ordering::AcqRel) {
            self.inner
                .deferred_adoptions
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn clear_backpressure(&self) {
        self.inner.backpressured.store(false, Ordering::Release);
    }

    fn set_audio_idle(&self, idle: bool) {
        self.inner.audio_idle.store(idle, Ordering::Release);
    }
}

impl Default for ConvolverControl {
    fn default() -> Self {
        Self::new(false)
    }
}

/// FFT convolver processor with wait-free kernel publication hand-off.
///
/// Publish and reclaim through [`ConvolverControl`]. Retired kernels are handed
/// back through its fixed single-slot channel; without control-side draining,
/// at most two retired kernels are parked and further adoptions are deferred
/// while the current valid kernel continues processing. Disable and drive
/// process/repeated finish until [`ConvolverStatus::is_quiescent`] before an
/// audio-thread-owned processor is destroyed; destroy the processor itself off
/// the realtime thread.
pub struct ConvolverProcessor {
    /// Active kernel. Held as a uniquely-owned `Arc` so retirement is a
    /// pointer hand-off instead of a reallocation.
    owned: Option<Arc<PublishedConvolver>>,
    /// Kernel withdrawn from the publication slot but not yet adoptable
    /// (ownership is still shared, or retirement stages are full).
    incoming: Option<Arc<PublishedConvolver>>,
    /// Retired kernel waiting for the control-side hand-off slot to free up.
    pending_retire: Option<Arc<PublishedConvolver>>,
    control: ConvolverControl,
    lifecycle: FixedLifecycle,
    sample_rate_hz: u32,
    finish_remaining_frames: Option<usize>,
}

impl ConvolverProcessor {
    /// Construct a processor that consumes one live audio side of `control`.
    pub fn new(control: ConvolverControl) -> Self {
        Self {
            owned: None,
            incoming: None,
            pending_retire: None,
            control,
            lifecycle: FixedLifecycle::default(),
            sample_rate_hz: 44_100,
            finish_remaining_frames: None,
        }
    }

    /// Clone the control-plane handle for off-audio publication/reclamation.
    pub fn control(&self) -> ConvolverControl {
        self.control.clone()
    }

    /// Move `pending_retire` into the control-side hand-off slot when it is free.
    /// Audio thread is the only writer of `retired`; the control side only
    /// takes, so an observed-empty slot cannot be concurrently filled.
    fn try_flush_retired(&mut self) {
        if let Some(arc) = self.pending_retire.take() {
            if self.control.inner.retired.load().is_none() {
                self.control.inner.retired.store(Some(arc));
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
            self.incoming = self.control.inner.published.swap(None);
        }

        if !self.control.is_enabled() {
            // Retire everything we hold, one stage per block if needed, without
            // deallocating on the audio thread.
            if self.pending_retire.is_none() {
                if let Some(arc) = self.owned.take() {
                    self.pending_retire = Some(arc);
                    self.control.note_retired();
                    self.try_flush_retired();
                } else if let Some(arc) = self.incoming.take() {
                    self.pending_retire = Some(arc);
                    self.control.note_discarded();
                    self.control.note_retired();
                    self.try_flush_retired();
                }
            }
            let still_holding =
                self.owned.is_some() || self.incoming.is_some() || self.pending_retire.is_some();
            let has_pending_publication = self.control.status().pending_kernels > 0;
            self.control
                .set_audio_idle(!still_holding && !has_pending_publication);
            if still_holding {
                self.control.mark_backpressured();
            } else {
                self.control.clear_backpressure();
            }
            return;
        }

        self.control.set_audio_idle(false);

        let Some(mut arc) = self.incoming.take() else {
            if self.pending_retire.is_some() {
                self.control.mark_backpressured();
            } else {
                self.control.clear_backpressure();
            }
            return;
        };
        if Arc::get_mut(&mut arc).is_none() {
            // The control API publishes by value, so this can only be a broken
            // ownership invariant. Retain and retry without cloning/dropping.
            self.incoming = Some(arc);
            self.control.mark_backpressured();
            return;
        }
        let generation = arc.generation;
        match self.owned.take() {
            None => {
                self.owned = Some(arc);
                self.control.note_adopted(generation);
                self.control.clear_backpressure();
            }
            Some(old) => {
                if self.pending_retire.is_none() {
                    self.owned = Some(arc);
                    self.control.note_adopted(generation);
                    self.pending_retire = Some(old);
                    self.control.note_retired();
                    self.try_flush_retired();
                    if self.pending_retire.is_some() {
                        self.control.mark_backpressured();
                    } else {
                        self.control.clear_backpressure();
                    }
                } else {
                    // Both retirement stages occupied: keep the old kernel and
                    // defer adoption until the control side drains.
                    self.owned = Some(old);
                    self.incoming = Some(arc);
                    self.control.mark_backpressured();
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

        if !self.control.is_enabled() {
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
        let channels = convolver.kernel.channels();
        process_fixed_1_to_1(
            "Convolver",
            true,
            Some(channels),
            buffers,
            |buffer, _channels| {
                convolver.kernel.process_inplace(buffer);
                Ok(())
            },
        )
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        if !self.control.is_enabled() {
            // Repeated terminal finish calls are the only lifecycle-safe audio
            // boundary available after ordinary processing has ended. Keep
            // progressing disabled retirement so control can reach quiescence.
            self.sync_convolver();
        }
        let enabled = self.control.is_enabled();
        let channels = if enabled {
            self.owned
                .as_ref()
                .map(|convolver| convolver.kernel.channels())
        } else {
            None
        };
        validate_channels("Convolver", channels, output.channels())?;
        if self.lifecycle.is_finished() {
            return Ok(ProcessProgress::finished(0));
        }

        self.lifecycle.begin_finish();
        let initial_remaining = if enabled {
            self.owned
                .as_ref()
                .map(|convolver| convolver.kernel.ir_length().saturating_sub(1))
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
            convolver
                .kernel
                .process_inplace(&mut output.samples_mut()[..samples]);
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
            convolver.kernel.reset();
        }
        self.lifecycle.reset();
        self.finish_remaining_frames = None;
        Ok(())
    }

    fn tail(&self) -> TailSpec {
        if !self.control.is_enabled() {
            return TailSpec::None;
        }
        let frames = self
            .owned
            .as_ref()
            .map(|convolver| convolver.kernel.ir_length().saturating_sub(1))
            .unwrap_or(0);
        TailSpec::finite(frames, self.sample_rate_hz).unwrap_or(TailSpec::Unknown)
    }

    fn is_enabled(&self) -> bool {
        self.control.is_enabled()
    }

    fn set_enabled(&mut self, enabled: bool) {
        // Kernel teardown is handled by `sync_convolver`'s disabled path so the
        // audio thread never deallocates it here.
        self.control.set_enabled(enabled);
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
        let control = ConvolverControl::default();
        let mut proc = ConvolverProcessor::new(control.clone());
        let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

        assert!(proc.process(&mut buffer, 1).is_bypassed());

        let generation = control.publish(FFTConvolver::new(&[0.5], 1));
        control.set_enabled(true);
        assert!(!proc.process(&mut buffer, 1).is_bypassed());
        assert_eq!(buffer, vec![0.5, 1.0, 1.5, 2.0]);

        let status = control.status();
        assert_eq!(status.latest_published_generation, generation);
        assert_eq!(status.latest_adopted_generation, generation);
        assert_eq!(status.adopted_kernels, 1);
        assert_eq!(status.pending_kernels, 0);
        assert!(!status.backpressured);
    }

    #[test]
    fn test_convolver_processor_clear_disables_owned_convolver() {
        let control = ConvolverControl::new(true);
        let mut proc = ConvolverProcessor::new(control.clone());
        let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

        control.publish(FFTConvolver::new(&[0.5], 1));
        assert!(!proc.process(&mut buffer, 1).is_bypassed());

        control.set_enabled(false);
        let mut bypassed = vec![1.0, 2.0, 3.0, 4.0];
        assert!(proc.process(&mut bypassed, 1).is_bypassed());
        assert_eq!(bypassed, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(control.status().pending_reclamations, 1);
        assert!(control.reclaim_retired());
        assert!(control.status().is_quiescent());
    }

    #[test]
    fn convolver_publication_is_latest_wins_before_audio_withdrawal() {
        let control = ConvolverControl::new(true);
        let mut proc = ConvolverProcessor::new(control.clone());
        let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

        let first = control.publish(FFTConvolver::new(&[0.5], 1));
        let latest = control.publish(FFTConvolver::new(&[0.25], 1));
        assert!(!proc.process(&mut buffer, 1).is_bypassed());
        assert_eq!(buffer, vec![0.25, 0.5, 0.75, 1.0]);

        let status = control.status();
        assert_eq!(first, 1);
        assert_eq!(status.latest_adopted_generation, latest);
        assert_eq!(status.adopted_kernels, 1);
        assert_eq!(status.superseded_kernels, 1);
        assert_eq!(status.pending_kernels, 0);
    }

    #[test]
    fn convolver_disable_reports_retirement_backpressure_and_recovers() {
        let control = ConvolverControl::new(true);
        let mut proc = ConvolverProcessor::new(control.clone());
        let mut buffer = vec![1.0; 4];

        control.publish(FFTConvolver::new(&[1.0], 1));
        assert!(!proc.process(&mut buffer, 1).is_bypassed());
        control.publish(FFTConvolver::new(&[0.5], 1));
        control.set_enabled(false);

        assert!(proc.process(&mut buffer, 1).is_bypassed());
        assert!(proc.process(&mut buffer, 1).is_bypassed());
        let saturated = control.status();
        assert!(saturated.backpressured);
        assert_eq!(saturated.discarded_kernels, 1);
        assert_eq!(saturated.pending_reclamations, 2);

        assert!(control.reclaim_retired());
        assert!(proc.process(&mut buffer, 1).is_bypassed());
        let recovered = control.status();
        assert!(!recovered.backpressured);
        assert!(recovered.audio_idle);
        assert_eq!(recovered.pending_reclamations, 1);

        assert!(control.reclaim_retired());
        assert!(control.status().is_quiescent());
    }

    #[test]
    fn convolver_processor_kernel_swap_is_allocation_free_on_audio_side() {
        let control = ConvolverControl::new(true);
        let mut proc = ConvolverProcessor::new(control.clone());
        let mut buffer = vec![0.3; 512];

        for _ in 0..8 {
            // Control side: publishing allocates (allowed).
            control.publish(FFTConvolver::new(&[0.5, 0.25], 1));
            // Audio side: swap-in, retirement hand-off, and processing must not
            // allocate or deallocate.
            assert_no_alloc::assert_no_alloc(|| {
                proc.process(&mut buffer, 1);
            });
            // Control side: draining performs the large deallocation.
            let _ = control.reclaim_retired();
        }

        control.publish(FFTConvolver::new(&[0.75], 1));
        control.set_enabled(false);
        assert_no_alloc::assert_no_alloc(|| {
            proc.process(&mut buffer, 1);
            proc.process(&mut buffer, 1);
        });
        assert!(control.status().backpressured);

        assert!(control.reclaim_retired());
        assert_no_alloc::assert_no_alloc(|| {
            proc.process(&mut buffer, 1);
        });
        assert!(control.reclaim_retired());

        control.set_enabled(true);
        control.publish(FFTConvolver::new(&[0.25], 1));
        assert_no_alloc::assert_no_alloc(|| {
            proc.process(&mut buffer, 1);
        });
    }

    #[test]
    fn convolver_control_stress_remains_bounded_and_adopts_latest_generation() {
        const UPDATES: u64 = 10_000;

        let control = ConvolverControl::new(true);
        let mut proc = ConvolverProcessor::new(control.clone());
        let mut buffer = [1.0; 4];
        let mut latest_gain = 0.0;

        for update in 0..UPDATES {
            latest_gain = 0.25 + (update % 23) as f64 * 0.01;
            let generation = control.publish(FFTConvolver::new(&[latest_gain], 1));
            assert_eq!(generation, update + 1);

            if update % 17 == 0 {
                buffer.fill(1.0);
                assert!(!proc.process(&mut buffer, 1).is_bypassed());
                assert!((buffer[0] - latest_gain).abs() <= f64::EPSILON);
            }
            if update % 113 == 0 {
                let _ = control.reclaim_retired();
            }
        }

        buffer.fill(1.0);
        assert!(!proc.process(&mut buffer, 1).is_bypassed());
        assert!((buffer[0] - latest_gain).abs() <= f64::EPSILON);
        let _ = control.reclaim_retired();

        let burst_status = control.status();
        assert_eq!(burst_status.latest_published_generation, UPDATES);
        assert_eq!(burst_status.latest_adopted_generation, UPDATES);
        assert_eq!(
            burst_status.adopted_kernels
                + burst_status.superseded_kernels
                + burst_status.discarded_kernels,
            UPDATES
        );
        assert_eq!(burst_status.pending_kernels, 0);
        assert_eq!(burst_status.pending_reclamations, 0);

        control.publish(FFTConvolver::new(&[0.5], 1));
        control.set_enabled(false);
        assert!(proc.process(&mut buffer, 1).is_bypassed());
        assert!(proc.process(&mut buffer, 1).is_bypassed());
        let saturated = control.status();
        assert!(saturated.backpressured);
        assert_eq!(saturated.pending_reclamations, 2);

        assert!(control.reclaim_retired());
        assert!(proc.process(&mut buffer, 1).is_bypassed());
        assert!(control.reclaim_retired());
        assert!(control.status().is_quiescent());

        control.set_enabled(true);
        let final_generation = control.publish(FFTConvolver::new(&[0.875], 1));
        buffer.fill(1.0);
        assert!(!proc.process(&mut buffer, 1).is_bypassed());
        assert_eq!(buffer, [0.875; 4]);

        let final_status = control.status();
        assert_eq!(final_status.latest_adopted_generation, final_generation);
        assert_eq!(final_status.pending_kernels, 0);
        assert_eq!(final_status.pending_reclamations, 0);
        assert!(!final_status.backpressured);
        assert!(final_status.deferred_adoptions >= 1);
        assert_eq!(
            final_status.adopted_kernels
                + final_status.superseded_kernels
                + final_status.discarded_kernels,
            final_status.latest_published_generation
        );
    }

    #[test]
    fn convolver_control_serializes_concurrent_publishers() {
        const PUBLISHERS: usize = 4;
        const UPDATES_PER_PUBLISHER: usize = 64;
        const TOTAL_UPDATES: usize = PUBLISHERS * UPDATES_PER_PUBLISHER;

        let control = ConvolverControl::new(true);
        let start = Arc::new(std::sync::Barrier::new(PUBLISHERS));
        let mut publishers = Vec::with_capacity(PUBLISHERS);
        for publisher in 0..PUBLISHERS {
            let control = control.clone();
            let start = Arc::clone(&start);
            publishers.push(std::thread::spawn(move || {
                start.wait();
                let mut published = Vec::with_capacity(UPDATES_PER_PUBLISHER);
                for update in 0..UPDATES_PER_PUBLISHER {
                    let ordinal = publisher * UPDATES_PER_PUBLISHER + update + 1;
                    let gain = ordinal as f64 / TOTAL_UPDATES as f64;
                    let generation = control.publish(FFTConvolver::new(&[gain], 1));
                    published.push((generation, gain));
                }
                published
            }));
        }

        let mut publications = Vec::with_capacity(TOTAL_UPDATES);
        for publisher in publishers {
            publications.extend(publisher.join().unwrap());
        }
        publications.sort_by_key(|(generation, _)| *generation);
        assert_eq!(publications.len(), TOTAL_UPDATES);
        for (index, (generation, _)) in publications.iter().enumerate() {
            assert_eq!(*generation, index as u64 + 1);
        }

        let (latest_generation, latest_gain) = publications[TOTAL_UPDATES - 1];
        let mut proc = ConvolverProcessor::new(control.clone());
        let mut buffer = [1.0; 4];
        assert!(!proc.process(&mut buffer, 1).is_bypassed());
        assert_eq!(buffer, [latest_gain; 4]);

        let status = control.status();
        assert_eq!(status.latest_published_generation, latest_generation);
        assert_eq!(status.latest_adopted_generation, latest_generation);
        assert_eq!(status.adopted_kernels, 1);
        assert_eq!(status.superseded_kernels, TOTAL_UPDATES as u64 - 1);
        assert_eq!(status.pending_kernels, 0);
    }

    #[test]
    fn convolver_kernels_are_destroyed_by_control_not_audio_thread() {
        use std::sync::mpsc::sync_channel;

        let control = ConvolverControl::new(true);
        let audio_control = control.clone();
        let (command_tx, command_rx) = sync_channel::<bool>(0);
        let (ready_tx, ready_rx) = sync_channel(0);
        let (processed_tx, processed_rx) = sync_channel(0);
        let audio_thread = std::thread::spawn(move || {
            ready_tx.send(std::thread::current().id()).unwrap();
            let mut proc = ConvolverProcessor::new(audio_control);
            let mut buffer = [1.0; 4];
            while command_rx.recv().unwrap() {
                buffer.fill(1.0);
                let _ = proc.process(&mut buffer, 1);
                processed_tx.send(()).unwrap();
            }
        });
        let audio_thread_id = ready_rx.recv().unwrap();
        let dropped_on_audio = Arc::new(AtomicBool::new(false));
        let drop_count = Arc::new(AtomicU64::new(0));
        let make_probe = || ConvolverDropProbe {
            audio_thread_id,
            dropped_on_audio: Arc::clone(&dropped_on_audio),
            drop_count: Arc::clone(&drop_count),
        };
        let process_once = || {
            command_tx.send(true).unwrap();
            processed_rx.recv().unwrap();
        };

        control.publish_with_drop_probe(FFTConvolver::new(&[1.0], 1), make_probe());
        process_once();
        control.publish_with_drop_probe(FFTConvolver::new(&[0.75], 1), make_probe());
        process_once();
        assert_eq!(drop_count.load(Ordering::Acquire), 0);
        assert!(control.reclaim_retired());

        control.publish_with_drop_probe(FFTConvolver::new(&[0.5], 1), make_probe());
        control.publish_with_drop_probe(FFTConvolver::new(&[0.25], 1), make_probe());
        process_once();
        assert_eq!(drop_count.load(Ordering::Acquire), 2);
        assert!(control.reclaim_retired());

        control.set_enabled(false);
        process_once();
        assert!(control.reclaim_retired());
        assert!(control.status().is_quiescent());

        command_tx.send(false).unwrap();
        audio_thread.join().unwrap();
        assert_eq!(drop_count.load(Ordering::Acquire), 4);
        assert!(!dropped_on_audio.load(Ordering::Acquire));
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

    fn direct_interleaved_convolution(input: &[f64], ir: &[f64], channels: usize) -> Vec<f64> {
        let input_frames = input.len() / channels;
        let ir_frames = ir.len() / channels;
        let mut output = vec![0.0; (input_frames + ir_frames - 1) * channels];

        for input_frame in 0..input_frames {
            for tap in 0..ir_frames {
                let output_frame = input_frame + tap;
                for channel in 0..channels {
                    output[output_frame * channels + channel] +=
                        input[input_frame * channels + channel] * ir[tap * channels + channel];
                }
            }
        }
        output
    }

    fn deterministic_convolver_input(frames: usize, channels: usize) -> Vec<f64> {
        (0..frames * channels)
            .map(|sample| ((sample * 7 + 3) % 19) as f64 * 0.03125 - 0.28125)
            .collect()
    }

    fn deterministic_convolver_ir(frames: usize, channels: usize) -> Vec<f64> {
        let mut ir = vec![0.0; frames * channels];
        for frame in 0..frames {
            for channel in 0..channels {
                let value = if frame == 0 {
                    0.75 - channel as f64 * 0.125
                } else {
                    let sign = if (frame + channel) % 2 == 0 {
                        1.0
                    } else {
                        -1.0
                    };
                    sign * (0.2 + channel as f64 * 0.025) / (frame + 1) as f64
                };
                ir[frame * channels + channel] = value;
            }
        }
        ir
    }

    fn render_convolver_with_patterns(
        proc: &mut ConvolverProcessor,
        input: &[f64],
        channels: usize,
        process_chunks: &[usize],
        finish_chunks: &[usize],
        expected_ir_frames: usize,
    ) -> Vec<f64> {
        assert!(!process_chunks.is_empty());
        assert!(!finish_chunks.is_empty());
        assert!(process_chunks.iter().all(|frames| *frames > 0));
        assert!(finish_chunks.iter().all(|frames| *frames > 0));
        assert_eq!(proc.latency(), FrameDuration::ZERO);

        let input_frames = input.len() / channels;
        let mut output = Vec::with_capacity((input_frames + expected_ir_frames - 1) * channels);
        let mut cursor = 0;
        let mut chunk_index = 0;
        while cursor < input_frames {
            let frames =
                process_chunks[chunk_index % process_chunks.len()].min(input_frames - cursor);
            let sample_start = cursor * channels;
            let sample_end = (cursor + frames) * channels;
            let mut block = input[sample_start..sample_end].to_vec();
            let progress = super::super::traits::process_checked(
                proc,
                ProcessBuffers::in_place(AudioBlockMut::new(&mut block, channels).unwrap()),
            )
            .unwrap();
            assert_eq!(progress.consumed_frames(), frames);
            assert_eq!(progress.produced_frames(), frames);
            assert_eq!(progress.state(), ProcessState::NeedInput);
            output.extend_from_slice(&block);
            cursor += frames;
            chunk_index += 1;
        }

        assert_eq!(
            proc.tail(),
            TailSpec::finite(expected_ir_frames - 1, 48_000).unwrap()
        );

        let mut finish_index = 0;
        let final_produced = loop {
            let capacity_frames = finish_chunks[finish_index % finish_chunks.len()];
            let mut scratch = vec![0.0; capacity_frames * channels];
            let progress = super::super::traits::finish_checked(
                proc,
                AudioBlockMut::new(&mut scratch, channels).unwrap(),
            )
            .unwrap();
            output.extend_from_slice(&scratch[..progress.produced_frames() * channels]);
            if progress.state() == ProcessState::Finished {
                break progress.produced_frames();
            }
            assert_eq!(progress.state(), ProcessState::NeedOutput);
            assert_eq!(progress.produced_frames(), capacity_frames);
            finish_index += 1;
        };

        if expected_ir_frames > 1 {
            assert!(final_produced > 0);
        }
        let mut terminal_scratch = vec![0.0; finish_chunks[0] * channels];
        assert_eq!(
            super::super::traits::finish_checked(
                proc,
                AudioBlockMut::new(&mut terminal_scratch, channels).unwrap(),
            )
            .unwrap(),
            ProcessProgress::finished(0)
        );
        output
    }

    fn assert_convolver_matches_direct_oracle(
        input_frames: usize,
        ir_frames: usize,
        channels: usize,
    ) {
        let input = deterministic_convolver_input(input_frames, channels);
        let ir = deterministic_convolver_ir(ir_frames, channels);
        let expected = direct_interleaved_convolution(&input, &ir, channels);

        for (process_chunks, finish_chunks) in [
            (vec![input_frames], vec![ir_frames.max(1)]),
            (vec![1, 4, 2, 7, 3], vec![1, 5, 17, 257]),
        ] {
            let control = ConvolverControl::new(true);
            control.publish(FFTConvolver::new(&ir, channels));
            let mut proc = ConvolverProcessor::new(control);
            proc.set_sample_rate(48_000).unwrap();
            let actual = render_convolver_with_patterns(
                &mut proc,
                &input,
                channels,
                &process_chunks,
                &finish_chunks,
                ir_frames,
            );

            assert_eq!(actual.len(), expected.len());
            for (sample, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                assert!(
                    (actual - expected).abs() <= 1.0e-8,
                    "sample {sample} differs: actual={actual:?} expected={expected:?}"
                );
            }
        }
    }

    #[test]
    fn convolver_process_and_finish_match_independent_direct_oracle() {
        let long_ir_frames = super::super::convolver::PARTITIONED_CONVOLUTION_IR_THRESHOLD + 1;
        assert_convolver_matches_direct_oracle(23, 1, 1);
        assert_convolver_matches_direct_oracle(29, 9, 2);
        assert_convolver_matches_direct_oracle(31, long_ir_frames, 1);
        assert_convolver_matches_direct_oracle(27, long_ir_frames, 2);
    }

    #[test]
    fn convolver_reset_isolates_prior_process_and_partial_finish_history() {
        const CHANNELS: usize = 2;
        let ir = deterministic_convolver_ir(11, CHANNELS);
        let control = ConvolverControl::new(true);
        let generation = control.publish(FFTConvolver::new(&ir, CHANNELS));
        let mut proc = ConvolverProcessor::new(control.clone());
        proc.set_sample_rate(48_000).unwrap();

        let mut prior = deterministic_convolver_input(17, CHANNELS);
        let _ = super::super::traits::process_checked(
            &mut proc,
            ProcessBuffers::in_place(AudioBlockMut::new(&mut prior, CHANNELS).unwrap()),
        )
        .unwrap();
        let mut partial_tail = [0.0; 3 * CHANNELS];
        let partial = super::super::traits::finish_checked(
            &mut proc,
            AudioBlockMut::new(&mut partial_tail, CHANNELS).unwrap(),
        )
        .unwrap();
        assert_eq!(partial.state(), ProcessState::NeedOutput);

        proc.reset().unwrap();
        assert_eq!(control.status().latest_adopted_generation, generation);
        let input = deterministic_convolver_input(19, CHANNELS);
        let actual =
            render_convolver_with_patterns(&mut proc, &input, CHANNELS, &[2, 5, 1, 7], &[3, 4], 11);
        let expected = direct_interleaved_convolution(&input, &ir, CHANNELS);

        for (sample, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
            assert!(
                (actual - expected).abs() <= 1.0e-10,
                "sample {sample} leaked prior stream state: actual={actual:?} expected={expected:?}"
            );
        }
    }

    #[test]
    fn convolver_sample_rate_only_retags_finite_tail_duration() {
        let control = ConvolverControl::new(true);
        let generation = control.publish(FFTConvolver::new(&[1.0, 0.5, 0.25], 1));
        let mut proc = ConvolverProcessor::new(control.clone());
        proc.set_sample_rate(48_000).unwrap();
        let mut input = [1.0];
        let _ = proc.process(&mut input, 1);

        assert_eq!(proc.latency(), FrameDuration::ZERO);
        assert_eq!(proc.tail(), TailSpec::finite(2, 48_000).unwrap());
        proc.set_sample_rate(96_000).unwrap();
        assert_eq!(proc.tail(), TailSpec::finite(2, 96_000).unwrap());
        assert_eq!(control.status().latest_adopted_generation, generation);

        control.set_enabled(false);
        assert_eq!(proc.tail(), TailSpec::None);
        assert_eq!(
            ConvolverProcessor::new(ConvolverControl::new(true)).tail(),
            TailSpec::None
        );
    }

    #[test]
    fn convolver_finish_preserves_last_frame_impulse_tail() {
        let control = ConvolverControl::new(true);
        control.publish(FFTConvolver::new(&[1.0, 0.5, 0.25], 1));
        let mut proc = ConvolverProcessor::new(control);
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
    fn convolver_terminal_finish_can_retire_to_control_quiescence() {
        let control = ConvolverControl::new(true);
        control.publish(FFTConvolver::new(&[1.0, 0.5], 1));
        let mut proc = ConvolverProcessor::new(control.clone());
        let mut input = [1.0];
        let _ = proc.process(&mut input, 1);
        let mut scratch = [0.0];
        assert_eq!(
            super::super::traits::finish_checked(
                &mut proc,
                AudioBlockMut::new(&mut scratch, 1).unwrap(),
            )
            .unwrap()
            .state(),
            ProcessState::Finished
        );

        control.publish(FFTConvolver::new(&[0.25], 1));
        control.set_enabled(false);
        for _ in 0..2 {
            assert_eq!(
                super::super::traits::finish_checked(
                    &mut proc,
                    AudioBlockMut::new(&mut scratch, 1).unwrap(),
                )
                .unwrap(),
                ProcessProgress::finished(0)
            );
        }
        assert!(control.status().backpressured);
        assert_eq!(control.status().pending_reclamations, 2);

        assert!(control.reclaim_retired());
        assert_eq!(
            super::super::traits::finish_checked(
                &mut proc,
                AudioBlockMut::new(&mut scratch, 1).unwrap(),
            )
            .unwrap(),
            ProcessProgress::finished(0)
        );
        assert!(control.reclaim_retired());
        assert!(control.status().is_quiescent());
    }

    #[test]
    fn finite_finish_paths_are_allocation_free_after_processing() {
        let limiter_params = Arc::new(AtomicPeakLimiterParams::new());
        let mut limiter = PeakLimiterProcessor::new(1, 48_000, limiter_params);
        let mut limiter_input = vec![0.25; 64];
        let _ = limiter.process(&mut limiter_input, 1);
        let mut limiter_output = vec![0.0; limiter.limiter.delay_frames()];

        let control = ConvolverControl::new(true);
        control.publish(FFTConvolver::new(&[1.0, 0.5, 0.25], 1));
        let mut convolver = ConvolverProcessor::new(control);
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
