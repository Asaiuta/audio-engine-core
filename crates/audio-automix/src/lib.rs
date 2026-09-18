#![deny(missing_docs)]
//! Bounded offline music analysis and AutoMix placement evidence.
//!
//! Run on a worker outside the audio callback. Analysis decodes bounded head/tail
//! windows, preserves absolute source timing, and returns evidence for the caller.
//! Scheduling, queues, persistence, and playback rendering belong to the application.
//!
//! ```no_run
//! use audio_automix::{analyze_automix, AutomixAnalysisOptions};
//! use audio_engine_core::decoder::MediaLocation;
//! # fn main() -> Result<(), audio_automix::AutomixError> {
//! let result = analyze_automix(MediaLocation::local("track.flac"), None,
//!     AutomixAnalysisOptions::default())?;
//! # let _ = result;
//! # Ok(())
//! # }
//! ```

mod decode;
mod features;
mod placement;
mod tempo;

use audio_engine_core::decoder::DecoderError;
use audio_engine_core::processor::ProcessError;
pub use decode::{analyze_automix, analyze_automix_with_cancel};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// Unreleased v4: Structure's agreed semantics will join this schema before
// release. A commit is not a schema release; cached v3 results need recomputing.
const ANALYSIS_VERSION: u32 = 4;
const DEFAULT_MAX_ANALYZE_TIME_SEC: f64 = 60.0;
const MIN_ANALYZE_TIME_SEC: f64 = 5.0;
const MAX_ANALYZE_TIME_SEC: f64 = 300.0;
const ENVELOPE_RATE: f64 = 50.0;
/// Longest container-declared duration this analysis will treat as real.
///
/// The declared duration comes from untrusted container metadata but sizes the
/// whole-track [`AutomixAnalysis::energy_profile`], one slot per
/// [`ENERGY_PROFILE_RATE`]. Without a bound, a file declaring an absurd
/// duration would ask for an allocation proportional to it, which `vec!` cannot
/// fail gracefully. Twenty-four hours is far beyond any mixable track and still
/// caps the profile at well under ten megabytes.
const MAX_DECLARED_DURATION_SEC: f64 = 24.0 * 60.0 * 60.0;
/// Slots per second in the whole-track energy profile.
const ENERGY_PROFILE_RATE: f64 = 10.0;
const WINDOW_SIZE_MS: usize = 20;
const SILENCE_THRESHOLD_DB: f32 = -48.0;
const HEAD_BEAT_TOLERANCE_SEC: f64 = 0.010;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
/// Amount of a track covered by an AutoMix analysis pass.
pub enum AutomixAnalysisMode {
    /// Analyze only the head/tail windows needed for placement decisions.
    Head,
    /// Analyze the head window plus the trailing tail window.
    ///
    /// Only these two bounded windows are decoded: the track interior between
    /// them is never read, its `energy_profile` entries stay zero, and the
    /// reported loudness covers the analyzed windows rather than the whole
    /// track.
    #[default]
    Full,
}

impl AutomixAnalysisMode {
    /// Whether this mode covers the trailing tail window.
    pub fn includes_tail(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Clone, Debug, Serialize)]
