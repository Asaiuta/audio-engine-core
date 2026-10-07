//! Compile-time identity checks across the package boundary.
use audio_automix::{
    analyze_automix, analyze_automix_with_cancel, analyze_automix_with_descriptors,
    AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions, AutomixDescriptorAnalysis,
    AutomixError, DescriptorReport,
};
use audio_engine_core::decoder::{DecodeCancelToken, DecoderError, HttpCredentials, MediaLocation};
use audio_engine_core::processor::ProcessError;

type AnalysisResult = Result<AutomixAnalysis, AutomixError>;
type DescriptorAnalysisResult = Result<AutomixDescriptorAnalysis, AutomixError>;
const _: fn(MediaLocation, Option<HttpCredentials>, AutomixAnalysisOptions) -> AnalysisResult =
    analyze_automix;
const _: fn(
    MediaLocation,
    Option<HttpCredentials>,
    AutomixAnalysisOptions,
    Option<DecodeCancelToken>,
) -> AnalysisResult = analyze_automix_with_cancel;
const _: fn(
    MediaLocation,
    Option<HttpCredentials>,
    AutomixAnalysisOptions,
    Option<DecodeCancelToken>,
) -> DescriptorAnalysisResult = analyze_automix_with_descriptors;
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
    assert_bounds::<AutomixDescriptorAnalysis>();
    assert_bounds::<DescriptorReport>();
}

#[test]
fn descriptor_request_observes_pre_open_cancellation() {
    let token = DecodeCancelToken::new();
    token.cancel();
    let error = analyze_automix_with_descriptors(
        MediaLocation::local("nonexistent-pre-canceled-descriptor-input.wav"),
        None,
        AutomixAnalysisOptions::default(),
        Some(token),
    )
    .unwrap_err();
    assert!(matches!(error, AutomixError::Canceled));
}
