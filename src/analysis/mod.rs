//! Offline and read-only audio analysis APIs.
//!
//! Analysis consumes decoded audio and reports measurements or placement
//! evidence; it does not transform samples on the realtime callback path. The
//! implementation modules are being moved here incrementally. Existing
//! `crate::processor::*` and crate-root re-exports remain supported while
//! downstream users migrate to this semantic namespace. Interleaved block
//! geometry is validated by the shared [`crate::audio_block`] contract; the
//! processor lifecycle/error protocol is not duplicated in this module.

mod automix;
mod loudness_info;
mod measurement;
mod spectrum;

pub use automix::{
    analyze_automix, analyze_automix_with_cancel, AutomixAnalysis, AutomixAnalysisMode,
    AutomixAnalysisOptions, AutomixError,
};
pub use loudness_info::LoudnessInfo;
pub(crate) use measurement::{true_peak_fir, true_peak_reconstruction_l1_bound, TRUE_PEAK_DELAY};
pub use measurement::{LoudnessMeter, TruePeakDetector};
pub use spectrum::{SpectrumAnalyzer, SpectrumConfig, WindowFunction};
