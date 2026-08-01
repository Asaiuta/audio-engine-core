//! Offline configuration records for the engine's non-callback entry points.
//!
//! Only settings with a real engine consumer live here. Callback-side effect
//! stages are configured through the `Playback*Config` records in
//! [`crate::pipeline`], which own the validated ranges the audio thread sees;
//! keeping a second copy of those knobs here is how their defaults drifted
//! apart in the first place.

use serde::{Deserialize, Serialize};

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
