//! Caller-driven callback DSP facade and ring-buffer primitive.
//!
//! The public facade separates control-thread construction/configuration from
//! the callback-owned [`PlaybackPipeline`]. Its [`CallbackSpec`] describes
//! already-converted device-domain `f64` audio; it does not negotiate devices,
//! decode files, or resample source audio.
use crate::processor::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry,
    AtomicEqParams, AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams,
    AtomicVolumeParams, AudioBlockMut, ChainFinishPolicy, ConvolverControl, ConvolverStatus,
    DspChain, FFTConvolver, FrameDuration, NoiseShaperCurve, OutputChainBuilder, OutputChainParams,
    ProcessError, ProcessProgress, SaturationParamsSnapshot, SaturationQuality, SaturationType,
    TailSpec,
};
use std::sync::Arc;

/// Validated callback-domain geometry and prepared capacity.
///
/// Construct this on the control thread before building a [`PlaybackPipeline`].
/// `sample_rate_hz` and `channels` describe already-converted device audio, and
/// every [`PlaybackPipeline::process`] or finish buffer must contain at most
/// `max_frames` complete interleaved frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallbackSpec {
    channels: usize,
    sample_rate_hz: u32,
    max_frames: usize,
}
impl CallbackSpec {
    /// Validate a callback-domain channel count, sample rate, and maximum block size.
    pub fn new(
        channels: usize,
        sample_rate_hz: u32,
        max_frames: usize,
    ) -> Result<Self, ProcessError> {
        if channels == 0 {
            return Err(ProcessError::InvalidGeometry {
                processor: "CallbackSpec",
                operation: "create callback spec",
                message: "channel count must be greater than zero",
            });
        }
        if sample_rate_hz == 0 {
            return Err(ProcessError::InvalidSampleRate {
                processor: "CallbackSpec",
                sample_rate_hz,
            });
        }
        if max_frames == 0 {
            return Err(ProcessError::InvalidGeometry {
                processor: "CallbackSpec",
                operation: "create callback spec",
                message: "maximum callback frames must be greater than zero",
            });
        }
        Ok(Self {
            channels,
            sample_rate_hz,
            max_frames,
        })
    }
    /// Create a validated stereo callback specification.
    pub fn stereo(sample_rate_hz: u32, max_frames: usize) -> Result<Self, ProcessError> {
        Self::new(2, sample_rate_hz, max_frames)
    }
    /// Number of interleaved callback channels.
    pub const fn channels(self) -> usize {
        self.channels
    }
    /// Callback/device sample rate in Hz.
    pub const fn sample_rate_hz(self) -> u32 {
        self.sample_rate_hz
    }
    /// Largest accepted callback or drain buffer, in frames.
    pub const fn max_frames(self) -> usize {
        self.max_frames
    }
}

