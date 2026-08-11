//! Caller-driven callback DSP facade and ring-buffer primitive.
//!
//! The public facade separates control-thread construction/configuration from
//! the callback-owned [`PlaybackPipeline`]. Its [`CallbackSpec`] describes
//! already-converted device-domain `f64` audio; it does not negotiate devices,
//! decode files, or resample source audio.
use crate::processor::lockfree_params::validate_eq_band_index;
use crate::processor::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry,
    AtomicEqParams, AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams,
    AtomicVolumeParams, AudioBlockMut, ChainFinishPolicy, ConvolverControl, ConvolverStatus,
    DspChain, FFTConvolver, FrameDuration, NoiseShaperCurve, OutputChainBuilder, OutputChainParams,
    ProcessError, ProcessProgress, ProcessState, SaturationParamsSnapshot, SaturationQuality,
    SaturationType, TailSpec, LOUDNESS_BANDS_N,
};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

/// Documented value ranges for the facade's intent-level controls.
///
/// A finite value outside its range is clamped by the parameter layer, so a UI
/// can use these bounds directly for its widgets.
pub use crate::processor::{
    CROSSFEED_CUTOFF_HZ_MAX, CROSSFEED_CUTOFF_HZ_MIN, CROSSFEED_MIX_MAX, CROSSFEED_MIX_MIN,
    DYNAMIC_LOUDNESS_STRENGTH_MAX, DYNAMIC_LOUDNESS_STRENGTH_MIN, EQ_BAND_GAIN_DB_MAX,
    EQ_BAND_GAIN_DB_MIN, LIMITER_THRESHOLD_DB_MAX, LIMITER_THRESHOLD_DB_MIN, NOISE_SHAPER_BITS_MAX,
    NOISE_SHAPER_BITS_MIN, SATURATION_DRIVE_MAX, SATURATION_DRIVE_MIN, SATURATION_GAIN_DB_MAX,
    SATURATION_GAIN_DB_MIN, SATURATION_HIGHPASS_CUTOFF_HZ_MAX, SATURATION_HIGHPASS_CUTOFF_HZ_MIN,
    SATURATION_MIX_MAX, SATURATION_MIX_MIN, SATURATION_THRESHOLD_MAX, SATURATION_THRESHOLD_MIN,
    VOLUME_MAX, VOLUME_MIN,
};

use crate::processor::lockfree_params::{CROSSFEED_CUTOFF_HZ_DEFAULT, CROSSFEED_MIX_DEFAULT};

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

/// Longest accepted stop ramp, in milliseconds.
pub const MAX_STOP_FADE_MS: u32 = 60_000;

/// Quietest dynamic-loudness listening volume accepted by the facade, in dBFS.
///
/// The stage stores a linear volume, so the facade bounds the dB domain
/// instead of letting `-inf` round-trip through a reader.
pub const DYNAMIC_LOUDNESS_VOLUME_DB_MIN: f64 = -120.0;
/// Loudest dynamic-loudness listening volume accepted by the facade, in dBFS.
pub const DYNAMIC_LOUDNESS_VOLUME_DB_MAX: f64 = 0.0;

/// Reject a non-finite control value at the public boundary.
///
/// Finite out-of-range values are clamped by the parameter layer instead, so a
/// UI slider never fails on float noise.
fn checked_parameter(
    processor: &'static str,
    parameter: &'static str,
    value: f64,
) -> Result<f64, ProcessError> {
    if value.is_finite() {
        return Ok(value);
    }
    Err(ProcessError::InvalidParameter {
        processor,
        parameter,
        message: "value must be finite",
    })
}

/// Reject a configuration value that is non-finite or outside its documented
/// range. Build-time configuration is strict so a bad preset surfaces at once.
fn checked_config_value(
    parameter: &'static str,
    value: f64,
    min: f64,
    max: f64,
) -> Result<f64, ProcessError> {
    if value.is_finite() && value >= min && value <= max {
        return Ok(value);
    }
    Err(ProcessError::InvalidParameter {
        processor: "PlaybackConfig",
        parameter,
        message: "value must be finite and inside its documented range",
    })
}

/// Lifecycle position of a [`PlaybackPipeline`] as seen by the audio callback.
///
/// `#[non_exhaustive]`: later work (for example gapless next-stream preroll)
/// may add states, so match with a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlaybackLifecycleState {
    /// Ordinary processing: the callback block is filtered in place.
    Running,
    /// A stop ramp is being applied before the tail is drained.
    FadingOut,
    /// The remaining effect tail is rendered into the callback block and the
    /// caller's input samples are ignored.
    Draining,
    /// The stream is terminal. `process` writes silence and succeeds until
    /// [`PlaybackController::request_reset`] re-arms the pipeline.
    Idle,
}
impl PlaybackLifecycleState {
    const fn as_code(self) -> u8 {
        match self {
            Self::Running => 0,
            Self::FadingOut => 1,
            Self::Draining => 2,
            Self::Idle => 3,
        }
    }
    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::FadingOut,
            2 => Self::Draining,
            3 => Self::Idle,
            _ => Self::Running,
        }
    }
}

/// Control-thread view of the lifecycle channel.
///
/// `requested_generation` is the newest request published by this controller;
/// `applied_generation` is the newest request the audio callback has consumed.
/// They are equal once every request has taken effect.
///
/// `#[non_exhaustive]`: reserved for later gapless/next-stream reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlaybackLifecycleStatus {
    /// Current lifecycle state of the pipeline.
    pub state: PlaybackLifecycleState,
    /// Newest request generation published by the controller.
    pub requested_generation: u64,
    /// Newest request generation consumed by the audio callback.
    pub applied_generation: u64,
}
impl PlaybackLifecycleStatus {
    /// Whether the callback has consumed every published request.
    pub const fn is_settled(self) -> bool {
        self.applied_generation == self.requested_generation
    }
}

// Request kinds packed into the lifecycle word. Values 4.. stay reserved for
// later transitions (for example gapless preroll); an unknown kind is ignored
// by the callback instead of aborting the stream.
const REQUEST_NONE: u8 = 0;
const REQUEST_RESET: u8 = 1;
const REQUEST_DRAIN: u8 = 2;
const REQUEST_STOP_WITH_FADE: u8 = 3;

