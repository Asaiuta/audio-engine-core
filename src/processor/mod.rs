//! Audio Processor Module
//!
//! High-performance audio processing pipeline using Rayon for parallelization.
//! Restored SoX VHQ Resampler and High-Order Noise Shaping for f64 Hi-Fi path.
//!
//! # Modules
//!
//! ## Core Processors
//! - [`StreamingResampler`] and [`Resampler`] - SoX VHQ polyphase resampling
//! - [`Equalizer`] and [`BiquadSection`] - 10-band parametric IIR equalizer
//! - [`VolumeController`] and [`NoiseShaper`] - Volume control and noise shaping
//! - [`SpectrumAnalyzer`] - FFT spectrum analyzer
//! - [`FFTConvolver`] - FFT convolution for FIR filters, with partitioned long-IR routing
//! - [`LoudnessNormalizer`], [`LoudnessMeter`], and [`TruePeakDetector`] - EBU R128 loudness normalization
//! - [`DynamicLoudness`] - ISO 226 dynamic loudness compensation (Fletcher-Munson)
//! - [`Saturation`] - Tube/tape saturation for analog warmth
//! - [`Crossfeed`] - Bauer binaural crossfeed for headphones
//! - [`FirEq`] - FIR EQ with linear/minimum phase options
//!
//! ## Unified Abstraction (Lock-Free Design)
//! - [`StreamingProcessor`] and streaming block/progress types - full consumed/produced,
//!   finish, latency/tail, and reset lifecycle
//! - [`lockfree_params`] - lock-free parameter structures for thread-safe parameter passing
//! - [`adapters`] - processor adapters implementing [`StreamingProcessor`]
//! - [`DspChain`] - composable DSP processing chain

mod automix_analysis;
mod convolver;
mod crossfeed;
mod dsp;
mod dynamic_loudness;
mod eq;
mod fir_eq;
mod loudness;
#[cfg(feature = "loudness-db")]
mod loudness_db;
mod output_chain;
mod resampler;
mod saturation;
mod spectrum;

// New unified abstraction modules
pub mod adapters;
pub mod downmix;
pub mod dsp_chain;
pub mod lockfree_params;
pub mod traits;

// Public processor API re-exports.
pub use automix_analysis::{
    analyze_automix, analyze_automix_with_cancel, AutomixAnalysis, AutomixAnalysisMode,
    AutomixAnalysisOptions, AutomixKeyStatus,
};
pub use convolver::{
    ConvolutionStrategy, FFTConvolver, PARTITIONED_CONVOLUTION_IR_THRESHOLD,
    PARTITIONED_CONVOLUTION_PARTITION_SIZE,
};
pub use crossfeed::{Crossfeed, CrossfeedSettings};
pub use dsp::{db_to_linear, linear_to_db, NoiseShaper, NoiseShaperCurve, VolumeController};
pub use dynamic_loudness::{AtomicDynamicLoudnessState, DynamicLoudness, LOUDNESS_BANDS};
pub use eq::{BiquadSection, Equalizer};
pub use fir_eq::{FirEq, FirPhaseMode, STANDARD_BANDS};
pub use loudness::{
    AtomicLoudnessState, GainRamp, LimiterMode, LoudnessInfo, LoudnessMeter, LoudnessNormalizer,
    PeakLimiter, TruePeakDetector,
};
#[cfg(feature = "loudness-db")]
pub use loudness_db::{
    DatabaseStats, LoudnessDatabase, TrackLoudness, CURRENT_SCAN_VERSION,
    DEFAULT_BROADCAST_TARGET_LUFS, DEFAULT_STREAMING_TARGET_LUFS,
};
pub use output_chain::{
    callback_stage_names, callback_stage_order_csv, canonical_output_stage_descriptors,
    canonical_post_render_analysis_descriptors, offline_render_stage_names,
    offline_render_stage_order_csv, post_render_analysis_names, post_render_analysis_order_csv,
    OfflineRenderPolicy, OutputChainBuilder, OutputChainParams, OutputRenderChain,
    OutputStageDescriptor, OutputStageId, PostRenderAnalysisDescriptor, PostRenderAnalysisId,
    RenderTimeline, RenderedOutput, UnknownTailPolicy,
};
pub use resampler::{Resampler, ResamplerError, StreamingResampler};
pub use saturation::{Saturation, SaturationQuality, SaturationSettings, SaturationType};
pub use spectrum::SpectrumAnalyzer;

// Re-export unified abstraction types
pub use adapters::{
    ConvolverControl, ConvolverProcessor, ConvolverStatus, CrossfeedProcessor,
    DynamicLoudnessProcessor, EqProcessor, NoiseShaperProcessor, PeakLimiterProcessor,
    SaturationEvent, SaturationEventKind, SaturationProcessor, VolumeProcessor,
    DEFAULT_CONVOLVER_SAMPLE_RATE_HZ, SATURATION_TRANSITION_FRAMES,
};
pub use downmix::{DownmixCoefficients, DownmixError, Downmixer};
pub use dsp_chain::{ChainFinishPolicy, DspChain};
pub use lockfree_params::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry,
    AtomicEqParams, AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams,
    AtomicVolumeParams, CrossfeedParamsSnapshot, DynamicLoudnessParamsSnapshot, EqParamsSnapshot,
    NoiseShaperParamsSnapshot, PeakLimiterParamsSnapshot, RealtimeSnapshotReader,
    SaturationParamsSnapshot, SaturationQualityValue, SaturationTypeValue, VolumeParamsSnapshot,
    EQ_BANDS,
};
pub use traits::{
    finish_checked, process_checked, AudioBlockError, AudioBlockMut, AudioBlockRef, FrameDuration,
    FrameRounding, ProcessBufferMode, ProcessBufferParts, ProcessBuffers, ProcessCapacity,
    ProcessError, ProcessProgress, ProcessState, StreamingProcessor, TailSpec, TimingError,
};