/// Initial saturation settings. `drive` is 0.0–2.0, threshold and mix are
/// linear 0.0–1.0 values, and gains/cutoff use dB/Hz respectively.
///
/// `#[non_exhaustive]`: construct via [`Self::disabled`]/[`Self::enabled`] and
/// the `with_*` builders so future fields are not breaking changes.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PlaybackSaturationConfig {
    pub enabled: bool,
    pub saturation_type: SaturationType,
    pub quality: SaturationQuality,
    pub drive: f64,
    pub threshold: f64,
    pub mix: f64,
    pub input_gain_db: f64,
    pub output_gain_db: f64,
    pub highpass_cutoff_hz: Option<f64>,
}
impl PlaybackSaturationConfig {
    /// A hard-bypassed saturation stage with no added saturation latency.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            saturation_type: SaturationType::Tube,
            quality: SaturationQuality::Direct,
            drive: 0.25,
            threshold: 0.88,
            mix: 0.2,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            highpass_cutoff_hz: None,
        }
    }
    /// An armed saturation stage using the default character settings.
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::disabled()
        }
    }
    /// Set the saturation character (tube/tape/soft-clip variant).
    pub fn with_type(mut self, saturation_type: SaturationType) -> Self {
        self.saturation_type = saturation_type;
        self
    }
    /// Set the processing quality/latency trade-off.
    pub fn with_quality(mut self, quality: SaturationQuality) -> Self {
        self.quality = quality;
        self
    }
    /// Set drive (0.0–2.0).
    pub fn with_drive(mut self, drive: f64) -> Self {
        self.drive = drive;
        self
    }
    /// Set onset threshold (linear 0.0–1.0).
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }
    /// Set dry/wet mix (linear 0.0–1.0).
    pub fn with_mix(mut self, mix: f64) -> Self {
        self.mix = mix;
        self
    }
    /// Set input/output makeup gains in dB.
    pub fn with_gains_db(mut self, input_gain_db: f64, output_gain_db: f64) -> Self {
        self.input_gain_db = input_gain_db;
        self.output_gain_db = output_gain_db;
        self
    }
    /// Restrict saturation to content above a high-pass cutoff in Hz.
    pub fn with_highpass_cutoff_hz(mut self, cutoff_hz: Option<f64>) -> Self {
        self.highpass_cutoff_hz = cutoff_hz;
        self
    }
}
impl Default for PlaybackSaturationConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Initial crossfeed settings. Mix is a dry/wet linear ratio and cutoff is Hz.
///
/// `#[non_exhaustive]`: construct via [`Self::disabled`]/[`Self::enabled`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PlaybackCrossfeedConfig {
    pub enabled: bool,
    pub mix: f64,
    pub cutoff_hz: f64,
}
impl PlaybackCrossfeedConfig {
    /// A bypassed crossfeed stage.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            mix: 0.5,
            cutoff_hz: 700.0,
        }
    }
    /// Enable crossfeed with a linear mix and low-pass cutoff in Hz.
    pub fn enabled(mix: f64, cutoff_hz: f64) -> Self {
        Self {
            enabled: true,
            mix,
            cutoff_hz,
        }
    }
}
impl Default for PlaybackCrossfeedConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Initial dynamic-loudness settings. Listening volume is the current playback
/// volume in dBFS; strength is a 0.0–1.0 compensation amount.
///
/// `#[non_exhaustive]`: construct via [`Self::disabled`]/[`Self::enabled`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PlaybackDynamicLoudnessConfig {
    pub enabled: bool,
    pub listening_volume_db: f64,
    pub strength: f64,
}
impl PlaybackDynamicLoudnessConfig {
    /// A bypassed dynamic-loudness stage.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            listening_volume_db: 0.0,
            strength: 1.0,
        }
    }
    /// Enable compensation at the current listening volume in dBFS.
    pub fn enabled(listening_volume_db: f64, strength: f64) -> Self {
        Self {
            enabled: true,
            listening_volume_db,
            strength,
        }
    }
}
impl Default for PlaybackDynamicLoudnessConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Initial output noise-shaping settings. Bit depth is the target quantizer
/// precision; the callback facade still processes interleaved `f64` samples.
///
/// `#[non_exhaustive]`: construct via [`Self::disabled`]/[`Self::enabled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlaybackNoiseShapingConfig {
    pub enabled: bool,
    pub bits: u32,
    pub curve: NoiseShaperCurve,
}
impl PlaybackNoiseShapingConfig {
    /// A bypassed noise-shaping stage.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            bits: 24,
            curve: NoiseShaperCurve::Lipshitz5,
        }
    }
    /// Enable shaping for the specified target bit depth and curve.
    pub fn enabled(bits: u32, curve: NoiseShaperCurve) -> Self {
        Self {
            enabled: true,
            bits,
            curve,
        }
    }
}
impl Default for PlaybackNoiseShapingConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Intent-level initial settings for a [`PlaybackPipeline`].
///
/// [`Self::transparent`] and [`Default`] bypass every non-identity stage,
/// including the limiter, so the default chain is sample-identical and adds no
/// effect latency. Enable and configure stages explicitly when desired.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackConfig {
    volume: f64,
    eq_enabled: bool,
    eq_gains: [f64; crate::processor::EQ_BANDS],
    limiter_enabled: bool,
    limiter_threshold: f64,
    saturation: PlaybackSaturationConfig,
    crossfeed: PlaybackCrossfeedConfig,
    dynamic_loudness: PlaybackDynamicLoudnessConfig,
    noise_shaping: PlaybackNoiseShapingConfig,
}
impl PlaybackConfig {
    /// A sample-transparent, zero-added-processing default profile.
    pub fn transparent() -> Self {
        Self {
            volume: 1.0,
            eq_enabled: false,
            eq_gains: [0.0; crate::processor::EQ_BANDS],
            limiter_enabled: false,
            limiter_threshold: 0.0,
            saturation: PlaybackSaturationConfig::disabled(),
            crossfeed: PlaybackCrossfeedConfig::disabled(),
            dynamic_loudness: PlaybackDynamicLoudnessConfig::disabled(),
            noise_shaping: PlaybackNoiseShapingConfig::disabled(),
        }
    }
    /// Set the initial linear volume multiplier.
    pub fn with_volume(mut self, volume: f64) -> Self {
        self.volume = volume;
        self
    }
    /// Enable EQ with per-band gains in dB.
    pub fn with_eq(mut self, band_gains_db: [f64; crate::processor::EQ_BANDS]) -> Self {
        self.eq_enabled = true;
        self.eq_gains = band_gains_db;
        self
    }
    /// Limiter is always true-peak in this facade. Its release is fixed to the
    /// processor default (150 ms); use the advanced API to change either.
    pub fn with_limiter(mut self, enabled: bool, threshold_db: f64) -> Self {
        self.limiter_enabled = enabled;
        self.limiter_threshold = threshold_db;
        self
    }
    /// Configure saturation.
    pub fn with_saturation(mut self, value: PlaybackSaturationConfig) -> Self {
        self.saturation = value;
        self
    }
    /// Configure crossfeed.
    pub fn with_crossfeed(mut self, value: PlaybackCrossfeedConfig) -> Self {
        self.crossfeed = value;
        self
    }
    /// Configure dynamic loudness.
    pub fn with_dynamic_loudness(mut self, value: PlaybackDynamicLoudnessConfig) -> Self {
        self.dynamic_loudness = value;
        self
    }
    /// Configure output noise shaping.
    pub fn with_noise_shaping(mut self, value: PlaybackNoiseShapingConfig) -> Self {
        self.noise_shaping = value;
        self
    }
}
impl Default for PlaybackConfig {
    fn default() -> Self {
        Self::transparent()
    }
}

/// Best-effort latest dynamic-loudness readings for a control/UI thread.
///
/// Values are independently published by the audio thread and are not a
/// coherent multi-field snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct DynamicLoudnessTelemetry {
    pub factor: f64,
    pub band_gains_db: [f64; 7],
}

