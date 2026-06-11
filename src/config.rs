use serde::{Deserialize, Serialize};

pub use crate::processor::SaturationType;

/// Resampling quality preset, trading CPU cost for stopband attenuation and
/// transition-band sharpness.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum ResampleQuality {
    /// Fastest, lowest fidelity.
    Low,
    /// Balanced quality suitable for general playback.
    Standard,
    /// High quality; the default.
    #[default]
    High,
    /// Maximum quality (SoX VHQ), highest CPU cost.
    UltraHigh,
}

/// Phase response for the resampling filter.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum PhaseResponse {
    /// Linear phase: symmetric impulse, no phase distortion, higher latency.
    #[default]
    Linear,
    /// Minimum phase: lowest latency, some phase distortion.
    Minimum,
    /// Maximum phase: energy concentrated toward the end of the impulse.
    Maximum,
}

impl PhaseResponse {
    /// Convert to soxr phase_response value.
    pub fn to_soxr_value(&self) -> f64 {
        match self {
            PhaseResponse::Minimum => 0.0,
            PhaseResponse::Linear => 50.0,
            PhaseResponse::Maximum => 100.0,
        }
    }
}

/// Loudness normalization reference mode: which measured gain to apply.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum NormalizationMode {
    /// Normalize each track to its own integrated loudness.
    #[default]
    Track,
    /// Normalize using album-wide integrated loudness (preserves intra-album dynamics).
    Album,
    /// Normalize toward a streaming-style target.
    Streaming,
    /// Apply ReplayGain track gain.
    ReplayGainTrack,
    /// Apply ReplayGain album gain.
    ReplayGainAlbum,
}

/// EBU R128 loudness normalization settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoudnessConfig {
    /// Target integrated loudness in LUFS.
    pub target_lufs: f64,
    /// True-peak ceiling in dBTP applied after gain.
    pub true_peak_limit_db: f64,
    /// Gain-change smoothing time constant in milliseconds.
    pub smoothing_time_ms: f64,
    /// Which reference gain to apply (see [`NormalizationMode`]).
    pub mode: NormalizationMode,
    /// Whether normalization is active.
    pub enabled: bool,
    /// Reference loudness for ReplayGain conversion, in LUFS.
    pub replaygain_reference_lufs: f64,
}

impl Default for LoudnessConfig {
    fn default() -> Self {
        Self {
            target_lufs: -12.0,
            true_peak_limit_db: -0.5,
            smoothing_time_ms: 200.0,
            mode: NormalizationMode::Track,
            enabled: true,
            replaygain_reference_lufs: -18.0,
        }
    }
}

/// Harmonic saturation settings (tube/tape/transistor coloration).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaturationConfig {
    /// Saturation character.
    pub sat_type: SaturationType,
    /// Drive amount (0.0–2.0); higher adds more harmonics.
    pub drive: f64,
    /// Linear threshold above which saturation engages.
    pub threshold: f64,
    /// Dry/wet blend (0.0 = dry, 1.0 = fully saturated).
    pub mix: f64,
    /// Input gain in dB applied before saturation.
    pub input_gain_db: f64,
    /// Output gain in dB applied after saturation, for level compensation.
    pub output_gain_db: f64,
    /// Whether saturation is active.
    pub enabled: bool,
}

impl Default for SaturationConfig {
    fn default() -> Self {
        Self {
            sat_type: SaturationType::Tube,
            drive: 0.25,
            threshold: 0.88,
            mix: 0.2,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            enabled: true,
        }
    }
}

/// Dynamic loudness compensation settings (ISO 226 / Fletcher-Munson).
///
/// Boosts perceptually weak frequency bands at low listening levels so the
/// tonal balance stays consistent as volume drops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicLoudnessConfig {
    /// Reference listening level in dB at which no compensation is applied.
    pub ref_volume_db: f64,
    /// Width in dB of the transition region around the reference level.
    pub transition_db: f64,
    /// Compensation strength multiplier (1.0 = full ISO 226 curve).
    pub strength: f64,
    /// Pre-gain in dB applied before compensation.
    pub pre_gain_db: f64,
    /// Whether dynamic loudness compensation is active.
    pub enabled: bool,
}

impl Default for DynamicLoudnessConfig {
    fn default() -> Self {
        Self {
            ref_volume_db: -15.0,
            transition_db: 25.0,
            strength: 1.0,
            pre_gain_db: -3.0,
            enabled: false,
        }
    }
}

/// Bauer-style binaural crossfeed settings for headphone listening.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossfeedConfig {
    /// Whether crossfeed is active.
    pub enabled: bool,
    /// Blend amount from 0.0 (off) to 1.0 (maximum crossfeed).
    pub mix: f64,
}

impl Default for CrossfeedConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mix: 0.3,
        }
    }
}

/// Dither and noise-shaping settings applied at the final bit-depth reduction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DitherConfig {
    /// Whether dithering is active.
    pub enabled: bool,
    /// Noise-shaping curve used to push quantization noise out of audible bands.
    pub noise_shaper_curve: crate::processor::NoiseShaperCurve,
}

impl Default for DitherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            noise_shaper_curve: crate::processor::NoiseShaperCurve::Lipshitz5,
        }
    }
}
