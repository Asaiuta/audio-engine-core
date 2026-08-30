//! Compile-level coverage for the additive analysis namespace.

use audio_engine_core::{
    analysis, audio_block, dsp, processor, AudioBlockError, AudioBlockMut, AudioBlockRef,
    AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions, AutomixError, LoudnessInfo,
    LoudnessMeter, SpectrumAnalyzer, TruePeakDetector,
};

#[test]
fn analysis_facade_matches_existing_public_types() {
    let mode: analysis::AutomixAnalysisMode = AutomixAnalysisMode::Head;
    assert_eq!(mode, analysis::AutomixAnalysisMode::Head);

    let _: Option<analysis::AutomixAnalysis> = None;
    let _: Option<AutomixAnalysis> = None;
    let _: Option<processor::AutomixAnalysis> = None;
    let _: Option<analysis::AutomixAnalysisOptions> = None;
    let _: Option<AutomixAnalysisOptions> = None;
    let _: Option<processor::AutomixAnalysisOptions> = None;
    let _: Option<analysis::AutomixError> = None;
    let _: Option<AutomixError> = None;
    let _: Option<processor::AutomixError> = None;
    let _: Option<analysis::SpectrumAnalyzer> = None;
    let _: Option<SpectrumAnalyzer> = None;
    let _: Option<processor::SpectrumAnalyzer> = None;
    let _: Option<analysis::LoudnessInfo> = None;
    let _: Option<LoudnessInfo> = None;
    let _: Option<processor::LoudnessInfo> = None;
    let _: Option<analysis::LoudnessMeter> = None;
    let _: Option<LoudnessMeter> = None;
    let _: Option<processor::LoudnessMeter> = None;
    let _: Option<analysis::TruePeakDetector> = None;
    let _: Option<TruePeakDetector> = None;
    let _: Option<processor::TruePeakDetector> = None;

    // Block geometry is shared by offline analysis and the streaming protocol;
    // the old processor-traits paths remain aliases to this crate-level owner.
    let _: Option<processor::traits::AudioBlockError> = None;
    let _: Option<processor::traits::AudioBlockRef<'static>> = None;
    let _: Option<processor::traits::AudioBlockMut<'static>> = None;
    let _: Option<audio_block::AudioBlockError> = None;
    let _: Option<audio_block::AudioBlockRef<'static>> = None;
    let _: Option<audio_block::AudioBlockMut<'static>> = None;
    let _: Option<AudioBlockError> = None;
    let _: Option<AudioBlockRef<'static>> = None;
    let _: Option<AudioBlockMut<'static>> = None;

    let mut samples = [0.0_f64; 4];
    let block = audio_block::AudioBlockMut::new(&mut samples, 2).expect("valid block geometry");
    let buffers = processor::traits::ProcessBuffers::in_place(block);
    assert_eq!(buffers.channels(), 2);
    let error: processor::traits::AudioBlockError =
        audio_block::AudioBlockRef::new(&[0.0_f64], 2).expect_err("incomplete frame");
    assert_eq!(
        error,
        audio_block::AudioBlockError::IncompleteFrame {
            samples: 1,
            channels: 2
        }
    );

    let _analyze = analysis::analyze_automix;
    let _analyze_with_cancel = analysis::analyze_automix_with_cancel;
    let _processor_analyze = processor::analyze_automix;
    let _processor_analyze_with_cancel = processor::analyze_automix_with_cancel;

    assert_eq!(dsp::db_to_linear(-6.0), processor::db_to_linear(-6.0));
    assert_eq!(dsp::linear_to_db(0.5), processor::linear_to_db(0.5));
}