/// Cloneable, control-thread-only parameter publisher for ordinary callback
/// controls. Each update becomes visible as one complete snapshot at a later
/// callback block boundary. It owns no convolver authority and cannot build a
/// second audio consumer.
#[derive(Clone)]
pub struct PlaybackParameters {
    volume: Arc<AtomicVolumeParams>,
    eq: Arc<AtomicEqParams>,
    limiter: Arc<AtomicPeakLimiterParams>,
    crossfeed: Arc<AtomicCrossfeedParams>,
    dynamic_loudness: Arc<AtomicDynamicLoudnessParams>,
    noise_shaper: Arc<AtomicNoiseShaperParams>,
    telemetry: Arc<AtomicDynamicLoudnessTelemetry>,
}
impl PlaybackParameters {
    /// Publish a linear volume multiplier.
    pub fn set_volume(&self, value: f64) {
        self.volume.set_volume(value);
    }
    /// Publish mute state.
    pub fn set_muted(&self, muted: bool) {
        self.volume.set_muted(muted);
    }
    /// Enable or bypass the equalizer.
    pub fn set_eq_enabled(&self, enabled: bool) {
        self.eq.set_enabled(enabled);
    }
    /// Publish one equalizer band gain in dB.
    pub fn set_eq_band_gain_db(&self, band: usize, gain_db: f64) {
        self.eq.set_band_gain(band, gain_db);
    }
    /// Publish enablement and all band gains (dB) as one coherent callback
    /// snapshot. Prefer this over per-band writes when applying a preset.
    pub fn set_eq(&self, enabled: bool, band_gains_db: [f64; crate::processor::EQ_BANDS]) {
        self.eq.write(&band_gains_db, enabled);
    }
    /// Enable or bypass the limiter.
    pub fn set_limiter_enabled(&self, enabled: bool) {
        self.limiter.set_enabled(enabled);
    }
    /// Publish limiter threshold in dB.
    pub fn set_limiter_threshold_db(&self, threshold_db: f64) {
        self.limiter.set_threshold(threshold_db);
    }
    /// Saturation arming controls fixed stage latency and is construction/reset
    /// only. Runtime enablement is rejected; rebuild with an enabled
    /// [`PlaybackSaturationConfig`] instead.
    pub fn set_saturation_enabled(&self, _enabled: bool) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedOperation {
            processor: "PlaybackParameters",
            operation: "change saturation arming",
            message: "rebuild the pipeline to change saturation enablement",
        })
    }
    /// Saturation configuration, including drive, is construction/reset only
    /// because the transparent profile hard-bypasses the fixed-latency stage.
    /// Rebuild with [`PlaybackSaturationConfig`] to change it.
    pub fn set_saturation_drive(&self, _drive: f64) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedOperation {
            processor: "PlaybackParameters",
            operation: "change saturation configuration",
            message: "rebuild the pipeline to change saturation configuration",
        })
    }
    /// Enable or bypass crossfeed.
    pub fn set_crossfeed_enabled(&self, enabled: bool) {
        self.crossfeed.set_enabled(enabled);
    }
    /// Publish crossfeed enablement, dry/wet mix, and low-pass cutoff as one
    /// coherent callback snapshot.
    pub fn set_crossfeed(&self, enabled: bool, mix: f64, cutoff_hz: f64) {
        self.crossfeed.write(enabled, mix, cutoff_hz);
    }
    /// Enable or bypass dynamic loudness.
    pub fn set_dynamic_loudness_enabled(&self, enabled: bool) {
        self.dynamic_loudness.set_enabled(enabled);
    }
    /// Publish dynamic-loudness enablement, current listening volume in dBFS,
    /// and strength as one coherent callback snapshot.
    pub fn set_dynamic_loudness(&self, enabled: bool, listening_volume_db: f64, strength: f64) {
        self.dynamic_loudness
            .write(enabled, db_to_linear(listening_volume_db), strength);
    }
    /// Enable or bypass output noise shaping.
    pub fn set_noise_shaping_enabled(&self, enabled: bool) {
        self.noise_shaper.set_enabled(enabled);
    }
    /// Publish noise-shaping enablement, target bit depth, and curve as one
    /// coherent callback snapshot.
    pub fn set_noise_shaping(&self, enabled: bool, bits: u32, curve: NoiseShaperCurve) {
        self.noise_shaper.write(enabled, bits, curve);
    }
    /// Read latest dynamic-loudness telemetry from a control/UI thread.
    pub fn dynamic_loudness_telemetry(&self) -> DynamicLoudnessTelemetry {
        DynamicLoudnessTelemetry {
            factor: self.telemetry.factor(),
            band_gains_db: self.telemetry.band_gains(),
        }
    }

    // --- control-thread snapshot readers -----------------------------------

    /// Current linear volume multiplier.
    pub fn volume(&self) -> f64 {
        self.volume.read().volume
    }
    /// Current mute state.
    pub fn muted(&self) -> bool {
        self.volume.read().muted
    }
    /// Current equalizer enablement.
    pub fn eq_enabled(&self) -> bool {
        self.eq.read().enabled
    }
    /// Current equalizer band gains in dB.
    pub fn eq_band_gains_db(&self) -> [f64; crate::processor::EQ_BANDS] {
        self.eq.read().gains
    }
    /// Current limiter enablement.
    pub fn limiter_enabled(&self) -> bool {
        self.limiter.read().enabled
    }
    /// Current limiter threshold in dB.
    pub fn limiter_threshold_db(&self) -> f64 {
        self.limiter.read().threshold_db
    }
    /// Current crossfeed enablement, mix, and cutoff.
    pub fn crossfeed(&self) -> (bool, f64, f64) {
        let snapshot = self.crossfeed.read();
        (snapshot.enabled, snapshot.mix, snapshot.cutoff_hz)
    }
    /// Current dynamic-loudness enablement, listening volume in dBFS, and
    /// strength. Volume converts back from the internal linear domain.
    pub fn dynamic_loudness(&self) -> (bool, f64, f64) {
        let snapshot = self.dynamic_loudness.read();
        (
            snapshot.enabled,
            linear_to_db(snapshot.volume),
            snapshot.strength,
        )
    }
    /// Current noise-shaping enablement, bit depth, and curve.
    pub fn noise_shaping(&self) -> (bool, u32, NoiseShaperCurve) {
        let snapshot = self.noise_shaper.read();
        (snapshot.enabled, snapshot.bits, snapshot.curve)
    }
}

