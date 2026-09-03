//! Compile-level coverage for the additive analysis namespace.

use audio_engine_core::{
    analysis, audio_block, dsp, processor, AudioBlockError, AudioBlockMut, AudioBlockRef,
    AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions, AutomixError, LoudnessInfo,
    LoudnessMeter, SpectrumAnalyzer, SpectrumConfig, TruePeakDetector, WindowFunction,
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
    let _: Option<analysis::SpectrumConfig> = None;
    let _: Option<SpectrumConfig> = None;
    let _: Option<processor::SpectrumConfig> = None;
    let _: Option<analysis::WindowFunction> = None;
    let _: Option<WindowFunction> = None;
    let _: Option<processor::WindowFunction> = None;

    // Assign through every facade so this test proves type identity rather
    // than only proving that three same-named paths resolve.
    let root_window = WindowFunction::Hann;
    let canonical_window: analysis::WindowFunction = root_window;
    let _: processor::WindowFunction = canonical_window;
    let root_config = SpectrumConfig::legacy(16, 4);
    let canonical_config: analysis::SpectrumConfig = root_config;
    let processor_config: processor::SpectrumConfig = canonical_config;
    let root_analyzer =
        SpectrumAnalyzer::with_config(processor_config, 48_000).expect("valid spectrum geometry");
    let canonical_analyzer: analysis::SpectrumAnalyzer = root_analyzer;
    let _: processor::SpectrumAnalyzer = canonical_analyzer;

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
