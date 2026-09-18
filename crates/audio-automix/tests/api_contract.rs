//! Compile-time identity checks across the package boundary.
use audio_automix::{
    analyze_automix, analyze_automix_with_cancel, AutomixAnalysis, AutomixAnalysisMode,
    AutomixAnalysisOptions, AutomixError,
};
use audio_engine_core::decoder::{DecodeCancelToken, DecoderError, HttpCredentials, MediaLocation};
use audio_engine_core::processor::ProcessError;

type AnalysisResult = Result<AutomixAnalysis, AutomixError>;
const _: fn(MediaLocation, Option<HttpCredentials>, AutomixAnalysisOptions) -> AnalysisResult =
    analyze_automix;
const _: fn(
    MediaLocation,
    Option<HttpCredentials>,
    AutomixAnalysisOptions,
    Option<DecodeCancelToken>,
) -> AnalysisResult = analyze_automix_with_cancel;
const _: fn(DecoderError) -> AutomixError = AutomixError::from;
const _: fn(ProcessError) -> AutomixError = AutomixError::from;

#[test]
fn analysis_values_keep_their_public_thread_and_unwind_bounds() {
    fn assert_bounds<
        T: Send + Sync + Unpin + std::panic::UnwindSafe + std::panic::RefUnwindSafe,
    >() {
    }
    assert_bounds::<AutomixAnalysis>();
    assert_bounds::<AutomixAnalysisOptions>();
    assert_bounds::<AutomixAnalysisMode>();
}