/// Convert a dBFS value to a linear amplitude multiplier.
fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Convert a linear amplitude multiplier back to dBFS.
fn linear_to_db(linear: f64) -> f64 {
    20.0 * linear.log10()
}

/// Non-cloneable lifecycle authority paired with one built pipeline.
///
/// It retains the private convolver lease; use [`Self::parameters`] to obtain
/// a clonable publisher for ordinary controls. Neither handle is callback-safe.
pub struct PlaybackController {
    parameters: PlaybackParameters,
    /// Private single-consumer convolver authority. Held so kernel adoption
    /// and drop-release lifecycles stay paired with this controller; never
    /// exposed directly through the high-level facade.
    convolver_lease: ConvolverControl,
    /// Callback channel count for validating impulse-response geometry.
    channels: usize,
    /// Callback sample rate for publishing kernels in the correct domain.
    sample_rate_hz: u32,
}
impl PlaybackController {
    /// Return a clonable, safe publisher for ordinary DSP parameters.
    pub fn parameters(&self) -> PlaybackParameters {
        self.parameters.clone()
    }
    /// Publish a linear volume multiplier.
    pub fn set_volume(&self, value: f64) {
        self.parameters.set_volume(value);
    }
    /// Publish mute state.
    pub fn set_muted(&self, muted: bool) {
        self.parameters.set_muted(muted);
    }
    /// Read latest dynamic-loudness telemetry from a control/UI thread.
    pub fn dynamic_loudness_telemetry(&self) -> DynamicLoudnessTelemetry {
        self.parameters.dynamic_loudness_telemetry()
    }
    /// Load an interleaved impulse response for convolution and enable it.
    ///
    /// The IR must be interleaved with the callback channel count and belong
    /// to the callback sample-rate domain of this controller's
    /// [`CallbackSpec`]. FFT preparation happens here on the control thread;
    /// the audio callback later adopts the prepared kernel without
    /// allocating. Publishing a new IR atomically supersedes the previous
    /// one. Returns the published kernel generation for status correlation.
    pub fn load_impulse_response(&self, interleaved_ir: &[f64]) -> Result<u64, ProcessError> {
        let kernel = FFTConvolver::new(interleaved_ir, self.channels)?;
        let generation = self
            .convolver_lease
            .publish_at_rate(kernel, self.sample_rate_hz)?;
        self.convolver_lease.set_enabled(true);
        Ok(generation)
    }
    /// Enable or bypass convolution without discarding the loaded kernel.
    pub fn set_convolution_enabled(&self, enabled: bool) {
        self.convolver_lease.set_enabled(enabled);
    }
    /// Read convolver adoption/reclamation telemetry for diagnostics.
    pub fn convolution_status(&self) -> ConvolverStatus {
        self.convolver_lease.status()
    }
    /// Reclaim any kernel retired by the audio thread. Call periodically from
    /// a control thread after superseding IRs so memory is returned promptly.
    pub fn reclaim_retired_convolution_kernels(&self) -> bool {
        self.convolver_lease.reclaim_retired()
    }
}

/// Control-thread builder for the canonical callback DSP stage order.
///
/// Building registers realtime snapshots and may allocate; complete it before
/// entering the audio callback. The resulting pair has one callback-owned
/// [`PlaybackPipeline`] and one non-cloneable [`PlaybackController`].
pub struct PlaybackBuilder {
    spec: CallbackSpec,
    config: PlaybackConfig,
}
impl PlaybackBuilder {
    /// Start with the transparent default configuration.
    pub fn new(spec: CallbackSpec) -> Self {
        Self {
            spec,
            config: PlaybackConfig::default(),
        }
    }
    /// Replace the initial intent-level configuration.
    pub fn configure(mut self, config: PlaybackConfig) -> Self {
        self.config = config;
        self
    }
    /// Materialize the canonical chain and its paired control authority.
    pub fn build(self) -> Result<(PlaybackPipeline, PlaybackController), ProcessError> {
        let volume = Arc::new(AtomicVolumeParams::new());
        volume.set_volume(self.config.volume);
        let eq = Arc::new(AtomicEqParams::new());
        eq.write(&self.config.eq_gains, self.config.eq_enabled);
        let limiter = Arc::new(AtomicPeakLimiterParams::new());
        limiter.set_enabled(self.config.limiter_enabled);
        limiter.set_threshold(self.config.limiter_threshold);
        let saturation = Arc::new(AtomicSaturationParams::new());
        saturation.write(SaturationParamsSnapshot {
            drive: self.config.saturation.drive,
            threshold: self.config.saturation.threshold,
            mix: self.config.saturation.mix,
            sat_type: self.config.saturation.saturation_type.into(),
            quality: self.config.saturation.quality.into(),
            input_gain_db: self.config.saturation.input_gain_db,
            output_gain_db: self.config.saturation.output_gain_db,
            highpass_mode: self.config.saturation.highpass_cutoff_hz.is_some(),
            highpass_cutoff: self.config.saturation.highpass_cutoff_hz.unwrap_or(4000.0),
            enabled: self.config.saturation.enabled,
            armed: self.config.saturation.enabled,
        });
        let crossfeed = Arc::new(AtomicCrossfeedParams::new());
        crossfeed.write(
            self.config.crossfeed.enabled,
            self.config.crossfeed.mix,
            self.config.crossfeed.cutoff_hz,
        );
        let dynamic_loudness = Arc::new(AtomicDynamicLoudnessParams::new());
        dynamic_loudness.write(
            self.config.dynamic_loudness.enabled,
            db_to_linear(self.config.dynamic_loudness.listening_volume_db),
            self.config.dynamic_loudness.strength,
        );
        let noise_shaper = Arc::new(AtomicNoiseShaperParams::new());
        noise_shaper.write(
            self.config.noise_shaping.enabled,
            self.config.noise_shaping.bits,
            self.config.noise_shaping.curve,
        );
        let telemetry = Arc::new(AtomicDynamicLoudnessTelemetry::new());
        let convolver = ConvolverControl::default();
        let params = OutputChainParams {
            channels: self.spec.channels,
            source_sample_rate: self.spec.sample_rate_hz,
            output_sample_rate: self.spec.sample_rate_hz,
            eq_params: Arc::clone(&eq),
            saturation_params: Arc::clone(&saturation),
            crossfeed_params: Arc::clone(&crossfeed),
            convolver_control: convolver.clone(),
            volume_params: Arc::clone(&volume),
            dynamic_loudness_params: Arc::clone(&dynamic_loudness),
            dynamic_loudness_telemetry: Arc::clone(&telemetry),
            limiter_params: Arc::clone(&limiter),
            noise_shaper_params: Arc::clone(&noise_shaper),
        };
        let pipeline =
            PlaybackPipeline::from_output_chain(&OutputChainBuilder::new(params), self.spec)?;
        Ok((
            pipeline,
            PlaybackController {
                parameters: PlaybackParameters {
                    volume,
                    eq,
                    limiter,
                    crossfeed,
                    dynamic_loudness,
                    noise_shaper,
                    telemetry,
                },
                convolver_lease: convolver,
                channels: self.spec.channels,
                sample_rate_hz: self.spec.sample_rate_hz,
            },
        ))
    }
}

