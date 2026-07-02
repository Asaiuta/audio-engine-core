//! Reusable audio-engine core.
//!
//! This crate owns app-agnostic decoder, DSP, and streaming pipeline building
//! blocks. The application/server crate layers playback control, persistence,
//! HTTP/WebSocket routes, and runtime directory handling on top.
//!
//! # Example
//!
//! Apply a 10-band graphic equalizer to a block of interleaved stereo audio:
//!
//! ```
//! use audio_engine_core::Equalizer;
//!
//! let sample_rate = 48_000.0;
//! let mut eq = Equalizer::new(2, sample_rate);
//!
//! // Boost the lowest band by +6 dB, leave the rest flat.
//! eq.set_band_gain(0, 6.0, sample_rate);
//! eq.set_enabled(true);
//!
//! // Interleaved L/R samples; `process` filters in place.
//! let mut buffer = vec![0.1_f64; 2 * 512];
//! eq.process(&mut buffer);
//! ```
//!
//! See the `examples/` directory for runnable resampling and EQ programs.

// Arm assert_no_alloc for unit tests: without a registered AllocDisabler the
// `assert_no_alloc(...)` RT-safety tests run their closures without any
// detection. Test builds only; release builds are unaffected.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOC_GUARD: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

pub mod channel_layout;
pub mod config;
pub mod decoder;
pub mod diagnostics;
pub mod pipeline;
pub mod processor;
pub mod runtime;

pub use channel_layout::{ChannelLayout, ChannelPosition};
pub use config::{LoudnessConfig, NormalizationMode};
pub use decoder::StreamingDecoder;
pub use pipeline::RingBuffer;
pub use processor::{
    analyze_automix, callback_stage_names, callback_stage_order_csv,
    canonical_output_stage_descriptors, offline_stage_names, offline_stage_order_csv,
    AtomicLoudnessState, AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions,
    ConvolutionStrategy, DownmixCoefficients, DownmixError, Downmixer, Equalizer, FFTConvolver,
    GainRamp, LimiterMode, LoudnessInfo, LoudnessMeter, LoudnessNormalizer, NoiseShaper,
    OutputChainBuilder, OutputChainParams, OutputRenderChain, OutputStageDescriptor, OutputStageId,
    PeakLimiter, RenderedOutput, Resampler, SpectrumAnalyzer, StreamingResampler, TruePeakDetector,
    VolumeController, PARTITIONED_CONVOLUTION_IR_THRESHOLD, PARTITIONED_CONVOLUTION_PARTITION_SIZE,
};

/// Loudness-database persistence types (requires the `loudness-db` feature).
#[cfg(feature = "loudness-db")]
pub use processor::{DatabaseStats, LoudnessDatabase, TrackLoudness, CURRENT_SCAN_VERSION};
