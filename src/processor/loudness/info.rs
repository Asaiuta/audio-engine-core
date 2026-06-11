//! Public DTO describing the current loudness measurement / gain state.

/// Loudness measurement information for API responses
#[derive(Debug, Clone, serde::Serialize)]
pub struct LoudnessInfo {
    pub integrated_lufs: f64,
    pub short_term_lufs: f64,
    pub momentary_lufs: f64,
    pub loudness_range: f64,
    pub true_peak_dbtp: f64,
    pub current_gain_db: f64,
    pub target_gain_db: f64,
    pub preamp_db: f64,
}