/// Aggregate latency and tail declarations for a [`PlaybackPipeline`].
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct PlaybackTiming {
    pub latency: FrameDuration,
    pub tail: TailSpec,
}
/// Callback-owned processor for the canonical DSP stage order.
///
/// This value is intentionally not cloneable. Build it on a control thread,
/// then move it to exactly one audio callback. [`Self::process`] has no
/// allocation, locks, I/O, or logging for a valid prepared-capacity block.
pub struct PlaybackPipeline {
    chain: DspChain,
    spec: CallbackSpec,
}
impl PlaybackPipeline {
    /// Start control-thread configuration for one callback-owned pipeline.
    pub fn builder(spec: CallbackSpec) -> PlaybackBuilder {
        PlaybackBuilder::new(spec)
    }
    /// Build from an advanced canonical-chain builder after validating this callback spec.
    ///
    /// This is a control-thread operation for integrations that intentionally
    /// use the lower-level output-chain API.
    pub fn from_output_chain(
        builder: &OutputChainBuilder,
        spec: CallbackSpec,
    ) -> Result<Self, ProcessError> {
        if builder.channels() != spec.channels {
            return Err(ProcessError::ChannelCountMismatch {
                processor: "PlaybackPipeline",
                expected_channels: spec.channels,
                actual_channels: builder.channels(),
            });
        }
        if builder.output_sample_rate_hz() != spec.sample_rate_hz {
            return Err(ProcessError::SampleRateMismatch {
                processor: "PlaybackPipeline",
                expected_sample_rate_hz: spec.sample_rate_hz,
                actual_sample_rate_hz: builder.output_sample_rate_hz(),
            });
        }
        Ok(Self {
            chain: builder.build_callback_chain()?,
            spec,
        })
    }
    /// Return the prepared callback-domain specification.
    pub const fn spec(&self) -> CallbackSpec {
        self.spec
    }
    /// Return declared algorithmic latency and tail for drain scheduling.
    pub fn timing(&self) -> PlaybackTiming {
        PlaybackTiming {
            latency: self.chain.latency(),
            tail: self.chain.tail(),
        }
    }
    /// Whether the most recent bounded finish operation reached its tail cap.
    pub fn finish_was_capped(&self) -> bool {
        self.chain.finish_was_capped()
    }
    fn validate(&self, samples: &[f64], operation: &'static str) -> Result<(), ProcessError> {
        if !samples.len().is_multiple_of(self.spec.channels) {
            return Err(ProcessError::InvalidGeometry {
                processor: "PlaybackPipeline",
                operation,
                message: "sample count is not a whole number of callback frames",
            });
        }
        if samples.len() / self.spec.channels > self.spec.max_frames {
            return Err(ProcessError::InvalidGeometry {
                processor: "PlaybackPipeline",
                operation,
                message: "callback block exceeds prepared maximum frames",
            });
        }
        Ok(())
    }
    /// Process one prepared-capacity interleaved callback block in place.
    ///
    /// This is the only facade operation intended for the audio callback. It
    /// performs no allocation, locking, I/O, or logging for a valid block.
    pub fn process(&mut self, samples: &mut [f64]) -> Result<ProcessProgress, ProcessError> {
        self.validate(samples, "process")?;
        self.chain.process(samples, self.spec.channels)
    }
    /// Advance explicit end-of-stream draining into caller-owned storage.
    ///
    /// Call outside the audio callback using a policy that bounds unknown or
    /// infinite tails. Repeat until `ProcessState::Finished`, then call
    /// [`Self::reset`] before processing a new logical stream.
    pub fn finish_into_with_policy(
        &mut self,
        output: &mut [f64],
        policy: ChainFinishPolicy,
    ) -> Result<ProcessProgress, ProcessError> {
        self.validate(output, "finish")?;
        self.chain
            .finish_with_policy(AudioBlockMut::new(output, self.spec.channels)?, policy)
    }
    /// Clear stateful DSP history and re-arm processing for a new logical stream.
    ///
    /// Reset is a lifecycle/control-thread operation, not callback-safe.
    pub fn reset(&mut self) -> Result<(), ProcessError> {
        self.chain.reset()
    }
}
/// Simple ring buffer for audio data
/// Uses monotonic counters (frames_written, frames_consumed) for clean overflow handling.
pub struct RingBuffer {
    data: Vec<f64>,
    capacity_frames: usize,
    channels: usize,
    /// Total frames written (monotonically increasing)
    frames_written: u64,
    /// Total frames consumed by readers (monotonically increasing)
    frames_consumed: u64,
    /// Number of overflow events
    overflow_count: u64,
}