/// Lock-free control-to-callback lifecycle channel.
///
/// One `u64` carries the complete request — `[generation:40][fade_ms:16][kind:8]`
/// — so the callback always observes a coherent request without a lock or a
/// second load. Requests coalesce: if several are published between two
/// callback blocks, only the newest takes effect.
#[derive(Debug)]
struct LifecycleChannel {
    request: AtomicU64,
    applied_generation: AtomicU64,
    state: AtomicU8,
}
impl LifecycleChannel {
    fn new() -> Self {
        Self {
            request: AtomicU64::new(0),
            applied_generation: AtomicU64::new(0),
            state: AtomicU8::new(PlaybackLifecycleState::Running.as_code()),
        }
    }
    /// Publish one request from a control thread and return its generation.
    fn publish(&self, kind: u8, fade_ms: u16) -> u64 {
        let mut current = self.request.load(Ordering::Acquire);
        loop {
            let generation = (current >> 24) + 1;
            let word = (generation << 24) | ((fade_ms as u64) << 8) | kind as u64;
            match self.request.compare_exchange_weak(
                current,
                word,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return generation,
                Err(observed) => current = observed,
            }
        }
    }
    /// Read a request newer than `seen_generation`. Callback-side: one atomic
    /// load, no allocation, no waiting.
    fn take_newer_than(&self, seen_generation: u64) -> Option<(u64, u8, u16)> {
        let word = self.request.load(Ordering::Acquire);
        let generation = word >> 24;
        if generation == seen_generation {
            return None;
        }
        Some((
            generation,
            (word & 0xff) as u8,
            ((word >> 8) & 0xffff) as u16,
        ))
    }
    fn requested_generation(&self) -> u64 {
        self.request.load(Ordering::Acquire) >> 24
    }
    fn status(&self) -> PlaybackLifecycleStatus {
        // Read `applied` first so a status that claims settled can never be
        // paired with a newer request than the callback has actually seen.
        let applied_generation = self.applied_generation.load(Ordering::Acquire);
        PlaybackLifecycleStatus {
            state: PlaybackLifecycleState::from_code(self.state.load(Ordering::Acquire)),
            requested_generation: self.requested_generation().max(applied_generation),
            applied_generation,
        }
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
    /// Whether the saturation stage is active.
    pub enabled: bool,
    /// Tube or tape-style transfer curve.
    pub saturation_type: SaturationType,
    /// Processing quality / antialiasing mode.
    pub quality: SaturationQuality,
    /// Drive amount (0.0–2.0, default 0.25).
    pub drive: f64,
    /// Linear threshold where saturation begins (default 0.88).
    pub threshold: f64,
    /// Dry/wet mix (0.0–1.0, default 0.2).
    pub mix: f64,
    /// Input gain applied before the transfer curve, in dB.
    pub input_gain_db: f64,
    /// Output gain applied after the transfer curve, in dB.
    pub output_gain_db: f64,
    /// Highpass cutoff for the "exciter" mode; saturates only above it.
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
    /// Whether the crossfeed stage is active.
    pub enabled: bool,
    /// Dry/wet linear crossfeed mix.
    pub mix: f64,
    /// Crossfeed lowpass cutoff in Hz.
    pub cutoff_hz: f64,
}
impl PlaybackCrossfeedConfig {
    /// A bypassed crossfeed stage.
    ///
    /// The inert mix and cutoff are the crossfeed core's own defaults, so
    /// enabling the stage without naming a profile keeps one description of the
    /// Bauer reference.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            mix: CROSSFEED_MIX_DEFAULT,
            cutoff_hz: CROSSFEED_CUTOFF_HZ_DEFAULT,
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
    /// Whether dynamic-loudness compensation is active.
    pub enabled: bool,
    /// Current playback volume in dBFS used as the compensation reference.
    pub listening_volume_db: f64,
    /// Compensation amount (0.0–1.0).
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
    /// Whether output noise shaping / dithering is active.
    pub enabled: bool,
    /// Target quantizer bit depth of the noise-shaping stage.
    pub bits: u32,
    /// Noise-shaping error curve.
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
    drain_policy: ChainFinishPolicy,
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
            drain_policy: ChainFinishPolicy::default(),
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
    /// Bound the tail rendered by a callback-side drain and by
    /// [`PlaybackPipeline::finish_into_with_policy`]'s default caller.
    ///
    /// The policy is fixed at build time so a
    /// [`PlaybackController::request_drain`] carries no payload across the
    /// thread boundary. A policy that cannot bound a tail is rejected by
    /// [`PlaybackBuilder::build`], not by the audio thread's first drain.
    pub fn with_drain_policy(mut self, policy: ChainFinishPolicy) -> Self {
        self.drain_policy = policy;
        self
    }
    /// The bounded drain policy applied when the callback renders a tail.
    pub const fn drain_policy(&self) -> ChainFinishPolicy {
        self.drain_policy
    }
    /// Reject non-finite or out-of-range configuration before any DSP state
    /// exists.
    fn validate(&self) -> Result<(), ProcessError> {
        checked_config_value("volume", self.volume, VOLUME_MIN, VOLUME_MAX)?;
        for gain in self.eq_gains {
            checked_config_value(
                "eq band gain",
                gain,
                EQ_BAND_GAIN_DB_MIN,
                EQ_BAND_GAIN_DB_MAX,
            )?;
        }
        checked_config_value(
            "limiter threshold",
            self.limiter_threshold,
            LIMITER_THRESHOLD_DB_MIN,
            LIMITER_THRESHOLD_DB_MAX,
        )?;
        checked_config_value(
            "saturation drive",
            self.saturation.drive,
            SATURATION_DRIVE_MIN,
            SATURATION_DRIVE_MAX,
        )?;
        checked_config_value(
            "saturation threshold",
            self.saturation.threshold,
            SATURATION_THRESHOLD_MIN,
            SATURATION_THRESHOLD_MAX,
        )?;
        checked_config_value(
            "saturation mix",
            self.saturation.mix,
            SATURATION_MIX_MIN,
            SATURATION_MIX_MAX,
        )?;
        checked_config_value(
            "saturation input gain",
            self.saturation.input_gain_db,
            SATURATION_GAIN_DB_MIN,
            SATURATION_GAIN_DB_MAX,
        )?;
        checked_config_value(
            "saturation output gain",
            self.saturation.output_gain_db,
            SATURATION_GAIN_DB_MIN,
            SATURATION_GAIN_DB_MAX,
        )?;
        if let Some(cutoff_hz) = self.saturation.highpass_cutoff_hz {
            checked_config_value(
                "saturation highpass cutoff",
                cutoff_hz,
                SATURATION_HIGHPASS_CUTOFF_HZ_MIN,
                SATURATION_HIGHPASS_CUTOFF_HZ_MAX,
            )?;
        }
        checked_config_value(
            "crossfeed mix",
            self.crossfeed.mix,
            CROSSFEED_MIX_MIN,
            CROSSFEED_MIX_MAX,
        )?;
        checked_config_value(
            "crossfeed cutoff",
            self.crossfeed.cutoff_hz,
            CROSSFEED_CUTOFF_HZ_MIN,
            CROSSFEED_CUTOFF_HZ_MAX,
        )?;
        checked_config_value(
            "dynamic loudness listening volume",
            self.dynamic_loudness.listening_volume_db,
            DYNAMIC_LOUDNESS_VOLUME_DB_MIN,
            DYNAMIC_LOUDNESS_VOLUME_DB_MAX,
        )?;
        checked_config_value(
            "dynamic loudness strength",
            self.dynamic_loudness.strength,
            DYNAMIC_LOUDNESS_STRENGTH_MIN,
            DYNAMIC_LOUDNESS_STRENGTH_MAX,
        )?;
        if self.noise_shaping.bits < NOISE_SHAPER_BITS_MIN
            || self.noise_shaping.bits > NOISE_SHAPER_BITS_MAX
        {
            return Err(ProcessError::InvalidParameter {
                processor: "PlaybackConfig",
                parameter: "noise shaping bits",
                message: "value must be finite and inside its documented range",
            });
        }
        // The drain policy is fixed at build time, so an unbounded-tail preset
        // must fail here rather than on the callback thread's first drain.
        self.drain_policy.validate()?;
        Ok(())
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
    /// Latest measured gain factor applied by the loudness model.
    pub factor: f64,
    /// One gain per band of the dynamic-loudness model, in model band order.
    /// Sized by [`LOUDNESS_BANDS_N`] so the model and its readout cannot drift.
    pub band_gains_db: [f64; LOUDNESS_BANDS_N],
}

/// Cloneable, control-thread-only parameter publisher for ordinary callback
/// controls. Each update becomes visible as one complete snapshot at a later
/// callback block boundary. It owns no convolver authority and cannot build a
/// second audio consumer.
///
/// Value contract: a non-finite value is rejected with
/// `ProcessError::InvalidParameter` and never reaches DSP state. A finite
/// out-of-range value is clamped into its documented range, and every reader
/// on this type returns the value actually in effect. An out-of-range *index*
/// such as an equalizer band is rejected rather than clamped, so an `Ok` result
/// always means the requested edit reached the callback.
#[derive(Clone)]
pub struct PlaybackParameters {
    volume: Arc<AtomicVolumeParams>,
    eq: Arc<AtomicEqParams>,
    limiter: Arc<AtomicPeakLimiterParams>,
    saturation: Arc<AtomicSaturationParams>,
    crossfeed: Arc<AtomicCrossfeedParams>,
    dynamic_loudness: Arc<AtomicDynamicLoudnessParams>,
    noise_shaper: Arc<AtomicNoiseShaperParams>,
    telemetry: Arc<AtomicDynamicLoudnessTelemetry>,
    /// Whether the saturation stage was armed at build time. Arming fixes the
    /// stage's latency, so it cannot change while the pipeline is live.
    saturation_armed: bool,
}
impl PlaybackParameters {
    /// Publish a linear volume multiplier, clamped to
    /// [`VOLUME_MIN`]..=[`VOLUME_MAX`] (the callback stage attenuates only).
    pub fn set_volume(&self, value: f64) -> Result<(), ProcessError> {
        self.volume
            .set_volume(checked_parameter("PlaybackParameters", "volume", value)?);
        Ok(())
    }
    /// Publish mute state.
    pub fn set_muted(&self, muted: bool) {
        self.volume.set_muted(muted);
    }
    /// Enable or bypass the equalizer.
    pub fn set_eq_enabled(&self, enabled: bool) {
        self.eq.set_enabled(enabled);
    }
    /// Publish one equalizer band gain in dB, clamped to
    /// [`EQ_BAND_GAIN_DB_MIN`]..=[`EQ_BAND_GAIN_DB_MAX`].
    ///
    /// `band` addresses one of [`EQ_BANDS`](crate::processor::EQ_BANDS) bands.
    /// An index at or above that count is rejected with
    /// [`ProcessError::InvalidParameter`] rather than clamped, because clamping
    /// would edit a different band than the caller asked for.
    pub fn set_eq_band_gain_db(&self, band: usize, gain_db: f64) -> Result<(), ProcessError> {
        validate_eq_band_index("PlaybackParameters", band)?;
        self.eq.set_band_gain(
            band,
            checked_parameter("PlaybackParameters", "eq band gain", gain_db)?,
        );
        Ok(())
    }
    /// Publish enablement and all band gains (dB) as one coherent callback
    /// snapshot. Prefer this over per-band writes when applying a preset.
    ///
    /// Gains are clamped like [`Self::set_eq_band_gain_db`], so
    /// [`Self::eq_band_gains_db`] reports the applied values.
    pub fn set_eq(
        &self,
        enabled: bool,
        band_gains_db: [f64; crate::processor::EQ_BANDS],
    ) -> Result<(), ProcessError> {
        for gain in band_gains_db {
            checked_parameter("PlaybackParameters", "eq band gain", gain)?;
        }
        self.eq.write(&band_gains_db, enabled);
        Ok(())
    }
    /// Enable or bypass the limiter.
    pub fn set_limiter_enabled(&self, enabled: bool) {
        self.limiter.set_enabled(enabled);
    }
    /// Publish limiter threshold in dB, clamped to
    /// [`LIMITER_THRESHOLD_DB_MIN`]..=[`LIMITER_THRESHOLD_DB_MAX`].
    pub fn set_limiter_threshold_db(&self, threshold_db: f64) -> Result<(), ProcessError> {
        self.limiter.set_threshold(checked_parameter(
            "PlaybackParameters",
            "limiter threshold",
            threshold_db,
        )?);
        Ok(())
    }
    /// Whether saturation was armed at build time and therefore accepts
    /// runtime changes.
    pub const fn saturation_armed(&self) -> bool {
        self.saturation_armed
    }
    /// Soft-bypass or re-engage an armed saturation stage.
    ///
    /// Arming is a build-time decision because it fixes the stage's latency;
    /// this toggle preserves that latency and the stage's history. A pipeline
    /// built with a disabled [`PlaybackSaturationConfig`] returns a typed
    /// error instead.
    pub fn set_saturation_enabled(&self, enabled: bool) -> Result<(), ProcessError> {
        self.require_armed_saturation("change saturation enablement")?;
        self.saturation.set_enabled(enabled);
        Ok(())
    }
    /// Publish saturation drive, clamped to
    /// [`SATURATION_DRIVE_MIN`]..=[`SATURATION_DRIVE_MAX`].
    pub fn set_saturation_drive(&self, drive: f64) -> Result<(), ProcessError> {
        self.require_armed_saturation("change saturation drive")?;
        self.saturation.set_drive(checked_parameter(
            "PlaybackParameters",
            "saturation drive",
            drive,
        )?);
        Ok(())
    }
    /// Publish the saturation onset threshold (linear).
    pub fn set_saturation_threshold(&self, threshold: f64) -> Result<(), ProcessError> {
        self.require_armed_saturation("change saturation threshold")?;
        self.saturation.set_threshold(checked_parameter(
            "PlaybackParameters",
            "saturation threshold",
            threshold,
        )?);
        Ok(())
    }
    /// Publish the saturation dry/wet mix (linear).
    pub fn set_saturation_mix(&self, mix: f64) -> Result<(), ProcessError> {
        self.require_armed_saturation("change saturation mix")?;
        self.saturation.set_mix(checked_parameter(
            "PlaybackParameters",
            "saturation mix",
            mix,
        )?);
        Ok(())
    }
    /// Publish the saturation character.
    pub fn set_saturation_type(&self, saturation_type: SaturationType) -> Result<(), ProcessError> {
        self.require_armed_saturation("change saturation type")?;
        self.saturation.set_sat_type(saturation_type.into());
        Ok(())
    }
    /// Publish saturation input/output makeup gains in dB.
    ///
    /// Both gains reach the callback as one coherent snapshot, so a block never
    /// runs with the new input gain against the old output gain.
    pub fn set_saturation_gains_db(
        &self,
        input_gain_db: f64,
        output_gain_db: f64,
    ) -> Result<(), ProcessError> {
        self.require_armed_saturation("change saturation gains")?;
        let input =
            checked_parameter("PlaybackParameters", "saturation input gain", input_gain_db)?;
        let output = checked_parameter(
            "PlaybackParameters",
            "saturation output gain",
            output_gain_db,
        )?;
        self.saturation.set_gains_db(input, output);
        Ok(())
    }
    fn require_armed_saturation(&self, operation: &'static str) -> Result<(), ProcessError> {
        if self.saturation_armed {
            return Ok(());
        }
        Err(ProcessError::UnsupportedOperation {
            processor: "PlaybackParameters",
            operation,
            message: "build with an enabled PlaybackSaturationConfig to arm the stage",
        })
    }
    /// Enable or bypass crossfeed.
    pub fn set_crossfeed_enabled(&self, enabled: bool) {
        self.crossfeed.set_enabled(enabled);
    }
    /// Publish crossfeed enablement, dry/wet mix, and low-pass cutoff as one
    /// coherent callback snapshot. Values are clamped to
    /// [`CROSSFEED_MIX_MIN`]..=[`CROSSFEED_MIX_MAX`] and
    /// [`CROSSFEED_CUTOFF_HZ_MIN`]..=[`CROSSFEED_CUTOFF_HZ_MAX`].
    pub fn set_crossfeed(
        &self,
        enabled: bool,
        mix: f64,
        cutoff_hz: f64,
    ) -> Result<(), ProcessError> {
        let mix = checked_parameter("PlaybackParameters", "crossfeed mix", mix)?;
        let cutoff_hz = checked_parameter("PlaybackParameters", "crossfeed cutoff", cutoff_hz)?;
        self.crossfeed.write(enabled, mix, cutoff_hz);
        Ok(())
    }
    /// Enable or bypass dynamic loudness.
    pub fn set_dynamic_loudness_enabled(&self, enabled: bool) {
        self.dynamic_loudness.set_enabled(enabled);
    }
    /// Publish dynamic-loudness enablement, current listening volume in dBFS,
    /// and strength as one coherent callback snapshot.
    ///
    /// The listening volume is clamped to
    /// [`DYNAMIC_LOUDNESS_VOLUME_DB_MIN`]..=[`DYNAMIC_LOUDNESS_VOLUME_DB_MAX`]
    /// so [`Self::dynamic_loudness`] round-trips it as a finite dB value.
    pub fn set_dynamic_loudness(
        &self,
        enabled: bool,
        listening_volume_db: f64,
        strength: f64,
    ) -> Result<(), ProcessError> {
        let listening_volume_db = checked_parameter(
            "PlaybackParameters",
            "dynamic loudness listening volume",
            listening_volume_db,
        )?
        .clamp(
            DYNAMIC_LOUDNESS_VOLUME_DB_MIN,
            DYNAMIC_LOUDNESS_VOLUME_DB_MAX,
        );
        let strength =
            checked_parameter("PlaybackParameters", "dynamic loudness strength", strength)?;
        self.dynamic_loudness
            .write(enabled, db_to_linear(listening_volume_db), strength);
        Ok(())
    }
    /// Enable or bypass output noise shaping.
    pub fn set_noise_shaping_enabled(&self, enabled: bool) {
        self.noise_shaper.set_enabled(enabled);
    }
    /// Publish noise-shaping enablement, target bit depth, and curve as one
    /// coherent callback snapshot. Bit depth is clamped to
    /// [`NOISE_SHAPER_BITS_MIN`]..=[`NOISE_SHAPER_BITS_MAX`].
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
    /// Current saturation soft-bypass state, drive, threshold, and mix as
    /// applied by the callback. Meaningful only when
    /// [`Self::saturation_armed`] is true.
    pub fn saturation(&self) -> (bool, f64, f64, f64) {
        let snapshot = self.saturation.read();
        (
            snapshot.enabled,
            snapshot.drive,
            snapshot.threshold,
            snapshot.mix,
        )
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
/// It owns exactly the two things that cannot be shared: the private
/// single-consumer convolver lease, and the lifecycle request channel. Ordinary
/// DSP controls are deliberately *not* mirrored here — use [`Self::parameters`]
/// for a clonable publisher, so volume, EQ, and the rest have one owner rather
/// than an arbitrary subset reachable through two handles. Neither handle is
/// callback-safe.
pub struct PlaybackController {
    parameters: PlaybackParameters,
    /// Private single-consumer convolver authority. Held so kernel adoption
    /// and drop-release lifecycles stay paired with this controller; never
    /// exposed directly through the high-level facade.
    convolver_lease: ConvolverControl,
    /// Lock-free lifecycle request channel shared with the callback-owned
    /// pipeline.
    lifecycle: Arc<LifecycleChannel>,
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
    /// Ask the audio callback to clear DSP history and re-arm for a new
    /// logical stream, and return the request generation.
    ///
    /// The request is applied at the next callback block boundary without
    /// allocation or locking; poll [`Self::lifecycle_status`] to observe it.
    /// A reset from [`PlaybackLifecycleState::Idle`] resumes ordinary
    /// processing.
    pub fn request_reset(&self) -> u64 {
        self.lifecycle.publish(REQUEST_RESET, 0)
    }
    /// Ask the audio callback to stop consuming input and render the remaining
    /// effect tail, then settle into [`PlaybackLifecycleState::Idle`].
    ///
    /// The tail is bounded by the drain policy fixed at build time
    /// ([`PlaybackConfig::with_drain_policy`]). Requesting a drain while the
    /// pipeline is already idle is accepted and changes nothing.
    pub fn request_drain(&self) -> u64 {
        self.lifecycle.publish(REQUEST_DRAIN, 0)
    }
    /// Ask the audio callback to ramp the output down over `fade_ms` and then
    /// drain, so a track change does not click.
    ///
    /// `fade_ms` must not exceed [`MAX_STOP_FADE_MS`]; a zero fade behaves
    /// like [`Self::request_drain`].
    pub fn request_stop_with_fade(&self, fade_ms: u32) -> Result<u64, ProcessError> {
        if fade_ms > MAX_STOP_FADE_MS {
            return Err(ProcessError::InvalidGeometry {
                processor: "PlaybackController",
                operation: "request stop with fade",
                message: "fade duration exceeds the maximum supported stop ramp",
            });
        }
        Ok(self
            .lifecycle
            .publish(REQUEST_STOP_WITH_FADE, fade_ms as u16))
    }
    /// Read the lifecycle state plus requested/applied request generations.
    pub fn lifecycle_status(&self) -> PlaybackLifecycleStatus {
        self.lifecycle.status()
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
    ///
    /// Configuration is validated strictly here: a non-finite or out-of-range
    /// value fails with `ProcessError::InvalidParameter` rather than being
    /// silently clamped, so a bad preset or config file surfaces at build
    /// time. Runtime [`PlaybackParameters`] writes clamp instead.
    pub fn build(self) -> Result<(PlaybackPipeline, PlaybackController), ProcessError> {
        self.config.validate()?;
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
        let lifecycle = Arc::new(LifecycleChannel::new());
        let params = OutputChainParams {
            channels: self.spec.channels,
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
        let pipeline = PlaybackPipeline::assemble(
            &OutputChainBuilder::new(params),
            self.spec,
            Arc::clone(&lifecycle),
            self.config.drain_policy,
        )?;
        Ok((
            pipeline,
            PlaybackController {
                parameters: PlaybackParameters {
                    volume,
                    eq,
                    limiter,
                    saturation,
                    crossfeed,
                    dynamic_loudness,
                    noise_shaper,
                    telemetry,
                    saturation_armed: self.config.saturation.enabled,
                },
                convolver_lease: convolver,
                lifecycle,
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
    /// Algorithmic latency in the pipeline's output sample-rate domain.
    pub latency: FrameDuration,
    /// Declared semantic tail after end of input.
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
    lifecycle: Arc<LifecycleChannel>,
    /// Newest lifecycle request generation this callback has consumed.
    seen_generation: u64,
    state: PlaybackLifecycleState,
    drain_policy: ChainFinishPolicy,
    /// Remaining and total stop-ramp length, in frames.
    fade_remaining_frames: usize,
    fade_total_frames: usize,
}
impl PlaybackPipeline {
    /// Start control-thread configuration for one callback-owned pipeline.
    pub fn builder(spec: CallbackSpec) -> PlaybackBuilder {
        PlaybackBuilder::new(spec)
    }
    /// Build from an advanced canonical-chain builder after validating this callback spec.
    ///
    /// This is a control-thread operation for integrations that intentionally
    /// use the lower-level output-chain API. The resulting pipeline has no
    /// paired [`PlaybackController`], so it never receives lifecycle requests;
    /// drive it with [`Self::finish_into_with_policy`] and [`Self::reset`].
    pub fn from_output_chain(
        builder: &OutputChainBuilder,
        spec: CallbackSpec,
    ) -> Result<Self, ProcessError> {
        Self::assemble(
            builder,
            spec,
            Arc::new(LifecycleChannel::new()),
            ChainFinishPolicy::default(),
        )
    }
    fn assemble(
        builder: &OutputChainBuilder,
        spec: CallbackSpec,
        lifecycle: Arc<LifecycleChannel>,
        drain_policy: ChainFinishPolicy,
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
            lifecycle,
            seen_generation: 0,
            state: PlaybackLifecycleState::Running,
            drain_policy,
            fade_remaining_frames: 0,
            fade_total_frames: 0,
        })
    }
    /// Return the prepared callback-domain specification.
    pub const fn spec(&self) -> CallbackSpec {
        self.spec
    }
    /// Current lifecycle position as observed by the audio callback.
    pub const fn lifecycle_state(&self) -> PlaybackLifecycleState {
        self.state
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
    ///
    /// Any lifecycle request published through the paired
    /// [`PlaybackController`] is consumed here, at the block boundary, before
    /// the block is processed. Behaviour then depends on
    /// [`Self::lifecycle_state`]:
    ///
    /// - [`PlaybackLifecycleState::Running`] filters the caller's samples.
    /// - [`PlaybackLifecycleState::FadingOut`] filters them and applies the
    ///   stop ramp.
    /// - [`PlaybackLifecycleState::Draining`] ignores the caller's samples and
    ///   overwrites the block with the remaining effect tail, silence-filling
    ///   any unwritten frames. The returned progress reports the real tail
    ///   frames produced.
    /// - [`PlaybackLifecycleState::Idle`] writes silence and succeeds, because
    ///   a device callback keeps firing after the stream ends.
    pub fn process(&mut self, samples: &mut [f64]) -> Result<ProcessProgress, ProcessError> {
        self.validate(samples, "process")?;
        self.consume_lifecycle_request()?;
        let frames = self.frames_in(samples);
        match self.state {
            PlaybackLifecycleState::Running => self.chain.process(samples, self.spec.channels),
            PlaybackLifecycleState::FadingOut => {
                let progress = self.chain.process(samples, self.spec.channels)?;
                self.apply_stop_ramp(samples, frames);
                Ok(progress)
            }
            PlaybackLifecycleState::Draining => self.drain_into(samples, frames),
            PlaybackLifecycleState::Idle => {
                samples.fill(0.0);
                Ok(ProcessProgress::new(
                    frames,
                    frames,
                    ProcessState::NeedInput,
                ))
            }
        }
    }
    const fn frames_in(&self, samples: &[f64]) -> usize {
        samples.len() / self.spec.channels
    }
    /// Consume at most one pending lifecycle request. Callback-side: one
    /// atomic load plus bounded state work; requests coalesce so only the
    /// newest one is applied.
    fn consume_lifecycle_request(&mut self) -> Result<(), ProcessError> {
        let Some((generation, kind, fade_ms)) =
            self.lifecycle.take_newer_than(self.seen_generation)
        else {
            return Ok(());
        };
        // Advance first: a request that fails to apply must not be retried on
        // every subsequent block.
        self.seen_generation = generation;
        let outcome = self.apply_lifecycle_request(kind, fade_ms);
        self.publish_lifecycle_state();
        self.lifecycle
            .applied_generation
            .store(generation, Ordering::Release);
        outcome
    }
    fn apply_lifecycle_request(&mut self, kind: u8, fade_ms: u16) -> Result<(), ProcessError> {
        match kind {
            REQUEST_RESET => {
                self.fade_remaining_frames = 0;
                self.fade_total_frames = 0;
                self.state = PlaybackLifecycleState::Running;
                self.chain.reset()
            }
            REQUEST_DRAIN => {
                if self.state != PlaybackLifecycleState::Idle {
                    self.state = PlaybackLifecycleState::Draining;
                }
                Ok(())
            }
            REQUEST_STOP_WITH_FADE => {
                if self.state == PlaybackLifecycleState::Idle {
                    return Ok(());
                }
                let ramp_frames = (fade_ms as u64 * self.spec.sample_rate_hz as u64 / 1_000)
                    .try_into()
                    .unwrap_or(usize::MAX);
                if ramp_frames == 0 {
                    self.state = PlaybackLifecycleState::Draining;
                } else {
                    self.fade_total_frames = ramp_frames;
                    self.fade_remaining_frames = ramp_frames;
                    self.state = PlaybackLifecycleState::FadingOut;
                }
                Ok(())
            }
            // `REQUEST_NONE` and reserved future kinds leave the state alone
            // rather than aborting an active stream.
            REQUEST_NONE => Ok(()),
            _ => Ok(()),
        }
    }
    fn publish_lifecycle_state(&self) {
        self.lifecycle
            .state
            .store(self.state.as_code(), Ordering::Release);
    }
    /// Apply the remaining stop ramp to an already-filtered block, then switch
    /// to draining once the ramp reaches silence.
    fn apply_stop_ramp(&mut self, samples: &mut [f64], frames: usize) {
        let total = self.fade_total_frames.max(1) as f64;
        for (index, frame) in samples.chunks_exact_mut(self.spec.channels).enumerate() {
            let remaining = self.fade_remaining_frames.saturating_sub(index + 1);
            let gain = remaining as f64 / total;
            for sample in frame {
                *sample *= gain;
            }
        }
        self.fade_remaining_frames = self.fade_remaining_frames.saturating_sub(frames);
        if self.fade_remaining_frames == 0 {
            self.state = PlaybackLifecycleState::Draining;
            self.publish_lifecycle_state();
        }
    }
    /// Render the bounded tail into the callback block and settle into idle
    /// once the chain reports terminal.
    fn drain_into(
        &mut self,
        samples: &mut [f64],
        frames: usize,
    ) -> Result<ProcessProgress, ProcessError> {
        if frames == 0 {
            return Ok(ProcessProgress::new(0, 0, ProcessState::NeedOutput));
        }
        let block = AudioBlockMut::new(samples, self.spec.channels)?;
        let progress = self.chain.finish_with_policy(block, self.drain_policy)?;
        let produced = progress.produced_frames().min(frames);
        samples[produced * self.spec.channels..].fill(0.0);
        if progress.state() == ProcessState::Finished {
            self.state = PlaybackLifecycleState::Idle;
            self.publish_lifecycle_state();
        }
        Ok(progress)
    }
    /// Advance explicit end-of-stream draining into caller-owned storage.
    ///
    /// This is the offline/non-callback drain path: it does not enter the
    /// callback-side [`PlaybackLifecycleState::Idle`] state, so once it
    /// reports `ProcessState::Finished` a further [`Self::process`] returns
    /// `ProcessError::AlreadyFinished` until [`Self::reset`]. Use
    /// [`PlaybackController::request_drain`] instead when the pipeline already
    /// lives in an audio callback.
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
    /// Reset is a lifecycle/control-thread operation. When the pipeline
    /// already lives in an audio callback, publish
    /// [`PlaybackController::request_reset`] instead — it applies the same
    /// transition at the next block boundary.
    pub fn reset(&mut self) -> Result<(), ProcessError> {
        self.fade_remaining_frames = 0;
        self.fade_total_frames = 0;
        self.state = PlaybackLifecycleState::Running;
        self.publish_lifecycle_state();
        self.chain.reset()
    }
}
/// Simple ring buffer for audio data.
///
/// Uses monotonic counters (frames_written, frames_consumed) for clean overflow
/// handling.
///
/// # Support status
///
/// Supported, but not used by any other type in this crate. It exists for a
/// consuming application that needs a decode-side producer/consumer conduit;
/// the callback path built by [`PlaybackPipeline`] does not use it and needs no
/// intermediate buffer. It is not a realtime-safe allocator boundary either:
/// [`Self::new`] allocates, so construct it during setup.
///
/// [`Self::write`] is deliberately silent. An overflow is reported through the
/// returned consumed position and [`Self::overflow_count`], because a caller may
/// legitimately drive this from an audio callback, where logging is forbidden
/// (see the realtime-safety spec).
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
    /// Create a ring buffer with fixed interleaved `f64` storage.
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
    ///
    /// Never logs: see the type-level support note. Poll
    /// [`Self::overflow_count`] from the control thread instead.
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
        parameters.set_eq_band_gain_db(0, 2.0).unwrap();
        parameters.set_limiter_threshold_db(-1.0).unwrap();
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
        // The stage was armed at build time, so runtime saturation control is
        // real and preserves the stage's fixed latency.
        assert!(parameters.saturation_armed());
        parameters.set_saturation_drive(1.0).unwrap();
        parameters.set_saturation_mix(0.25).unwrap();
        remote.set_saturation_enabled(false).unwrap();
        let (enabled, drive, _threshold, mix) = parameters.saturation();
        assert!(!enabled);
        assert!((drive - 1.0).abs() < 1e-12);
        assert!((mix - 0.25).abs() < 1e-12);
        parameters.set_crossfeed(true, 0.25, 900.0).unwrap();
        remote.set_dynamic_loudness(true, -20.0, 0.5).unwrap();
        remote.set_noise_shaping(true, 20, NoiseShaperCurve::Lipshitz5);
        assert!(pipeline.process(&mut [0.1, -0.1]).is_ok());
    }

    #[test]
    fn saturation_runtime_control_requires_an_armed_build() {
        let (_pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        let parameters = controller.parameters();
        assert!(!parameters.saturation_armed());
        assert!(matches!(
            parameters.set_saturation_drive(1.0),
            Err(ProcessError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            parameters.set_saturation_enabled(true),
            Err(ProcessError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            parameters.set_saturation_mix(0.5),
            Err(ProcessError::UnsupportedOperation { .. })
        ));
    }

    #[test]
    fn armed_saturation_soft_bypass_preserves_stage_latency() {
        let config = PlaybackConfig::transparent()
            .with_saturation(PlaybackSaturationConfig::enabled().with_mix(0.0));
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec())
            .configure(config)
            .build()
            .unwrap();
        let armed_latency = pipeline.timing().latency;
        let parameters = controller.parameters();

        parameters.set_saturation_enabled(false).unwrap();
        let mut samples = vec![0.25; MAX * 2];
        let _ = pipeline.process(&mut samples).unwrap();
        assert_eq!(pipeline.timing().latency, armed_latency);

        parameters.set_saturation_enabled(true).unwrap();
        let _ = pipeline.process(&mut samples).unwrap();
        assert_eq!(pipeline.timing().latency, armed_latency);
    }

    #[test]
    fn cloned_parameter_publishers_can_update_while_audio_processes() {
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        let first = controller.parameters();
        let second = first.clone();
        let worker = std::thread::spawn(move || {
            for index in 0..256 {
                second.set_volume((index % 101) as f64 / 100.0).unwrap();
            }
        });
        for _ in 0..256 {
            let _ = pipeline.process(&mut [0.25, -0.25]).unwrap();
        }
        worker.join().unwrap();
        first.set_volume(0.5).unwrap();
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
        parameters.set_eq(true, gains).unwrap();
        assert!(parameters.eq_enabled());
        assert_eq!(parameters.eq_band_gains_db(), gains);

        parameters.set_limiter_enabled(true);
        parameters.set_limiter_threshold_db(-2.0).unwrap();
        assert!(parameters.limiter_enabled());
        assert!((parameters.limiter_threshold_db() + 2.0).abs() < 1e-12);

        parameters.set_crossfeed(true, 0.3, 650.0).unwrap();
        assert_eq!(parameters.crossfeed(), (true, 0.3, 650.0));

        parameters.set_dynamic_loudness(true, -14.0, 0.6).unwrap();
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

    /// Build an all-stage pipeline with a convolution tail, matching the
    /// heaviest realistic callback configuration.
    fn loaded_pipeline(ir_frames: usize) -> (PlaybackPipeline, PlaybackController) {
        let config = PlaybackConfig::transparent()
            .with_volume(0.8)
            .with_eq([1.0; crate::processor::EQ_BANDS])
            .with_limiter(true, -1.0)
            .with_saturation(
                PlaybackSaturationConfig::enabled().with_quality(SaturationQuality::Oversampled4x),
            )
            .with_crossfeed(PlaybackCrossfeedConfig::enabled(0.4, 800.0))
            .with_dynamic_loudness(PlaybackDynamicLoudnessConfig::enabled(-18.0, 0.75))
            .with_noise_shaping(PlaybackNoiseShapingConfig::enabled(
                16,
                NoiseShaperCurve::Lipshitz5,
            ));
        let (pipeline, controller) = PlaybackPipeline::builder(spec())
            .configure(config)
            .build()
            .unwrap();
        let mut ir = vec![0.0; ir_frames * 2];
        ir[0] = 1.0;
        ir[1] = 1.0;
        if ir_frames > 1 {
            let last = ir.len();
            ir[last - 2] = 0.5;
            ir[last - 1] = 0.5;
        }
        controller.load_impulse_response(&ir).unwrap();
        (pipeline, controller)
    }

    #[test]
    fn lifecycle_reset_request_is_applied_at_the_next_block_boundary() {
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        let mut samples = [0.25, -0.25];
        let _ = pipeline.process(&mut samples).unwrap();

        let generation = controller.request_reset();
        let before = controller.lifecycle_status();
        assert_eq!(before.requested_generation, generation);
        assert!(!before.is_settled());

        let _ = pipeline.process(&mut samples).unwrap();
        let after = controller.lifecycle_status();
        assert!(after.is_settled());
        assert_eq!(after.applied_generation, generation);
        assert_eq!(after.state, PlaybackLifecycleState::Running);
        assert_eq!(pipeline.lifecycle_state(), PlaybackLifecycleState::Running);
    }

    #[test]
    fn lifecycle_drain_renders_the_tail_then_idles_without_erroring() {
        let (mut pipeline, controller) = loaded_pipeline(256);
        let mut samples = vec![0.5; MAX * 2];
        // Adopt the kernel and build up state before stopping.
        for _ in 0..4 {
            let _ = pipeline.process(&mut samples).unwrap();
        }

        controller.request_drain();
        let mut blocks = 0;
        let mut tail_frames = 0;
        while pipeline.lifecycle_state() != PlaybackLifecycleState::Idle {
            let mut block = vec![0.5; MAX * 2];
            let progress = pipeline.process(&mut block).unwrap();
            tail_frames += progress.produced_frames();
            blocks += 1;
            assert!(blocks < 10_000, "drain did not terminate");
        }
        assert!(
            tail_frames > 0,
            "a convolution tail should have been drained"
        );
        assert_eq!(
            controller.lifecycle_status().state,
            PlaybackLifecycleState::Idle
        );

        // A device callback keeps firing after the stream ends: it must get
        // silence and success, not `AlreadyFinished`.
        let mut idle = vec![0.5; MAX * 2];
        let progress = pipeline.process(&mut idle).unwrap();
        assert_eq!(progress.produced_frames(), MAX);
        assert!(idle.iter().all(|sample| *sample == 0.0));

        // Reset re-arms the same pipeline for a new logical stream.
        controller.request_reset();
        let mut resumed = vec![0.5; MAX * 2];
        let _ = pipeline.process(&mut resumed).unwrap();
        assert_eq!(pipeline.lifecycle_state(), PlaybackLifecycleState::Running);
        assert!(resumed.iter().any(|sample| *sample != 0.0));
    }

    #[test]
    fn stop_with_fade_ramps_monotonically_to_silence_before_draining() {
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        let mut warmup = vec![1.0; MAX * 2];
        let _ = pipeline.process(&mut warmup).unwrap();

        // 2 ms at 48 kHz is 96 frames: one full block plus a partial one.
        controller.request_stop_with_fade(2).unwrap();
        let mut rendered = Vec::new();
        for _ in 0..2 {
            let mut block = vec![1.0; MAX * 2];
            let _ = pipeline.process(&mut block).unwrap();
            rendered.extend_from_slice(&block);
        }
        assert_eq!(pipeline.lifecycle_state(), PlaybackLifecycleState::Draining);

        assert!(rendered[0] < 1.0 && rendered[0] > 0.9);
        for pair in rendered.chunks_exact(2).collect::<Vec<_>>().windows(2) {
            assert!(
                pair[1][0] <= pair[0][0],
                "stop ramp must be monotonically non-increasing"
            );
        }
        assert_eq!(rendered.last().copied(), Some(0.0));
    }

    #[test]
    fn stop_with_fade_rejects_an_over_long_ramp() {
        let (_pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        assert!(matches!(
            controller.request_stop_with_fade(MAX_STOP_FADE_MS + 1),
            Err(ProcessError::InvalidGeometry { .. })
        ));
        assert!(controller.request_stop_with_fade(MAX_STOP_FADE_MS).is_ok());
    }

    #[test]
    fn lifecycle_requests_coalesce_so_the_newest_one_wins() {
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        let mut samples = vec![0.5; MAX * 2];
        let _ = pipeline.process(&mut samples).unwrap();

        // Three requests between two callback blocks: only the last applies.
        controller.request_drain();
        controller.request_stop_with_fade(50).unwrap();
        let newest = controller.request_reset();

        let _ = pipeline.process(&mut samples).unwrap();
        assert_eq!(pipeline.lifecycle_state(), PlaybackLifecycleState::Running);
        let status = controller.lifecycle_status();
        assert_eq!(status.applied_generation, newest);
        assert!(status.is_settled());
    }

    #[test]
    fn lifecycle_request_published_before_the_first_block_is_applied() {
        let (mut pipeline, controller) = loaded_pipeline(8);
        let generation = controller.request_drain();
        assert!(!controller.lifecycle_status().is_settled());

        let mut samples = vec![0.5; MAX * 2];
        let _ = pipeline.process(&mut samples).unwrap();
        assert_eq!(controller.lifecycle_status().applied_generation, generation);
        assert!(matches!(
            pipeline.lifecycle_state(),
            PlaybackLifecycleState::Draining | PlaybackLifecycleState::Idle
        ));
    }

    #[test]
    fn lifecycle_requests_after_a_terminal_drain_are_accepted() {
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        let mut samples = vec![0.5; MAX * 2];
        let _ = pipeline.process(&mut samples).unwrap();

        controller.request_drain();
        let mut blocks = 0;
        while pipeline.lifecycle_state() != PlaybackLifecycleState::Idle {
            let mut block = vec![0.0; MAX * 2];
            let _ = pipeline.process(&mut block).unwrap();
            blocks += 1;
            assert!(blocks < 10_000, "drain did not terminate");
        }

        // Draining or fading from idle is accepted and settles without
        // leaving the terminal state.
        let drain_again = controller.request_drain();
        let mut block = vec![0.5; MAX * 2];
        let _ = pipeline.process(&mut block).unwrap();
        assert_eq!(pipeline.lifecycle_state(), PlaybackLifecycleState::Idle);
        assert_eq!(
            controller.lifecycle_status().applied_generation,
            drain_again
        );

        let fade_from_idle = controller.request_stop_with_fade(20).unwrap();
        let _ = pipeline.process(&mut block).unwrap();
        assert_eq!(pipeline.lifecycle_state(), PlaybackLifecycleState::Idle);
        assert_eq!(
            controller.lifecycle_status().applied_generation,
            fade_from_idle
        );

        controller.request_reset();
        let mut resumed = vec![0.5; MAX * 2];
        let _ = pipeline.process(&mut resumed).unwrap();
        assert_eq!(pipeline.lifecycle_state(), PlaybackLifecycleState::Running);
    }

    #[test]
    fn pipeline_keeps_processing_after_its_controller_is_dropped() {
        let (mut pipeline, controller) = loaded_pipeline(64);
        let mut samples = vec![0.25; MAX * 2];
        let _ = pipeline.process(&mut samples).unwrap();
        drop(controller);

        // No further requests can arrive, but the callback must stay healthy.
        for _ in 0..8 {
            let _ = pipeline.process(&mut samples).unwrap();
        }
        assert_eq!(pipeline.lifecycle_state(), PlaybackLifecycleState::Running);
    }

    #[test]
    fn lifecycle_handling_and_full_capacity_processing_are_allocation_free() {
        std::thread::spawn(|| {
            let (mut pipeline, controller) = loaded_pipeline(8_192);
            let mut samples = vec![0.1_f64; MAX * 2];
            assert_no_alloc::assert_no_alloc(|| {
                // Full-capacity block with every stage armed, including the
                // first kernel adoption.
                let _ = pipeline.process(&mut samples).unwrap();
                controller.request_stop_with_fade(5).unwrap();
                let _ = pipeline.process(&mut samples).unwrap();
                controller.request_drain();
                let _ = pipeline.process(&mut samples).unwrap();
                controller.request_reset();
                let _ = pipeline.process(&mut samples).unwrap();
            });
            // A superseding kernel is adopted on the audio thread too.
            let mut ir = vec![0.0; 16_384 * 2];
            ir[0] = 1.0;
            ir[1] = 1.0;
            controller.load_impulse_response(&ir).unwrap();
            assert_no_alloc::assert_no_alloc(|| {
                let _ = pipeline.process(&mut samples).unwrap();
            });
        })
        .join()
        .unwrap();
    }

    #[test]
    fn non_finite_parameter_writes_are_rejected_and_cannot_poison_dsp_state() {
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec())
            .configure(PlaybackConfig::transparent().with_eq([0.0; crate::processor::EQ_BANDS]))
            .build()
            .unwrap();
        let parameters = controller.parameters();
        parameters.set_eq_enabled(true);

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                parameters.set_volume(bad),
                Err(ProcessError::InvalidParameter { .. })
            ));
            assert!(matches!(
                parameters.set_eq_band_gain_db(0, bad),
                Err(ProcessError::InvalidParameter { .. })
            ));
            assert!(matches!(
                parameters.set_eq(true, [bad; crate::processor::EQ_BANDS]),
                Err(ProcessError::InvalidParameter { .. })
            ));
            assert!(matches!(
                parameters.set_limiter_threshold_db(bad),
                Err(ProcessError::InvalidParameter { .. })
            ));
            assert!(matches!(
                parameters.set_crossfeed(true, bad, 800.0),
                Err(ProcessError::InvalidParameter { .. })
            ));
            assert!(matches!(
                parameters.set_dynamic_loudness(true, bad, 0.5),
                Err(ProcessError::InvalidParameter { .. })
            ));
        }

        // The previous snapshot survives, so audio stays finite.
        assert!(parameters.volume().is_finite());
        let mut samples = vec![0.5; MAX * 2];
        let _ = pipeline.process(&mut samples).unwrap();
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn non_finite_writes_are_also_refused_by_the_low_level_parameter_types() {
        let volume = AtomicVolumeParams::new();
        volume.set_volume(0.5);
        volume.set_volume(f64::NAN);
        assert!((volume.read().volume - 0.5).abs() < 1e-12);

        let eq = AtomicEqParams::new();
        eq.write(&[3.0; crate::processor::EQ_BANDS], true);
        eq.write(&[f64::NAN; crate::processor::EQ_BANDS], true);
        eq.set_band_gain(0, f64::INFINITY);
        assert!(eq
            .read()
            .gains
            .iter()
            .all(|gain| (*gain - 3.0).abs() < 1e-12));

        let crossfeed = AtomicCrossfeedParams::new();
        crossfeed.write(true, 0.4, 800.0);
        crossfeed.write(true, f64::NAN, 800.0);
        assert!((crossfeed.read().mix - 0.4).abs() < 1e-12);
    }

    #[test]
    fn low_level_eq_setter_no_ops_for_an_invalid_band_index() {
        let eq = AtomicEqParams::new();
        eq.write(&[3.0; crate::processor::EQ_BANDS], true);

        eq.set_band_gain(crate::processor::EQ_BANDS, 12.0);
        eq.set_band_gain(usize::MAX, 12.0);

        // The atomic family stays infallible: the previous snapshot survives and
        // the facade is the layer that reports the rejection.
        assert!(eq
            .read()
            .gains
            .iter()
            .all(|gain| (*gain - 3.0).abs() < 1e-12));
    }

    #[test]
    fn out_of_range_eq_band_index_is_rejected_instead_of_acknowledged() {
        let (_pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        let parameters = controller.parameters();

        parameters.set_eq_band_gain_db(0, 6.0).unwrap();
        let applied = parameters.eq_band_gains_db();

        for band in [
            crate::processor::EQ_BANDS,
            crate::processor::EQ_BANDS + 1,
            usize::MAX,
        ] {
            assert_eq!(
                parameters.set_eq_band_gain_db(band, 3.0).unwrap_err(),
                ProcessError::InvalidParameter {
                    processor: "PlaybackParameters",
                    parameter: "eq band index",
                    message: "band index must be below EQ_BANDS",
                }
            );
        }

        // A rejected index publishes nothing, so the reader still reports the
        // last edit that actually reached the callback.
        assert_eq!(parameters.eq_band_gains_db(), applied);

        // The highest valid band is still addressable.
        parameters
            .set_eq_band_gain_db(crate::processor::EQ_BANDS - 1, -3.0)
            .unwrap();
        assert!(
            (parameters.eq_band_gains_db()[crate::processor::EQ_BANDS - 1] + 3.0).abs() < 1e-12
        );
    }

    /// One control operation must become exactly one callback-visible snapshot.
    fn assert_single_publication(label: &str, generation: impl Fn() -> u64, action: impl FnOnce()) {
        let before = generation();
        action();
        assert_eq!(
            generation(),
            before + 1,
            "{label} must publish exactly one coherent snapshot"
        );
    }

    fn armed_saturation_parameters() -> (PlaybackController, PlaybackParameters) {
        let (_pipeline, controller) = PlaybackPipeline::builder(spec())
            .configure(
                PlaybackConfig::transparent().with_saturation(PlaybackSaturationConfig::enabled()),
            )
            .build()
            .unwrap();
        let parameters = controller.parameters();
        (controller, parameters)
    }

    #[test]
    fn every_facade_setter_publishes_exactly_one_coherent_snapshot() {
        let (_controller, parameters) = armed_saturation_parameters();

        let volume = || parameters.volume.load_with_generation().1;
        let eq = || parameters.eq.load_with_generation().1;
        let limiter = || parameters.limiter.load_with_generation().1;
        let saturation = || parameters.saturation.load_with_generation().1;
        let crossfeed = || parameters.crossfeed.load_with_generation().1;
        let dynamic_loudness = || parameters.dynamic_loudness.load_with_generation().1;
        let noise_shaper = || parameters.noise_shaper.load_with_generation().1;

        assert_single_publication("set_volume", volume, || parameters.set_volume(0.5).unwrap());
        assert_single_publication("set_muted", volume, || parameters.set_muted(true));
        assert_single_publication("set_eq_enabled", eq, || parameters.set_eq_enabled(true));
        assert_single_publication("set_eq_band_gain_db", eq, || {
            parameters.set_eq_band_gain_db(0, 3.0).unwrap()
        });
        assert_single_publication("set_eq", eq, || {
            parameters
                .set_eq(true, [1.0; crate::processor::EQ_BANDS])
                .unwrap()
        });
        assert_single_publication("set_limiter_enabled", limiter, || {
            parameters.set_limiter_enabled(true)
        });
        assert_single_publication("set_limiter_threshold_db", limiter, || {
            parameters.set_limiter_threshold_db(-2.0).unwrap()
        });
        assert_single_publication("set_saturation_enabled", saturation, || {
            parameters.set_saturation_enabled(true).unwrap()
        });
        assert_single_publication("set_saturation_drive", saturation, || {
            parameters.set_saturation_drive(0.7).unwrap()
        });
        assert_single_publication("set_saturation_threshold", saturation, || {
            parameters.set_saturation_threshold(0.6).unwrap()
        });
        assert_single_publication("set_saturation_mix", saturation, || {
            parameters.set_saturation_mix(0.4).unwrap()
        });
        assert_single_publication("set_saturation_type", saturation, || {
            parameters
                .set_saturation_type(SaturationType::Tube)
                .unwrap()
        });
        assert_single_publication("set_saturation_gains_db", saturation, || {
            parameters.set_saturation_gains_db(-4.0, 4.0).unwrap()
        });
        assert_single_publication("set_crossfeed_enabled", crossfeed, || {
            parameters.set_crossfeed_enabled(true)
        });
        assert_single_publication("set_crossfeed", crossfeed, || {
            parameters.set_crossfeed(true, 0.3, 750.0).unwrap()
        });
        assert_single_publication("set_dynamic_loudness_enabled", dynamic_loudness, || {
            parameters.set_dynamic_loudness_enabled(true)
        });
        assert_single_publication("set_dynamic_loudness", dynamic_loudness, || {
            parameters.set_dynamic_loudness(true, -18.0, 0.5).unwrap()
        });
        assert_single_publication("set_noise_shaping_enabled", noise_shaper, || {
            parameters.set_noise_shaping_enabled(true)
        });
        assert_single_publication("set_noise_shaping", noise_shaper, || {
            parameters.set_noise_shaping(true, 20, NoiseShaperCurve::Lipshitz5)
        });
    }

    #[test]
    fn paired_saturation_gains_reach_a_realtime_reader_as_one_snapshot() {
        let (_controller, parameters) = armed_saturation_parameters();
        parameters.set_saturation_gains_db(0.0, 0.0).unwrap();

        let (reader, initial, before) = parameters.saturation.subscribe_realtime();
        assert!(initial.input_gain_db.abs() < 1e-12);
        assert!(initial.output_gain_db.abs() < 1e-12);

        parameters.set_saturation_gains_db(-6.0, 6.0).unwrap();

        let (snapshot, after) = parameters
            .saturation
            .load_realtime_if_changed_since(&reader, before)
            .expect("the paired write must reach the callback");

        // The decisive assertion: two publications would advance the generation
        // twice and expose a new-input/old-output pair in between.
        assert_eq!(
            after,
            before + 1,
            "one paired gain update must be one publication"
        );
        assert!((snapshot.input_gain_db + 6.0).abs() < 1e-12);
        assert!((snapshot.output_gain_db - 6.0).abs() < 1e-12);
        assert!(parameters
            .saturation
            .load_realtime_if_changed_since(&reader, after)
            .is_none());
    }

    #[test]
    fn non_finite_paired_saturation_gain_publishes_nothing() {
        let (_controller, parameters) = armed_saturation_parameters();
        parameters.set_saturation_gains_db(-4.0, 4.0).unwrap();
        let (_, before) = parameters.saturation.load_with_generation();

        for (input, output) in [
            (f64::NAN, 4.0),
            (-4.0, f64::INFINITY),
            (f64::NEG_INFINITY, f64::NAN),
        ] {
            assert!(matches!(
                parameters.set_saturation_gains_db(input, output),
                Err(ProcessError::InvalidParameter { .. })
            ));
        }

        let (snapshot, after) = parameters.saturation.load_with_generation();
        assert_eq!(after, before, "a rejected pair must not publish");
        assert!((snapshot.input_gain_db + 4.0).abs() < 1e-12);
        assert!((snapshot.output_gain_db - 4.0).abs() < 1e-12);
    }

    #[test]
    fn out_of_range_runtime_writes_clamp_and_readers_report_the_applied_value() {
        let (_pipeline, controller) = PlaybackPipeline::builder(spec()).build().unwrap();
        let parameters = controller.parameters();

        parameters.set_volume(2.0).unwrap();
        assert!((parameters.volume() - VOLUME_MAX).abs() < 1e-12);

        // A preset written through `set_eq` reports the same gains the
        // equalizer actually applies.
        parameters
            .set_eq(true, [40.0; crate::processor::EQ_BANDS])
            .unwrap();
        assert!(parameters
            .eq_band_gains_db()
            .iter()
            .all(|gain| (*gain - EQ_BAND_GAIN_DB_MAX).abs() < 1e-12));
        parameters.set_eq_band_gain_db(0, 40.0).unwrap();
        assert!((parameters.eq_band_gains_db()[0] - EQ_BAND_GAIN_DB_MAX).abs() < 1e-12);

        parameters.set_limiter_threshold_db(12.0).unwrap();
        assert!((parameters.limiter_threshold_db() - LIMITER_THRESHOLD_DB_MAX).abs() < 1e-12);

        parameters.set_crossfeed(true, 5.0, 50_000.0).unwrap();
        let (_, mix, cutoff_hz) = parameters.crossfeed();
        assert!((mix - CROSSFEED_MIX_MAX).abs() < 1e-12);
        assert!((cutoff_hz - CROSSFEED_CUTOFF_HZ_MAX).abs() < 1e-12);

        // Listening volume round-trips as a finite dB value instead of -inf.
        parameters.set_dynamic_loudness(true, 6.0, 2.0).unwrap();
        let (_, listening_volume_db, strength) = parameters.dynamic_loudness();
        assert!((listening_volume_db - DYNAMIC_LOUDNESS_VOLUME_DB_MAX).abs() < 1e-9);
        assert!((strength - DYNAMIC_LOUDNESS_STRENGTH_MAX).abs() < 1e-12);
        parameters.set_dynamic_loudness(true, -400.0, 0.5).unwrap();
        let (_, floored_db, _) = parameters.dynamic_loudness();
        assert!(floored_db.is_finite());
        assert!((floored_db - DYNAMIC_LOUDNESS_VOLUME_DB_MIN).abs() < 1e-6);

        parameters.set_noise_shaping(true, 64, NoiseShaperCurve::TpdfOnly);
        assert_eq!(parameters.noise_shaping().1, NOISE_SHAPER_BITS_MAX);
    }

    #[test]
    fn build_rejects_non_finite_and_out_of_range_configuration() {
        let cases = [
            PlaybackConfig::transparent().with_volume(f64::NAN),
            PlaybackConfig::transparent().with_volume(2.0),
            PlaybackConfig::transparent().with_eq([f64::INFINITY; crate::processor::EQ_BANDS]),
            PlaybackConfig::transparent().with_eq([40.0; crate::processor::EQ_BANDS]),
            PlaybackConfig::transparent().with_limiter(true, 6.0),
            PlaybackConfig::transparent()
                .with_crossfeed(PlaybackCrossfeedConfig::enabled(2.0, 800.0)),
            PlaybackConfig::transparent()
                .with_dynamic_loudness(PlaybackDynamicLoudnessConfig::enabled(6.0, 0.5)),
            PlaybackConfig::transparent()
                .with_saturation(PlaybackSaturationConfig::enabled().with_drive(9.0)),
            PlaybackConfig::transparent().with_noise_shaping(PlaybackNoiseShapingConfig::enabled(
                4,
                NoiseShaperCurve::TpdfOnly,
            )),
        ];
        for config in cases {
            assert!(matches!(
                PlaybackPipeline::builder(spec()).configure(config).build(),
                Err(ProcessError::InvalidParameter { .. })
            ));
        }
        // The documented bounds themselves stay buildable.
        assert!(PlaybackPipeline::builder(spec())
            .configure(
                PlaybackConfig::transparent()
                    .with_volume(VOLUME_MAX)
                    .with_eq([EQ_BAND_GAIN_DB_MAX; crate::processor::EQ_BANDS])
                    .with_limiter(true, LIMITER_THRESHOLD_DB_MIN)
            )
            .build()
            .is_ok());
    }

    /// A drain policy that cannot bound a tail must fail deterministically at
    /// build time. Deferring it to the callback's first drain would surface a
    /// preset error on the realtime lifecycle path.
    #[test]
    fn build_rejects_a_drain_policy_that_cannot_bound_a_tail() {
        let cases = [
            ChainFinishPolicy::new(f64::NAN, 12_000, 1_440_000),
            // A positive threshold never falls below the silence gate.
            ChainFinishPolicy::new(6.0, 12_000, 1_440_000),
            ChainFinishPolicy::new(-120.0, 0, 1_440_000),
            // A cap below the hold can never observe a full quiet window.
            ChainFinishPolicy::new(-120.0, 12_000, 11_999),
        ];
        for policy in cases {
            assert!(
                matches!(
                    PlaybackPipeline::builder(spec())
                        .configure(PlaybackConfig::transparent().with_drain_policy(policy))
                        .build(),
                    Err(ProcessError::InvalidRenderPolicy { .. })
                ),
                "policy {policy:?} was accepted at build time"
            );
        }
        assert!(PlaybackPipeline::builder(spec())
            .configure(
                PlaybackConfig::transparent()
                    .with_drain_policy(ChainFinishPolicy::new(-90.0, 480, 480))
            )
            .build()
            .is_ok());
    }

    /// A bypassed crossfeed config must describe the same profile the core
    /// starts from, so flipping it on cannot silently change the mix.
    #[test]
    fn disabled_crossfeed_config_matches_the_core_profile() {
        let disabled = PlaybackCrossfeedConfig::disabled();
        let core = crate::processor::Crossfeed::new(48_000.0).get_settings();
        assert_eq!(disabled.mix, core.mix);
        assert_eq!(disabled.cutoff_hz, core.cutoff_hz);
    }

    /// A deterministic, non-trivial test signal.
    fn signal(frames: usize, channels: usize) -> Vec<f64> {
        (0..frames * channels)
            .map(|index| {
                let frame = (index / channels) as f64;
                let channel = (index % channels) as f64;
                0.6 * (frame * 0.07 + channel * 0.9).sin() + 0.2 * (frame * 0.31).cos()
            })
            .collect()
    }

    /// Every callback stage armed, so chunking and reset cover real state.
    ///
    /// Dynamic loudness is deliberately excluded: its smoother steps once per
    /// caller-supplied block, so its output is block-size dependent (see the
    /// chunk-equivalence test below).
    fn all_stage_config() -> PlaybackConfig {
        PlaybackConfig::transparent()
            .with_volume(0.8)
            .with_eq([2.0; crate::processor::EQ_BANDS])
            .with_limiter(true, -1.0)
            .with_saturation(
                PlaybackSaturationConfig::enabled()
                    .with_quality(SaturationQuality::Oversampled4x)
                    .with_drive(0.8)
                    .with_mix(0.6),
            )
            .with_crossfeed(PlaybackCrossfeedConfig::enabled(0.4, 800.0))
    }

    #[test]
    fn callback_output_is_identical_across_irregular_chunk_sizes() {
        const FRAMES: usize = 512;
        let spec = CallbackSpec::stereo(RATE, FRAMES).unwrap();
        let input = signal(FRAMES, 2);

        let (mut single, _controller_a) = PlaybackPipeline::builder(spec)
            .configure(all_stage_config())
            .build()
            .unwrap();
        let mut whole = input.clone();
        let _ = single.process(&mut whole).unwrap();

        let (mut chunked, _controller_b) = PlaybackPipeline::builder(spec)
            .configure(all_stage_config())
            .build()
            .unwrap();
        let mut pieces = input.clone();
        let mut offset = 0;
        for chunk_frames in [1_usize, 7, 64, 100, 128, 212] {
            let end = offset + chunk_frames * 2;
            let _ = chunked.process(&mut pieces[offset..end]).unwrap();
            offset = end;
        }
        assert_eq!(offset, FRAMES * 2, "chunk sizes must cover the whole block");

        assert_eq!(
            whole, pieces,
            "callback block size must not change signal content"
        );
    }

    #[test]
    fn reset_isolates_a_new_stream_from_previous_history() {
        let spec = CallbackSpec::stereo(RATE, MAX).unwrap();
        let probe = signal(MAX, 2);

        // A pipeline that processed a loud burst and was then reset must match
        // a freshly built one for the same new stream.
        let (mut reused, _reused_controller) = PlaybackPipeline::builder(spec)
            .configure(all_stage_config())
            .build()
            .unwrap();
        let mut burst = vec![0.95; MAX * 2];
        for _ in 0..4 {
            let _ = reused.process(&mut burst).unwrap();
        }
        reused.reset().unwrap();
        let mut after_reset = probe.clone();
        let _ = reused.process(&mut after_reset).unwrap();

        let (mut fresh, _fresh_controller) = PlaybackPipeline::builder(spec)
            .configure(all_stage_config())
            .build()
            .unwrap();
        let mut fresh_output = probe.clone();
        let _ = fresh.process(&mut fresh_output).unwrap();

        assert_eq!(
            after_reset, fresh_output,
            "reset must not leak the previous stream's history"
        );
    }

    #[test]
    fn bounded_drain_policy_reports_a_capped_unknown_tail() {
        // An enabled crossfeed declares an unknown tail, so the policy cap is
        // what terminates the drain.
        let capped_policy = ChainFinishPolicy::new(-120.0, 1, 8);
        let (mut pipeline, controller) = PlaybackPipeline::builder(spec())
            .configure(
                PlaybackConfig::transparent()
                    .with_crossfeed(PlaybackCrossfeedConfig::enabled(0.5, 700.0))
                    .with_drain_policy(capped_policy),
            )
            .build()
            .unwrap();
        let mut samples = vec![0.9; MAX * 2];
        let _ = pipeline.process(&mut samples).unwrap();
        assert!(!pipeline.finish_was_capped());

        controller.request_drain();
        let mut blocks = 0;
        while pipeline.lifecycle_state() != PlaybackLifecycleState::Idle {
            let mut block = vec![0.0; MAX * 2];
            let _ = pipeline.process(&mut block).unwrap();
            blocks += 1;
            assert!(blocks < 10_000, "capped drain did not terminate");
        }
        assert!(
            pipeline.finish_was_capped(),
            "an unknown tail cut off by the policy must report as capped"
        );

        // A generous policy on a fresh stream is not reported as capped.
        let (mut generous, generous_controller) = PlaybackPipeline::builder(spec())
            .configure(PlaybackConfig::transparent())
            .build()
            .unwrap();
        let mut block = vec![0.25; MAX * 2];
        let _ = generous.process(&mut block).unwrap();
        generous_controller.request_drain();
        let mut blocks = 0;
        while generous.lifecycle_state() != PlaybackLifecycleState::Idle {
            let mut tail = vec![0.0; MAX * 2];
            let _ = generous.process(&mut tail).unwrap();
            blocks += 1;
            assert!(blocks < 10_000, "drain did not terminate");
        }
        assert!(!generous.finish_was_capped());
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
