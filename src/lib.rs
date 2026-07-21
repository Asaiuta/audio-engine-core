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
//! Realtime adapters use the unified streaming contract. Blocks borrow the
//! caller's interleaved `f64` storage, and [`process_checked`] validates exact
//! consumed/produced progress without allocating:
//!
//! ```
//! use std::sync::Arc;
//! use audio_engine_core::processor::{AtomicVolumeParams, VolumeProcessor};
//! use audio_engine_core::processor::traits::{
//!     process_checked, AudioBlockMut, ProcessBuffers, ProcessError,
//! };
//!
//! # fn main() -> Result<(), ProcessError> {
//! let params = Arc::new(AtomicVolumeParams::new());
//! params.set_volume(0.5);
//! let mut volume = VolumeProcessor::new(params);
//! let mut samples = [0.25_f64, -0.25, 0.5, -0.5];
//! let block = AudioBlockMut::new(&mut samples, 2)?;
//! let progress = process_checked(&mut volume, ProcessBuffers::in_place(block))?;
//!
//! assert_eq!(progress.consumed_frames(), 2);
//! assert_eq!(progress.produced_frames(), 2);
//! # Ok(())
//! # }
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
    canonical_output_stage_descriptors, canonical_post_render_analysis_descriptors, finish_checked,
    offline_render_stage_names, offline_render_stage_order_csv, post_render_analysis_names,
    post_render_analysis_order_csv, process_checked, AtomicLoudnessState, AudioBlockError,
    AudioBlockMut, AudioBlockRef, AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions,
    AutomixKeyStatus, ChainFinishPolicy, ConvolutionStrategy, ConvolverControl, ConvolverStatus,
    DownmixCoefficients, DownmixError, Downmixer, DspChain, Equalizer, FFTConvolver, FrameDuration,
    FrameRounding, GainRamp, LimiterMode, LoudnessInfo, LoudnessMeter, LoudnessNormalizer,
    NoiseShaper, OfflineRenderPolicy, OutputChainBuilder, OutputChainParams, OutputRenderChain,
    OutputStageDescriptor, OutputStageId, PeakLimiter, PostRenderAnalysisDescriptor,
    PostRenderAnalysisId, ProcessBufferMode, ProcessBufferParts, ProcessBuffers, ProcessCapacity,
    ProcessError, ProcessProgress, ProcessState, RenderTimeline, RenderedOutput, Resampler,
    SaturationEvent, SaturationEventKind, SpectrumAnalyzer, StreamingProcessor, StreamingResampler,
    TailSpec, TimingError, TruePeakDetector, UnknownTailPolicy, VolumeController,
    DEFAULT_CONVOLVER_SAMPLE_RATE_HZ, PARTITIONED_CONVOLUTION_IR_THRESHOLD,
    PARTITIONED_CONVOLUTION_PARTITION_SIZE, RESAMPLER_BACKEND_NAME, SATURATION_TRANSITION_FRAMES,
};

/// Loudness-database persistence types (requires the `loudness-db` feature).
#[cfg(feature = "loudness-db")]
pub use processor::{DatabaseStats, LoudnessDatabase, TrackLoudness, CURRENT_SCAN_VERSION};