impl RingBuffer {
    pub fn new(capacity_frames: usize, channels: usize) -> Self {
        Self {
            data: vec![0.0; capacity_frames * channels],
            capacity_frames,
            channels,
            frames_written: 0,
            frames_consumed: 0,
            overflow_count: 0,
        }
    }

    /// Write frames to the buffer, returns number of frames written
    /// If buffer would overflow, drops the oldest data (ring buffer behavior)
    /// Returns (frames_written, overflow_new_consumed) — overflow_new_consumed is
    /// the updated frames_consumed value that external read positions must respect.
    pub fn write(&mut self, samples: &[f64]) -> (usize, Option<u64>) {
        let frames_to_write = samples.len() / self.channels;
        let samples_to_write = frames_to_write * self.channels;

        if frames_to_write == 0 {
            return (0, None);
        }

        // Check for potential overflow
        let frames_in_buffer = self.frames_written.saturating_sub(self.frames_consumed);
        let available_space = self
            .capacity_frames
            .saturating_sub(frames_in_buffer as usize);

        let overflow_consumed = if frames_to_write > available_space {
            // Overflow detected - advance consumer position to make room
            // This effectively drops the oldest frames
            let overflow_frames = frames_to_write - available_space;
            self.frames_consumed = self.frames_consumed.saturating_add(overflow_frames as u64);
            self.overflow_count = self.overflow_count.saturating_add(1);
            log::warn!(
                "RingBuffer overflow: dropping {} frames (total overflows: {})",
                overflow_frames,
                self.overflow_count
            );
            Some(self.frames_consumed)
        } else {
            None
        };

        // Write samples using at most two contiguous copies split at the wrap boundary.
        let frames_to_copy = frames_to_write.min(self.capacity_frames);
        let source_frame_offset = frames_to_write - frames_to_copy;
        let source_sample_offset = source_frame_offset * self.channels;
        let write_frame = ((self.frames_written % self.capacity_frames as u64) as usize
            + source_frame_offset)
            % self.capacity_frames;
        self.copy_frames_from_slice(
            write_frame,
            &samples[source_sample_offset..samples_to_write],
            frames_to_copy,
        );

        self.frames_written += frames_to_write as u64;
        (frames_to_write, overflow_consumed)
    }

    /// Read frames from the buffer at a given position
    pub fn read(&self, start_frame: u64, output: &mut [f64]) -> usize {
        let frames_to_read = output.len() / self.channels;
        let available = self.frames_written.saturating_sub(start_frame) as usize;
        let actual_frames = frames_to_read.min(available);

        if actual_frames == 0 {
            return 0;
        }

        let read_frame = (start_frame % self.capacity_frames as u64) as usize;
        self.copy_frames_to_slice(
            read_frame,
            &mut output[..actual_frames * self.channels],
            actual_frames,
        );

        actual_frames
    }

    fn copy_frames_from_slice(&mut self, start_frame: usize, source: &[f64], frames: usize) {
        let first_frames = frames.min(self.capacity_frames - start_frame);
        let first_samples = first_frames * self.channels;
        let start_sample = start_frame * self.channels;

        self.data[start_sample..start_sample + first_samples]
            .copy_from_slice(&source[..first_samples]);

        let remaining_frames = frames - first_frames;
        if remaining_frames > 0 {
            let remaining_samples = remaining_frames * self.channels;
            self.data[..remaining_samples]
                .copy_from_slice(&source[first_samples..first_samples + remaining_samples]);
        }
    }

    fn copy_frames_to_slice(&self, start_frame: usize, output: &mut [f64], frames: usize) {
        let first_frames = frames.min(self.capacity_frames - start_frame);
        let first_samples = first_frames * self.channels;
        let start_sample = start_frame * self.channels;

        output[..first_samples]
            .copy_from_slice(&self.data[start_sample..start_sample + first_samples]);

        let remaining_frames = frames - first_frames;
        if remaining_frames > 0 {
            let remaining_samples = remaining_frames * self.channels;
            output[first_samples..first_samples + remaining_samples]
                .copy_from_slice(&self.data[..remaining_samples]);
        }
    }

    /// Update consumed position (call after reading)
    pub fn advance_read_pos(&mut self, frames: u64) {
        self.frames_consumed = self.frames_consumed.saturating_add(frames);
    }

    /// Get number of frames available for reading from a given position
    pub fn available_frames(&self, read_pos: u64) -> u64 {
        self.frames_written.saturating_sub(read_pos)
    }

    /// Get total frames written
    pub fn total_written(&self) -> u64 {
        self.frames_written
    }

