//! Audio Processor Module
//!
//! Realtime-safe DSP building blocks with lock-free parameter passing.
//! Resampling is backend-selectable at compile time: native SoXR (SoX VHQ)
//! via the `soxr` feature or the pure-Rust quality-routed backend via the
//! `rubato` feature. Offline multi-channel resampling parallelizes across
//! channels; the realtime paths are single-threaded and allocation-free.
//!
//! # Modules
//!
//! ## Core Processors
//! - [`StreamingResampler`] and [`Resampler`] - backend-selectable resampling (SoXR or pure Rust)
//! - [`Equalizer`] - 10-band fixed-band graphic IIR equalizer
//! - [`VolumeProcessor`] and [`NoiseShaper`] - Volume control and noise shaping
//! - [`FFTConvolver`] - FFT convolution for FIR filters, with partitioned long-IR routing
//! - [`LoudnessNormalizer`], [`LoudnessMeter`], and [`TruePeakDetector`] - EBU R128 loudness normalization
//! - [`DynamicLoudness`] - ISO 226 dynamic loudness compensation (Fletcher-Munson)
//! - [`Saturation`] - Tube/tape saturation for analog warmth
//! - [`Crossfeed`] - Bauer binaural crossfeed for headphones
//! - [`FirEq`] - FIR EQ with linear/minimum phase options
//!
//! The [`SpectrumAnalyzer`] re-export below is retained for compatibility;
//! its canonical path is [`crate::analysis::SpectrumAnalyzer`].
//!
//! ## Unified Abstraction (Lock-Free Design)
//! - [`StreamingProcessor`] and streaming block/progress types - full consumed/produced,
//!   finish, latency/tail, and reset lifecycle
//! - [`lockfree_params`] - lock-free parameter structures for thread-safe parameter passing
//! - [`adapters`] - processor adapters implementing [`StreamingProcessor`]
//! - [`DspChain`] - composable DSP processing chain
//!
//! Offline/read-only analysis types are also available from [`crate::analysis`].
//! The re-exports in this module remain the compatibility surface for existing
//! consumers; new code should prefer the semantic analysis namespace.

mod atomic_f64;
mod convolver;
mod crossfeed;
mod dynamic_loudness;
mod eq;
mod fir_design;
mod fir_eq;
mod loudness;
mod noise_shaper;
// COMPAT: loudness-db persistence lives at the crate root (`crate::loudness_db`)
// because it is control-side storage, not a realtime DSP building block. The
// historical `processor::*` public re-exports below are retained.
mod output_chain;
mod resampler;
mod saturation;

// New unified abstraction modules
pub mod adapters;
pub mod downmix;
pub mod dsp_chain;
pub mod lockfree_params;
pub mod traits;

// Public processor API re-exports. Analysis entries are compatibility aliases
// tracked in `.trellis/spec/backend/analysis-compatibility.md`.
// COMPAT: analysis-layer — AutoMix implementation now lives under `analysis`,
// while these historical processor paths remain source-compatible.
pub use crate::analysis::{
    analyze_automix, analyze_automix_with_cancel, AutomixAnalysis, AutomixAnalysisMode,
    AutomixAnalysisOptions, AutomixError,
};
pub use convolver::{
    ConvolutionStrategy, FFTConvolver, PARTITIONED_CONVOLUTION_IR_THRESHOLD,
    PARTITIONED_CONVOLUTION_PARTITION_SIZE,
};
pub use crossfeed::{Crossfeed, CrossfeedSettings};
// COMPAT: dsp-layer — scalar gain helpers moved to the crate-level stateless
// DSP namespace; processor-level names remain source-compatible.
pub use crate::dsp::{db_to_linear, linear_to_db};
pub use dynamic_loudness::{DynamicLoudness, LOUDNESS_BANDS, LOUDNESS_BANDS_N};
pub use eq::Equalizer;
pub use fir_eq::{FirEq, FirPhaseMode, STANDARD_BANDS};
pub use loudness::{
    // COMPAT: analysis-layer — measurement types remain here while the
    // normalizer/limiter shared core is split safely.
    AtomicLoudnessState,
    LimiterMode,
    LoudnessNormalizer,
    PeakLimiter,
};
pub use noise_shaper::{NoiseShaper, NoiseShaperCurve};
// COMPAT: analysis-layer — measurement types remain available at their
// historical processor paths while their implementation is analysis-owned.
pub use crate::analysis::{LoudnessMeter, TruePeakDetector};
// LoudnessInfo is processor-owned (normalizer control state); the analysis
// path is the compatibility re-export.
#[cfg(feature = "loudness-db")]
pub use crate::loudness_db::{
    DatabaseStats, LoudnessDatabase, LoudnessDatabaseError, LoudnessSourceIdentity, TrackLoudness,
    CURRENT_SCAN_VERSION, DEFAULT_STREAMING_TARGET_LUFS,
};
pub use loudness::LoudnessInfo;
pub use output_chain::{
    callback_stage_names, callback_stage_order_csv, canonical_output_stage_descriptors,
    canonical_post_render_analysis_descriptors, offline_render_stage_names,
    offline_render_stage_order_csv, post_render_analysis_names, post_render_analysis_order_csv,
    OfflineRenderPolicy, OutputChainBuilder, OutputChainParams, OutputRenderChain,
    OutputStageDescriptor, OutputStageId, PostRenderAnalysisDescriptor, PostRenderAnalysisId,
    RenderTimeline, RenderedOutput, UnknownTailPolicy,
};
pub use resampler::{Resampler, ResamplerError, StreamingResampler, RESAMPLER_BACKEND_NAME};
pub use saturation::{Saturation, SaturationQuality, SaturationSettings, SaturationType};
// COMPAT: analysis-layer — `SpectrumAnalyzer` is an offline/read-only analyzer.
// Its implementation now lives under `analysis`; this re-export keeps the
// historical processor path source-compatible during the physical module
// split. See `.trellis/spec/backend/analysis-compatibility.md`.
pub use crate::analysis::{SpectrumAnalyzer, SpectrumConfig, WindowFunction};

