//! Public DTO describing the current loudness measurement / gain state.
//!
//! Owned here (the loudness-normalizer module) because the
//! `current_gain_db` / `target_gain_db` / `preamp_db` fields describe the
//! normalizer's control state, not a pure analysis measurement. The
//! historical `crate::analysis::LoudnessInfo` path remains as a documented
//! compatibility re-export; the serialized shape is unchanged.

/// Loudness measurement information for API responses
#[derive(Debug, Clone, serde::Serialize)]
pub struct LoudnessInfo {
    /// Integrated loudness in LUFS (BS.1770).
    pub integrated_lufs: f64,
    /// Short-term loudness in LUFS.
    pub short_term_lufs: f64,
    /// Momentary loudness in LUFS.
    pub momentary_lufs: f64,
    /// Loudness range in LU (BS.1770-4).
    pub loudness_range: f64,
    /// True-peak level in dBTP.
    pub true_peak_dbtp: f64,
    /// Current applied gain in dB.
    pub current_gain_db: f64,
    /// Target gain in dB the normalizer is converging to.
    pub target_gain_db: f64,
    /// Fixed preamp gain in dB applied before measurement.
    pub preamp_db: f64,
}
