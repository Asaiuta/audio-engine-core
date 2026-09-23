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
    let _: Option<analysis::DescriptorAnalyzer> = None;
    let _: Option<analysis::DescriptorConfig> = None;
    let _: Option<analysis::SpectralDescriptors> = None;
    let _: Option<analysis::SignalMeasurements> = None;
    let _: Option<analysis::SpectrumAnalyzer> = None;
    let _: Option<analysis::SpectrumConfig> = None;
    let _: Option<analysis::WindowFunction> = None;
    let _: Option<LoudnessInfo> = None;
    let _: Option<processor::LoudnessInfo> = None;
    let _: Option<analysis::LoudnessMeter> = None;
    let _: Option<analysis::TruePeakDetector> = None;
}

#[test]
fn descriptor_api_reports_borrowed_spectral_and_signal_measurements() {
    let config = analysis::DescriptorConfig {
        fft_size: 16,
        hop_size: 7,
        ..Default::default()
    };
    let mut analyzer = analysis::DescriptorAnalyzer::new(&config, 48_000).unwrap();
    assert_eq!(analyzer.push(&[0.0; 16]), 1);
    let spectral: &analysis::SpectralDescriptors = analyzer.spectral().unwrap();
    assert_eq!(spectral.flatness, None);
    let signal: &analysis::SignalMeasurements = analyzer.signal();
    assert_eq!(signal.sample_count, 16);
    assert_eq!(signal.rms, Some(0.0));
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