    /// Get overflow count
    pub fn overflow_count(&self) -> u64 {
        self.overflow_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(frames: usize, channels: usize, start: f64) -> Vec<f64> {
        (0..frames * channels).map(|i| start + i as f64).collect()
    }

    #[test]
    fn ring_buffer_reads_back_exact_capacity() {
        let mut buffer = RingBuffer::new(4, 2);
        let input = samples(4, 2, 1.0);
        let mut output = vec![0.0; input.len()];

        assert_eq!(buffer.write(&input), (4, None));
        assert_eq!(buffer.read(0, &mut output), 4);
        assert_eq!(output, input);
    }

    #[test]
    fn ring_buffer_write_and_read_wrap_preserve_order() {
        let mut buffer = RingBuffer::new(4, 2);
        let first = samples(3, 2, 1.0);
        let second = samples(3, 2, 101.0);

        assert_eq!(buffer.write(&first), (3, None));
        buffer.advance_read_pos(2);
        assert_eq!(buffer.write(&second), (3, None));

        let mut output = vec![0.0; 4 * 2];
        assert_eq!(buffer.read(2, &mut output), 4);

        let mut expected = first[2 * 2..].to_vec();
        expected.extend_from_slice(&second);
        assert_eq!(output, expected);
    }

    #[test]
    fn ring_buffer_overflow_keeps_newest_frames_and_reports_consumed_position() {
        let mut buffer = RingBuffer::new(4, 2);
        let input = samples(6, 2, 1.0);
        let mut output = vec![0.0; 4 * 2];

        assert_eq!(buffer.write(&input), (6, Some(2)));
        assert_eq!(buffer.overflow_count(), 1);
        assert_eq!(buffer.read(2, &mut output), 4);
        assert_eq!(output, input[2 * 2..].to_vec());
    }

    #[test]
    fn ring_buffer_empty_read_leaves_output_untouched() {
        let buffer = RingBuffer::new(4, 2);
        let mut output = vec![42.0; 4];

        assert_eq!(buffer.read(0, &mut output), 0);
        assert_eq!(output, vec![42.0; 4]);
    }

    #[test]
    fn ring_buffer_partial_read_only_copies_available_frames() {
        let mut buffer = RingBuffer::new(8, 2);
        let input = samples(2, 2, 1.0);
        let mut output = vec![42.0; 4 * 2];

        assert_eq!(buffer.write(&input), (2, None));
        assert_eq!(buffer.read(0, &mut output), 2);
        assert_eq!(&output[..4], &input[..]);
        assert_eq!(&output[4..], &[42.0; 4]);
    }

    #[test]
    fn ring_buffer_wrap_preserves_multichannel_interleaving() {
        let channels = 6;
        let mut buffer = RingBuffer::new(4, channels);
        let first = samples(3, channels, 1.0);
        let second = samples(3, channels, 101.0);

        assert_eq!(buffer.write(&first), (3, None));
        buffer.advance_read_pos(2);
        assert_eq!(buffer.write(&second), (3, None));

        let mut output = vec![0.0; 4 * channels];
        assert_eq!(buffer.read(2, &mut output), 4);

        let mut expected = first[2 * channels..].to_vec();
        expected.extend_from_slice(&second);
        assert_eq!(output, expected);
    }
}

#[cfg(test)]
mod playback_facade_tests {
    use super::*;
    use crate::processor::ProcessState;

    const RATE: u32 = 48_000;
    const MAX: usize = 64;

    fn spec() -> CallbackSpec {
        CallbackSpec::stereo(RATE, MAX).unwrap()
    }

    #[test]
    fn callback_spec_validates_all_prepare_bounds() {
        assert!(CallbackSpec::new(0, RATE, MAX).is_err());
        assert!(CallbackSpec::new(2, 0, MAX).is_err());
        assert!(CallbackSpec::new(2, RATE, 0).is_err());
    }

    #[test]
    fn controller_updates_and_capacity_validation_are_exposed_without_raw_handles() {
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec())
            .configure(PlaybackConfig::transparent().with_volume(0.5))
            .build()
            .unwrap();
        let parameters = controller.parameters();
        parameters.set_eq_enabled(true);
        parameters.set_eq_band_gain_db(0, 2.0);
        parameters.set_limiter_threshold_db(-1.0);
        assert!(pipeline.process(&mut [0.25, -0.25]).is_ok());
        assert!(matches!(
            pipeline.process(&mut vec![0.0; (MAX + 1) * 2]),
            Err(ProcessError::InvalidGeometry { .. })
        ));
    }

