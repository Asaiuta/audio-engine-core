//! Compile-level smoke coverage for the canonical analysis namespace and the
//! shared audio-block geometry it consumes.
//!
//! The 1.x compatibility aliases (root and `processor::*` analysis names, the
//! `processor::traits::AudioBlock*` re-exports) were removed for 2.0; this
//! file now pins the canonical paths only. The removal record and migration
//! table live in `.trellis/spec/backend/analysis-compatibility.md` and the
//! CHANGELOG's 2.0.0 section.

use audio_engine_core::{analysis, audio_block, processor, LoudnessInfo};

#[test]
fn canonical_analysis_surface_resolves() {
    let mode: analysis::AutomixAnalysisMode = analysis::AutomixAnalysisMode::Head;
    assert_eq!(mode, analysis::AutomixAnalysisMode::Head);

    let _: Option<analysis::AutomixAnalysis> = None;
    let _: Option<analysis::AutomixAnalysisOptions> = None;
    let _: Option<analysis::AutomixError> = None;
    let _: Option<analysis::SpectrumAnalyzer> = None;
    let _: Option<analysis::SpectrumConfig> = None;
    let _: Option<analysis::WindowFunction> = None;
    let _: Option<LoudnessInfo> = None;
    let _: Option<processor::LoudnessInfo> = None;
    let _: Option<analysis::LoudnessMeter> = None;
    let _: Option<analysis::TruePeakDetector> = None;

    let _analyze = analysis::analyze_automix;
    let _analyze_with_cancel = analysis::analyze_automix_with_cancel;
}

#[test]
fn shared_audio_block_geometry_serves_analysis_and_processors() {
    let mut samples = [0.0_f64; 4];
    let block = audio_block::AudioBlockMut::new(&mut samples, 2).expect("valid block geometry");
    let buffers = processor::traits::ProcessBuffers::in_place(block);
    assert_eq!(buffers.channels(), 2);
    let error: audio_block::AudioBlockError =
        audio_block::AudioBlockRef::new(&[0.0_f64], 2).expect_err("incomplete frame");
    assert_eq!(
        error,
        audio_block::AudioBlockError::IncompleteFrame {
            samples: 1,
            channels: 2
        }
    );
}