/// Stable offline-analysis result used to plan an AutoMix transition.
pub struct AutomixAnalysis {
    /// Analysis algorithm version; consumers may gate on it.
    pub version: u32,
    /// Analysis mode that produced this result.
    pub mode: AutomixAnalysisMode,
    /// Analyzed track duration in seconds.
    ///
    /// This is the placement timeline, not the decoded-coverage duration: it
    /// is taken from container metadata when plausible, else derived from the
    /// total frame count, and falls back to the analyzed head span when
    /// neither is available. In [`AutomixAnalysisMode::Full`] the interior
    /// between the two analyzed windows is never decoded.
    pub duration: f64,
    /// Requested per-window analysis duration in seconds.
    ///
    /// This reports the configured cap from
    /// [`AutomixAnalysisOptions::max_analyze_time_sec`] rather than the
    /// realized head length; the head window is clamped to the track duration
    /// when the track is shorter than the cap.
    pub analyze_window: f64,
    /// Fitted constant tempo, rounded to 0.01 BPM only for reporting.
    ///
    /// A present tempo may summarize a drifting performance. Consult
    /// [`Self::beat_grid_stability`] before using its constant grid.
    pub bpm: Option<f64>,
    /// Prior-weighted autocorrelation salience times grid stability and the
    /// fraction of tracked beats supported by observed onsets, in 0..1.
    ///
    /// This is an evidence score, not a calibrated probability of correctness.
    /// Without a fitted grid it can report the available periodicity evidence;
    /// silence/insufficient input produces `None`. v3 thresholds do not apply.
    pub bpm_confidence: Option<f64>,
    /// First reported beat at or after the analyzed head origin, in absolute
    /// source seconds. A short isolated head transient can include a fitted
    /// beat up to 10 ms before the origin; that event is reported at the origin.
    /// Internal cut snapping and stability retain the unrounded fitted grid.
    /// This is not a downbeat or bar-start claim.
    pub first_beat_pos: Option<f64>,
    /// Constant-grid stability in 0..1, derived from RMS phase residual.
    ///
    /// Residuals include all supported tracked beats, including robust-fit
    /// outliers. The score is `1 - min(1, residual / min(period/4, 0.125))`,
    /// with times in seconds, so slow grids cannot hide large absolute errors.
    /// `None` (JSON null) means no grid was fitted. Cut snapping
    /// requires stability >=0.80 and [`Self::bpm_confidence`] >=0.35.
    pub beat_grid_stability: Option<f64>,
    /// Integrated loudness in LUFS, when measurable.
    pub loudness: Option<f64>,
    /// True-peak level in dBTP, when measurable.
    pub true_peak_dbtp: Option<f64>,
    /// Recommended fade-in position in seconds.
    pub fade_in_pos: f64,
    /// Recommended fade-out position in seconds.
    pub fade_out_pos: f64,
    /// Beat-aligned cut-in position in seconds, when found.
    pub cut_in_pos: Option<f64>,
    /// Beat-aligned cut-out position in seconds, when found.
    pub cut_out_pos: Option<f64>,
    /// Center of the mixable section in seconds.
    pub mix_center_pos: f64,
    /// Start of the mixable section in seconds.
    pub mix_start_pos: f64,
    /// End of the mixable section in seconds.
    pub mix_end_pos: f64,
    /// Energy envelope slots carry evidence; the interval between them stays
    /// zero. The length follows [`Self::duration`], bounded by an internal
    /// 24-hour `MAX_DECLARED_DURATION_SEC` cap, so a file declaring an absurd
    /// duration cannot size this vector.
    pub energy_profile: Vec<f64>,
    /// Drop (beat-matched break) position in seconds, when found.
    pub drop_pos: Option<f64>,
    /// First vocal entry position in seconds, when detected.
    pub vocal_in_pos: Option<f64>,
    /// Final vocal exit position in seconds, when detected.
    pub vocal_out_pos: Option<f64>,
    /// Last vocal entry position before the outro in seconds, when detected.
    pub vocal_last_in_pos: Option<f64>,
    /// RMS energy of the outro window, when measured.
    pub outro_energy_level: Option<f64>,
}

/// Failures produced by bounded offline AutoMix analysis.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AutomixError {
    /// Analysis was cooperatively canceled.
    #[error("AutoMix analysis canceled")]
    Canceled,
    /// Opening, seeking, or decoding the media source failed.
    #[error("AutoMix decoder operation failed")]
    Decoder(#[from] DecoderError),
    /// EBU R128 construction or ingestion failed during analysis.
    #[error("AutoMix loudness analysis failed")]
    Loudness(#[from] ProcessError),
    /// A coarse decoder seek landed after the requested tail boundary.
    #[error(
        "AutoMix tail seek landed after planned frame {planned_frame}: realized frame {realized_frame}"
    )]
    TailSeekPastStart {
        /// First frame the bounded tail analysis intended to decode.
        planned_frame: u64,
        /// Actual decoder position after the coarse seek.
        realized_frame: u64,
    },
}

#[derive(Clone, Debug)]
/// Bounds and coverage mode for one AutoMix analysis pass.
pub struct AutomixAnalysisOptions {
    /// Which analysis mode to run.
    pub mode: AutomixAnalysisMode,
    /// Per-window source-audio duration cap in seconds.
    ///
    /// Each decoded window (the head, and the tail in
    /// [`AutomixAnalysisMode::Full`]) covers at most this many seconds of
    /// source audio; it is not a wall-clock compute-time budget. Non-finite
    /// values reset to the built-in default.
    pub max_analyze_time_sec: f64,
}

impl Default for AutomixAnalysisOptions {
    fn default() -> Self {
        Self {
            mode: AutomixAnalysisMode::Full,
            max_analyze_time_sec: DEFAULT_MAX_ANALYZE_TIME_SEC,
        }
    }
}

impl AutomixAnalysisOptions {
    /// Clamp analysis time to a finite in-range value.
    pub fn normalized(mut self) -> Self {
        if !self.max_analyze_time_sec.is_finite() {
            self.max_analyze_time_sec = DEFAULT_MAX_ANALYZE_TIME_SEC;
        }
        self.max_analyze_time_sec = self
            .max_analyze_time_sec
            .clamp(MIN_ANALYZE_TIME_SEC, MAX_ANALYZE_TIME_SEC);
        self
    }
}

// A private predicate keeps feature/tempo computation independent of decoder state.
fn check_cancel(cancel: Option<&dyn Fn() -> bool>) -> Result<(), AutomixError> {
    if cancel.is_some_and(|is_canceled| is_canceled()) {
        Err(AutomixError::Canceled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