    #[test]
    fn intent_level_effect_configs_build_and_parameter_handle_is_cloneable() {
        let config = PlaybackConfig::transparent()
            .with_saturation(
                PlaybackSaturationConfig::enabled()
                    .with_drive(0.6)
                    .with_mix(0.5),
            )
            .with_crossfeed(PlaybackCrossfeedConfig::enabled(0.4, 800.0))
            .with_dynamic_loudness(PlaybackDynamicLoudnessConfig::enabled(-18.0, 0.75))
            .with_noise_shaping(PlaybackNoiseShapingConfig::enabled(
                16,
                NoiseShaperCurve::TpdfOnly,
            ));
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec())
            .configure(config)
            .build()
            .unwrap();
        let parameters = controller.parameters();
        let remote = parameters.clone();
        assert!(matches!(
            parameters.set_saturation_drive(1.0),
            Err(ProcessError::UnsupportedOperation { .. })
        ));
        parameters.set_crossfeed(true, 0.25, 900.0);
        remote.set_dynamic_loudness(true, -20.0, 0.5);
        remote.set_noise_shaping(true, 20, NoiseShaperCurve::Lipshitz5);
        assert!(parameters.set_saturation_enabled(true).is_err());
        assert!(pipeline.process(&mut [0.1, -0.1]).is_ok());
    }

    #[test]
    fn cloned_parameter_publishers_can_update_while_audio_processes() {
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        let first = controller.parameters();
        let second = first.clone();
        let worker = std::thread::spawn(move || {
            for index in 0..256 {
                second.set_volume((index % 101) as f64 / 100.0);
            }
        });
        for _ in 0..256 {
            let _ = pipeline.process(&mut [0.25, -0.25]).unwrap();
        }
        worker.join().unwrap();
        first.set_volume(0.5);
        let mut samples = [1.0, -1.0];
        let _ = pipeline.process(&mut samples).unwrap();
        assert!(samples.iter().all(|sample| sample.abs() <= 1.0));
    }

    #[test]
    fn zero_length_block_is_an_accepted_no_op() {
        let (mut pipeline, _) = PlaybackPipeline::builder(spec()).build().unwrap();
        let progress = pipeline.process(&mut []).unwrap();
        assert_eq!(progress.consumed_frames(), 0);
        assert_eq!(progress.produced_frames(), 0);
        // A zero-capacity finish buffer makes no progress and reports
        // `NeedOutput` instead of erroring or spinning to completion.
        let finish = pipeline
            .finish_into_with_policy(&mut [], ChainFinishPolicy::default())
            .unwrap();
        assert_eq!(finish.produced_frames(), 0);
        assert_eq!(finish.state(), ProcessState::NeedOutput);
    }

    #[test]
    fn transparent_default_is_sample_identical() {
        let input = [0.25, -0.25, 0.75, -0.75, 0.0, 0.5];
        for config in [PlaybackConfig::transparent(), PlaybackConfig::default()] {
            let (mut pipeline, _) = PlaybackPipeline::builder(spec())
                .configure(config)
                .build()
                .unwrap();
            let mut samples = input;
            let _ = pipeline.process(&mut samples).unwrap();
            assert_eq!(samples, input);
        }
    }

    #[test]
    fn high_level_pipeline_relinquishes_its_private_convolver_lease_on_drop() {
        let (pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();

        // This test-only inspection stays within the facade module: the public
        // controller never exposes this handle. While the pipeline owns the
        // canonical chain, a second consumer of its private control is refused.
        assert!(matches!(
            crate::processor::ConvolverProcessor::new(controller.convolver_lease.clone()),
            Err(ProcessError::ConsumerAlreadyActive { .. })
        ));
        drop(pipeline);

        // Dropping the callback-owned pipeline releases the private lease even
        // while its control-thread controller remains alive.
        assert!(
            crate::processor::ConvolverProcessor::new(controller.convolver_lease.clone()).is_ok()
        );
    }

    #[test]
    fn parameter_snapshot_readers_round_trip_control_writes() {
        let (_pipeline, controller) = PlaybackPipeline::builder(spec())
            .configure(PlaybackConfig::transparent().with_volume(0.8))
            .build()
            .unwrap();
        let parameters = controller.parameters();
        assert!((parameters.volume() - 0.8).abs() < 1e-12);
        assert!(!parameters.muted());

        let mut gains = [0.0; crate::processor::EQ_BANDS];
        gains[3] = 4.5;
        parameters.set_eq(true, gains);
        assert!(parameters.eq_enabled());
        assert_eq!(parameters.eq_band_gains_db(), gains);

        parameters.set_limiter_enabled(true);
        parameters.set_limiter_threshold_db(-2.0);
        assert!(parameters.limiter_enabled());
        assert!((parameters.limiter_threshold_db() + 2.0).abs() < 1e-12);

        parameters.set_crossfeed(true, 0.3, 650.0);
        assert_eq!(parameters.crossfeed(), (true, 0.3, 650.0));

        parameters.set_dynamic_loudness(true, -14.0, 0.6);
        let (enabled, listening_volume_db, strength) = parameters.dynamic_loudness();
        assert!(enabled);
        assert!((listening_volume_db + 14.0).abs() < 1e-9);
        assert!((strength - 0.6).abs() < 1e-12);

        parameters.set_noise_shaping(true, 16, NoiseShaperCurve::TpdfOnly);
        assert_eq!(
            parameters.noise_shaping(),
            (true, 16, NoiseShaperCurve::TpdfOnly)
        );
    }

    #[test]
    fn impulse_response_loading_validates_geometry_and_reports_status() {
        let (_pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();

        // Wrong interleave (odd sample count for stereo) is refused before the
        // kernel can reach the audio consumer.
        assert!(controller.load_impulse_response(&[1.0, 0.0, 0.5]).is_err());

        // A valid stereo IR publishes, enables convolution, and each
        // publication advances the generation.
        let first = controller
            .load_impulse_response(&[1.0, 1.0, 0.25, 0.25])
            .unwrap();
        let second = controller.load_impulse_response(&[1.0, 1.0]).unwrap();
        assert!(second > first);
        let status = controller.convolution_status();
        assert!(status.enabled);
        assert_eq!(status.latest_published_generation, second);

        controller.set_convolution_enabled(false);
        assert!(!controller.convolution_status().enabled);
        // Reclamation is a no-op here (nothing retired by an audio thread yet)
        // but must be callable from the control thread.
        let _ = controller.reclaim_retired_convolution_kernels();
    }

    #[test]
    fn timing_and_explicit_drain_policy_are_forwarded() {
        let (mut pipeline, _) = PlaybackPipeline::builder(spec()).build().unwrap();
        let _timing = pipeline.timing();
        let mut input = [0.25, -0.25];
        let _ = pipeline.process(&mut input).unwrap();
        let mut output = [0.0; MAX * 2];
        loop {
            if pipeline
                .finish_into_with_policy(&mut output, ChainFinishPolicy::default())
                .unwrap()
                .state()
                == ProcessState::Finished
            {
                break;
            }
        }
        assert!(matches!(
            pipeline.process(&mut input),
            Err(ProcessError::AlreadyFinished { .. })
        ));
        pipeline.reset().unwrap();
        assert!(pipeline.process(&mut input).is_ok());
    }

    #[test]
    fn first_process_and_finish_are_allocation_free_on_fresh_threads() {
        std::thread::spawn(|| {
            let (mut pipeline, _) = PlaybackPipeline::builder(spec()).build().unwrap();
            let mut samples = [0.0; 2];
            assert_no_alloc::assert_no_alloc(|| {
                let _ = pipeline.process(&mut samples).unwrap();
            });
        })
        .join()
        .unwrap();
        std::thread::spawn(|| {
            let (mut pipeline, _) = PlaybackPipeline::builder(spec()).build().unwrap();
            let mut output = [0.0; MAX * 2];
            assert_no_alloc::assert_no_alloc(|| {
                let _ = pipeline
                    .finish_into_with_policy(&mut output, ChainFinishPolicy::default())
                    .unwrap();
            });
        })
        .join()
        .unwrap();
    }
}