// Re-export unified abstraction types
pub use adapters::{
    ConvolverControl, ConvolverProcessor, ConvolverStatus, CrossfeedProcessor,
    DynamicLoudnessProcessor, EqProcessor, NoiseShaperProcessor, PeakLimiterProcessor,
    SaturationEvent, SaturationEventKind, SaturationProcessor, VolumeProcessor,
    SATURATION_TRANSITION_FRAMES,
};
pub use downmix::{DownmixCoefficients, DownmixError, Downmixer};
pub use dsp_chain::{ChainFinishPolicy, DspChain};
pub use lockfree_params::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry,
    AtomicEqParams, AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams,
    AtomicVolumeParams, CrossfeedParamsSnapshot, DynamicLoudnessParamsSnapshot,
    DynamicLoudnessTuningSnapshot, EqParamsSnapshot, NoiseShaperParamsSnapshot,
    PeakLimiterParamsSnapshot, RealtimeSnapshotReader, SaturationParamsSnapshot,
    SaturationQualityValue, SaturationTypeValue, VolumeParamsSnapshot, CROSSFEED_CUTOFF_HZ_MAX,
    CROSSFEED_CUTOFF_HZ_MIN, CROSSFEED_MIX_MAX, CROSSFEED_MIX_MIN,
    DYNAMIC_LOUDNESS_COMPENSATION_REF_DB_MAX, DYNAMIC_LOUDNESS_COMPENSATION_REF_DB_MIN,
    DYNAMIC_LOUDNESS_PRE_GAIN_DB_MAX, DYNAMIC_LOUDNESS_PRE_GAIN_DB_MIN,
    DYNAMIC_LOUDNESS_STRENGTH_MAX, DYNAMIC_LOUDNESS_STRENGTH_MIN,
    DYNAMIC_LOUDNESS_TRANSITION_DB_MAX, DYNAMIC_LOUDNESS_TRANSITION_DB_MIN,
    DYNAMIC_LOUDNESS_VOLUME_MAX, DYNAMIC_LOUDNESS_VOLUME_MIN, EQ_BANDS, EQ_BAND_GAIN_DB_MAX,
    EQ_BAND_GAIN_DB_MIN, LIMITER_RELEASE_MS_MAX, LIMITER_RELEASE_MS_MIN, LIMITER_THRESHOLD_DB_MAX,
    LIMITER_THRESHOLD_DB_MIN, NOISE_SHAPER_BITS_MAX, NOISE_SHAPER_BITS_MIN, SATURATION_DRIVE_MAX,
    SATURATION_DRIVE_MIN, SATURATION_GAIN_DB_MAX, SATURATION_GAIN_DB_MIN,
    SATURATION_HIGHPASS_CUTOFF_HZ_MAX, SATURATION_HIGHPASS_CUTOFF_HZ_MIN, SATURATION_MIX_MAX,
    SATURATION_MIX_MIN, SATURATION_THRESHOLD_MAX, SATURATION_THRESHOLD_MIN, VOLUME_MAX, VOLUME_MIN,
};
pub use traits::{
    finish_checked, process_checked, AudioBlockError, AudioBlockMut, AudioBlockRef,
    FixedInPlaceProcessor, FrameDuration, FrameRounding, ProcessBufferMode, ProcessBufferParts,
    ProcessBuffers, ProcessCapacity, ProcessError, ProcessProgress, ProcessState,
    StreamingProcessor, TailSpec, TimingError,
};
