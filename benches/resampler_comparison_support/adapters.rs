//! Benchmark-owned adapters for the cross-project resampler comparison.
//!
//! None of these types are part of the crate's production API.  The raw
//! controls deliberately expose upstream lifecycle and latency behavior while
//! sharing one narrow progress contract with the report driver.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CStr;
use std::fs;
use std::io::Read;
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use audio_engine_core::config::{PhaseResponse, ResampleQuality};
use audio_engine_core::{
    finish_checked, process_checked, AudioBlockMut, AudioBlockRef, ProcessBuffers, ProcessState,
    StreamingProcessor, StreamingResampler, RESAMPLER_BACKEND_NAME,
};
use libloading::Library;
use sha2::{Digest, Sha256};

#[cfg(feature = "rubato")]
use rubato::{
    audioadapter::{Adapter, AdapterMut},
    audioadapter_buffers::direct::InterleavedSlice,
    Fft, FixedSync, Indexing, Resampler, WindowFunction,
};
#[cfg(feature = "soxr")]
use soxr::{
    format::Stereo,
    params::{QualityFlags, QualityRecipe, QualitySpec, Rolloff, RuntimeSpec},
    Soxr,
};

use super::{
    rounded_output_frames, EngineIdentity, MetricClassification, NativeArtifactIdentity,
    NativeLibraryIdentity, RatePair, SampleFormat, UnavailableEngine, ADAPTER_SCHEMA, CHANNELS,
    CHUNK_FRAMES, LIBSAMPLERATE_ENGINE_ID, NATIVE_SHIM_ENGINE_IDS, PROJECT_ENGINE_ID, RATE_PAIRS,
    RAW_RUBATO_ENGINE_ID, RAW_SOXR_ENGINE_ID,
};

const RAW_SOXR_CRATE_VERSION: &str = "0.6.0";
const RAW_RUBATO_CRATE_VERSION: &str = "4.0.0";
const RAW_RUBATO_MAX_ZERO_OUTPUT_DRAIN_STEPS: usize = 8;
const LIBSAMPLERATE_SINC_BEST_QUALITY: c_int = 0;
const PINNED_LIBSAMPLERATE_DLL_SHA256: &str =
    "1e08aeb1fecade2cf2d7a83463a1b375e13d5f2f008cdeea7409a3eff7ed9a0e";
const PINNED_LIBSAMPLERATE_PACKAGE_SHA256: &str =
    "454b2d8eb1a22f8df2a84d10fa0244420fde55de877823062e93c192a551f8b6";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AdapterProgress {
    pub(crate) consumed_frames: usize,
    pub(crate) produced_frames: usize,
    pub(crate) finished: bool,
}

pub(crate) struct Discovery {
    pub(crate) factories: Vec<EngineFactory>,
    pub(crate) unavailable: Vec<UnavailableEngine>,
}

#[cfg(feature = "rubato")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RawRubatoGeometry {
    pub(crate) chunk_frames: usize,
    pub(crate) sub_chunks: usize,
}

#[cfg(feature = "rubato")]
impl RawRubatoGeometry {
    pub(crate) const FFT_512_1: Self = Self {
        chunk_frames: 512,
        sub_chunks: 1,
    };
    pub(crate) const FFT_1024_2: Self = Self {
        chunk_frames: 1_024,
        sub_chunks: 2,
    };

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "512/1" => Ok(Self::FFT_512_1),
            "1024/2" => Ok(Self::FFT_1024_2),
            _ => Err(format!(
                "unsupported raw Rubato geometry '{value}'; expected 512/1 or 1024/2"
            )),
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match (self.chunk_frames, self.sub_chunks) {
            (512, 1) => "512/1",
            (1_024, 2) => "1024/2",
            _ => "unsupported",
        }
    }

    fn algorithm_id(self) -> &'static str {
        match (self.chunk_frames, self.sub_chunks) {
            (512, 1) => "raw_rubato_fft512_bh2_subchunk1_compensated_exact_v3",
            (1_024, 2) => "raw_rubato_fft1024_bh2_subchunk2_compensated_exact_v3",
            _ => "raw_rubato_unsupported_geometry",
        }
    }
}

#[derive(Clone)]
pub(crate) struct EngineFactory {
    identity: EngineIdentity,
    kind: EngineFactoryKind,
}

#[derive(Clone)]
enum EngineFactoryKind {
    Project,
    #[cfg(feature = "soxr")]
    RawSoxr,
    #[cfg(feature = "rubato")]
    RawRubato(RawRubatoGeometry),
    LibSamplerate(Arc<LibSamplerateLibrary>),
    NativeShim(Arc<NativeShimLibrary>),
    #[cfg(test)]
    SilentTest,
    #[cfg(test)]
    CreateFailureTest,
}

impl EngineFactory {
    pub(crate) fn identity(&self) -> &EngineIdentity {
        &self.identity
    }

    pub(crate) fn create(
        &self,
        rate: RatePair,
        channels: usize,
        chunk_frames: usize,
    ) -> Result<EngineAdapter, String> {
        match &self.kind {
            EngineFactoryKind::Project => {
                ProjectAdapter::new(rate, channels, chunk_frames).map(EngineAdapter::Project)
            }
            #[cfg(feature = "soxr")]
            EngineFactoryKind::RawSoxr => {
                RawSoxrAdapter::new(rate, channels, chunk_frames).map(EngineAdapter::RawSoxr)
            }
            #[cfg(feature = "rubato")]
            EngineFactoryKind::RawRubato(geometry) => {
                RawRubatoAdapter::new(rate, channels, chunk_frames, *geometry)
                    .map(Box::new)
                    .map(EngineAdapter::RawRubato)
            }
            EngineFactoryKind::LibSamplerate(library) => {
                LibSamplerateAdapter::new(Arc::clone(library), rate, channels, chunk_frames)
                    .map(EngineAdapter::LibSamplerate)
            }
            EngineFactoryKind::NativeShim(library) => {
                NativeShimAdapter::new(Arc::clone(library), rate, channels, chunk_frames)
                    .map(EngineAdapter::NativeShim)
            }
            #[cfg(test)]
            EngineFactoryKind::SilentTest => {
                SilentTestAdapter::new(rate, channels, chunk_frames).map(EngineAdapter::SilentTest)
            }
            #[cfg(test)]
            EngineFactoryKind::CreateFailureTest => Err(format!(
                "intentional canonical create failure for {}",
                rate.id
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn silent_test() -> Self {
        Self {
            identity: EngineIdentity {
                engine_id: "silent_test".to_string(),
                display_name: "silent test adapter".to_string(),
                implementation: "test-only exact-length zero-output adapter".to_string(),
                upstream_version: "test".to_string(),
                adapter_schema: ADAPTER_SCHEMA.to_string(),
                algorithm_id: "silent_test_v1".to_string(),
                sample_format: SampleFormat::InterleavedF64,
                quality_recipe: "consume input and emit exact-length silence".to_string(),
                phase_response: "not a resampler".to_string(),
                native_library: None,
            },
            kind: EngineFactoryKind::SilentTest,
        }
    }

    #[cfg(test)]
    fn create_failure_test() -> Self {
        Self {
            identity: EngineIdentity {
                engine_id: "create_failure_test".to_string(),
                display_name: "create failure test adapter".to_string(),
                implementation: "test-only factory that rejects canonical creation".to_string(),
                upstream_version: "test".to_string(),
                adapter_schema: ADAPTER_SCHEMA.to_string(),
                algorithm_id: "create_failure_test_v1".to_string(),
                sample_format: SampleFormat::InterleavedF64,
                quality_recipe: "not runnable".to_string(),
                phase_response: "not runnable".to_string(),
                native_library: None,
            },
            kind: EngineFactoryKind::CreateFailureTest,
        }
    }
}

#[cfg_attr(
    all(feature = "rubato", not(feature = "soxr")),
    expect(
        clippy::large_enum_variant,
        reason = "boxing only the largest benchmark adapter would charge an engine-specific allocation to measured setup"
    )
)]
pub(crate) enum EngineAdapter {
    Project(ProjectAdapter),
    #[cfg(feature = "soxr")]
    RawSoxr(RawSoxrAdapter),
    #[cfg(feature = "rubato")]
    RawRubato(Box<RawRubatoAdapter>),
    LibSamplerate(LibSamplerateAdapter),
    NativeShim(NativeShimAdapter),
    #[cfg(test)]
    SilentTest(SilentTestAdapter),
}

impl EngineAdapter {
    pub(crate) fn sample_format(&self) -> SampleFormat {
        match self {
            Self::Project(_) => SampleFormat::InterleavedF64,
            #[cfg(feature = "soxr")]
            Self::RawSoxr(_) => SampleFormat::InterleavedF64,
            #[cfg(feature = "rubato")]
            Self::RawRubato(_) => SampleFormat::InterleavedF64,
            Self::LibSamplerate(_) => SampleFormat::InterleavedF32,
            Self::NativeShim(adapter) => adapter.sample_format,
            #[cfg(test)]
            Self::SilentTest(_) => SampleFormat::InterleavedF64,
        }
    }

    pub(crate) fn max_output_frames(&self) -> usize {
        match self {
            Self::Project(adapter) => adapter.max_output_frames,
            #[cfg(feature = "soxr")]
            Self::RawSoxr(adapter) => adapter.max_output_frames,
            #[cfg(feature = "rubato")]
            Self::RawRubato(adapter) => adapter.max_output_frames,
            Self::LibSamplerate(adapter) => adapter.max_output_frames,
            Self::NativeShim(adapter) => adapter.max_output_frames,
            #[cfg(test)]
            Self::SilentTest(adapter) => adapter.max_output_frames,
        }
    }

    pub(crate) fn api_buffering_latency_frames(&self) -> Option<usize> {
        match self {
            Self::Project(adapter) => adapter.api_buffering_latency_frames,
            #[cfg(feature = "soxr")]
            Self::RawSoxr(adapter) => adapter.api_buffering_latency_frames,
            #[cfg(feature = "rubato")]
            Self::RawRubato(adapter) => adapter.api_buffering_latency_frames,
            Self::LibSamplerate(adapter) => adapter.api_buffering_latency_frames,
            Self::NativeShim(adapter) => adapter.api_buffering_latency_frames,
            #[cfg(test)]
            Self::SilentTest(_) => None,
        }
    }

    pub(crate) fn expected_complete_output_frames(&self, input_frames: usize) -> usize {
        match self {
            Self::Project(adapter) => adapter.expected_complete_output_frames(input_frames),
            #[cfg(feature = "soxr")]
            Self::RawSoxr(adapter) => adapter.expected_complete_output_frames(input_frames),
            #[cfg(feature = "rubato")]
            Self::RawRubato(adapter) => adapter.expected_complete_output_frames(input_frames),
            Self::LibSamplerate(adapter) => adapter.expected_complete_output_frames(input_frames),
            Self::NativeShim(adapter) => adapter.expected_complete_output_frames(input_frames),
            #[cfg(test)]
            Self::SilentTest(adapter) => adapter.expected_complete_output_frames(input_frames),
        }
    }

    pub(crate) fn process_f64(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<AdapterProgress, String> {
        match self {
            Self::Project(adapter) => adapter.process(input, output),
            #[cfg(feature = "soxr")]
            Self::RawSoxr(adapter) => adapter.process(input, output),
            #[cfg(feature = "rubato")]
            Self::RawRubato(adapter) => adapter.process(input, output),
            Self::LibSamplerate(_) => {
                Err("libsamplerate uses the interleaved_f32 lane, not interleaved_f64".to_string())
            }
            Self::NativeShim(adapter) => adapter.process_f64(input, output, false),
            #[cfg(test)]
            Self::SilentTest(adapter) => adapter.process(input, output, false),
        }
    }

    pub(crate) fn process_final_f64(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<AdapterProgress, String> {
        match self {
            Self::NativeShim(adapter) => adapter.process_f64(input, output, true),
            #[cfg(test)]
            Self::SilentTest(adapter) => adapter.process(input, output, true),
            _ => self.process_f64(input, output),
        }
    }

    pub(crate) fn process_f32(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<AdapterProgress, String> {
        match self {
            Self::LibSamplerate(adapter) => adapter.process(input, output),
            Self::NativeShim(adapter) => adapter.process_f32(input, output, false),
            _ => Err("selected engine uses the interleaved_f64 lane, not interleaved_f32".into()),
        }
    }

    pub(crate) fn process_final_f32(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<AdapterProgress, String> {
        match self {
            Self::LibSamplerate(adapter) => adapter.process_final(input, output),
            Self::NativeShim(adapter) => adapter.process_f32(input, output, true),
            _ => Err("selected engine uses the interleaved_f64 lane, not interleaved_f32".into()),
        }
    }

    pub(crate) fn drain_f64(&mut self, output: &mut [f64]) -> Result<AdapterProgress, String> {
        match self {
            Self::Project(adapter) => adapter.drain(output),
            #[cfg(feature = "soxr")]
            Self::RawSoxr(adapter) => adapter.drain(output),
            #[cfg(feature = "rubato")]
            Self::RawRubato(adapter) => adapter.drain(output),
            Self::LibSamplerate(_) => {
                Err("libsamplerate uses the interleaved_f32 lane, not interleaved_f64".to_string())
            }
            Self::NativeShim(adapter) => adapter.drain_f64(output),
            #[cfg(test)]
            Self::SilentTest(adapter) => adapter.drain(output),
        }
    }

    pub(crate) fn drain_f32(&mut self, output: &mut [f32]) -> Result<AdapterProgress, String> {
        match self {
            Self::LibSamplerate(adapter) => adapter.drain(output),
            Self::NativeShim(adapter) => adapter.drain_f32(output),
            _ => Err("selected engine uses the interleaved_f64 lane, not interleaved_f32".into()),
        }
    }

    pub(crate) fn reset(&mut self) -> Result<(), String> {
        match self {
            Self::Project(adapter) => adapter.reset(),
            #[cfg(feature = "soxr")]
            Self::RawSoxr(adapter) => adapter.reset(),
            #[cfg(feature = "rubato")]
            Self::RawRubato(adapter) => adapter.reset(),
            Self::LibSamplerate(adapter) => adapter.reset(),
            Self::NativeShim(adapter) => adapter.reset(),
            #[cfg(test)]
            Self::SilentTest(adapter) => adapter.reset(),
        }
    }
}

#[cfg(test)]
pub(crate) struct SilentTestAdapter {
    channels: usize,
    from_hz: u32,
    to_hz: u32,
    max_output_frames: usize,
    total_input_frames: usize,
    total_output_frames: usize,
    end_signalled: bool,
    finished: bool,
}

#[cfg(test)]
impl SilentTestAdapter {
    fn new(rate: RatePair, channels: usize, chunk_frames: usize) -> Result<Self, String> {
        if channels == 0 || chunk_frames == 0 {
            return Err("silent test adapter requires non-zero geometry".to_string());
        }
        Ok(Self {
            channels,
            from_hz: rate.from_hz,
            to_hz: rate.to_hz,
            max_output_frames: generous_output_capacity(chunk_frames, rate),
            total_input_frames: 0,
            total_output_frames: 0,
            end_signalled: false,
            finished: false,
        })
    }

    fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
        end_of_input: bool,
    ) -> Result<AdapterProgress, String> {
        validate_interleaved(input.len(), self.channels, "silent test input")?;
        validate_interleaved(output.len(), self.channels, "silent test output")?;
        if self.end_signalled || self.finished {
            return Err("silent test adapter received input after end-of-input".to_string());
        }
        let input_frames = input.len() / self.channels;
        self.total_input_frames = self
            .total_input_frames
            .checked_add(input_frames)
            .ok_or_else(|| "silent test input total overflowed".to_string())?;
        let target = self.expected_complete_output_frames(self.total_input_frames);
        let produced = target.saturating_sub(self.total_output_frames);
        let output_capacity = output.len() / self.channels;
        if produced > output_capacity {
            return Err(format!(
                "silent test adapter needs {produced} output frames, capacity is {output_capacity}"
            ));
        }
        output[..produced * self.channels].fill(0.0);
        self.total_output_frames += produced;
        self.end_signalled = end_of_input;
        Ok(AdapterProgress {
            consumed_frames: input_frames,
            produced_frames: produced,
            finished: false,
        })
    }

    fn drain(&mut self, _output: &mut [f64]) -> Result<AdapterProgress, String> {
        if !self.end_signalled {
            return Err("silent test drain requires end-of-input".to_string());
        }
        self.finished = true;
        Ok(AdapterProgress {
            consumed_frames: 0,
            produced_frames: 0,
            finished: true,
        })
    }

    fn reset(&mut self) -> Result<(), String> {
        self.total_input_frames = 0;
        self.total_output_frames = 0;
        self.end_signalled = false;
        self.finished = false;
        Ok(())
    }

    fn expected_complete_output_frames(&self, input_frames: usize) -> usize {
        rounded_output_frames(input_frames, self.from_hz, self.to_hz)
    }
}

pub(crate) fn discover(
    libsamplerate_path: Option<&Path>,
    native_library_paths: &BTreeMap<String, std::path::PathBuf>,
    required_engines: &BTreeSet<String>,
    #[cfg(feature = "rubato")] raw_rubato_geometry: RawRubatoGeometry,
) -> Discovery {
    let mut factories = vec![EngineFactory {
        identity: project_identity(),
        kind: EngineFactoryKind::Project,
    }];
    let mut unavailable = Vec::new();

    #[cfg(feature = "soxr")]
    factories.push(EngineFactory {
        identity: raw_soxr_identity(),
        kind: EngineFactoryKind::RawSoxr,
    });
    #[cfg(not(feature = "soxr"))]
    unavailable.push(unavailable_engine(
        RAW_SOXR_ENGINE_ID,
        "raw libsoxr control was not compiled; enable the 'soxr' Cargo feature",
        required_engines,
    ));

    #[cfg(feature = "rubato")]
    factories.push(EngineFactory {
        identity: raw_rubato_identity(raw_rubato_geometry),
        kind: EngineFactoryKind::RawRubato(raw_rubato_geometry),
    });
    #[cfg(not(feature = "rubato"))]
    unavailable.push(unavailable_engine(
        RAW_RUBATO_ENGINE_ID,
        "raw Rubato control was not compiled; enable the 'rubato' Cargo feature",
        required_engines,
    ));

    match libsamplerate_path {
        Some(path) => match LibSamplerateLibrary::load(path) {
            Ok(library) => {
                let identity = library.engine_identity();
                factories.push(EngineFactory {
                    identity,
                    kind: EngineFactoryKind::LibSamplerate(Arc::new(library)),
                });
            }
            Err(error) => unavailable.push(unavailable_engine(
                LIBSAMPLERATE_ENGINE_ID,
                &error,
                required_engines,
            )),
        },
        None => unavailable.push(unavailable_engine(
            LIBSAMPLERATE_ENGINE_ID,
            "no explicit library path; pass --libsamplerate <path> or set AUDIO_BENCH_LIBSAMPLERATE_PATH",
            required_engines,
        )),
    }

    for engine_id in NATIVE_SHIM_ENGINE_IDS {
        match native_library_paths.get(engine_id) {
            Some(path) => match NativeShimLibrary::load(path, engine_id) {
                Ok(library) => {
                    let identity = library.engine_identity.clone();
                    factories.push(EngineFactory {
                        identity,
                        kind: EngineFactoryKind::NativeShim(Arc::new(library)),
                    });
                }
                Err(error) => {
                    unavailable.push(unavailable_engine(engine_id, &error, required_engines))
                }
            },
            None => unavailable.push(unavailable_engine(
                engine_id,
                &format!(
                    "no explicit shim path; pass --engine-library {engine_id}=<absolute-path>"
                ),
                required_engines,
            )),
        }
    }

    probe_canonical_factories(factories, unavailable, required_engines)
}

fn probe_canonical_factories(
    factories: Vec<EngineFactory>,
    mut unavailable: Vec<UnavailableEngine>,
    required_engines: &BTreeSet<String>,
) -> Discovery {
    let mut probed_factories = Vec::with_capacity(factories.len());
    for factory in factories {
        let probe_failure = RATE_PAIRS.iter().find_map(|rate| {
            factory
                .create(*rate, CHANNELS, CHUNK_FRAMES)
                .err()
                .map(|error| format!("{} canonical create probe failed: {error}", rate.id))
        });
        if let Some(error) = probe_failure {
            unavailable.push(unavailable_engine(
                &factory.identity.engine_id,
                &error,
                required_engines,
            ));
        } else {
            probed_factories.push(factory);
        }
    }

    Discovery {
        factories: probed_factories,
        unavailable,
    }
}

fn unavailable_engine(
    engine_id: &str,
    reason: &str,
    required_engines: &BTreeSet<String>,
) -> UnavailableEngine {
    UnavailableEngine {
        engine_id: engine_id.to_string(),
        classification: MetricClassification::Skipped,
        required: required_engines.contains(engine_id),
        reason: reason.to_string(),
    }
}

fn project_identity() -> EngineIdentity {
    let (upstream_version, algorithm_id, quality_recipe) = match RESAMPLER_BACKEND_NAME {
        "soxr" => (
            format!("libsoxr via soxr crate {RAW_SOXR_CRATE_VERSION}"),
            "audio_engine_core_soxr_interleaved_stereo_high_linear_v2".to_string(),
            "audio-engine-core High: libsoxr HQ/Bits20, linear phase, high-precision clock"
                .to_string(),
        ),
        "rubato" => (
            format!("rubato {RAW_RUBATO_CRATE_VERSION}"),
            "audio_engine_core_rubato_fft1024_subchunk2_bulk_io_split_input_terminal_drain_v17".to_string(),
            "audio-engine-core High: Rubato FFT 1024/2 with bulk channel input/output adapters, split FIFO-prefix/caller-suffix input, constrained split spill, and partial-zero terminal-truncating FFT drain, linear phase"
                .to_string(),
        ),
        other => (
            other.to_string(),
            format!("audio_engine_core_{other}_high_linear_v1"),
            "audio-engine-core High, linear phase".to_string(),
        ),
    };
    EngineIdentity {
        engine_id: PROJECT_ENGINE_ID.to_string(),
        display_name: format!("audio-engine-core ({RESAMPLER_BACKEND_NAME})"),
        implementation: format!(
            "audio-engine-core {} StreamingResampler",
            env!("CARGO_PKG_VERSION")
        ),
        upstream_version,
        adapter_schema: ADAPTER_SCHEMA.to_string(),
        algorithm_id,
        sample_format: SampleFormat::InterleavedF64,
        quality_recipe,
        phase_response: "linear; project lifecycle semantics".to_string(),
        native_library: None,
    }
}

#[cfg(feature = "soxr")]
fn raw_soxr_identity() -> EngineIdentity {
    EngineIdentity {
        engine_id: RAW_SOXR_ENGINE_ID.to_string(),
        display_name: "raw libsoxr stereo".to_string(),
        implementation: format!("direct Soxr<Stereo<f64>> via soxr {RAW_SOXR_CRATE_VERSION}"),
        upstream_version: format!("libsoxr via soxr crate {RAW_SOXR_CRATE_VERSION}"),
        adapter_schema: ADAPTER_SCHEMA.to_string(),
        algorithm_id: "raw_libsoxr_hq_bits20_linear_single_thread_v1".to_string(),
        sample_format: SampleFormat::InterleavedF64,
        quality_recipe:
            "QualityRecipe::high/Bits20; Small rolloff; HighPrecisionClock; RuntimeSpec threads=1"
                .to_string(),
        phase_response: "linear; phase_response=50".to_string(),
        native_library: None,
    }
}

#[cfg(feature = "rubato")]
fn raw_rubato_identity(geometry: RawRubatoGeometry) -> EngineIdentity {
    EngineIdentity {
        engine_id: RAW_RUBATO_ENGINE_ID.to_string(),
        display_name: "raw Rubato FFT".to_string(),
        implementation: format!("direct rubato::Fft<f64> {RAW_RUBATO_CRATE_VERSION}"),
        upstream_version: format!("rubato {RAW_RUBATO_CRATE_VERSION}"),
        adapter_schema: ADAPTER_SCHEMA.to_string(),
        algorithm_id: geometry.algorithm_id().to_string(),
        sample_format: SampleFormat::InterleavedF64,
        quality_recipe: format!(
            "Fft<f64>; requested_chunk_frames={}; BlackmanHarris2; sub_chunks={}; FixedSync::Input; native multichannel; adapter delay compensation and exact-duration pacing",
            geometry.chunk_frames, geometry.sub_chunks
        ),
        phase_response:
            "linear; native output_delay reported by recipe and compensated before caller output"
                .to_string(),
        native_library: None,
    }
}

pub(crate) struct ProjectAdapter {
    resampler: StreamingResampler,
    channels: usize,
    from_hz: u32,
    to_hz: u32,
    max_output_frames: usize,
    api_buffering_latency_frames: Option<usize>,
}

impl ProjectAdapter {
    fn new(rate: RatePair, channels: usize, chunk_frames: usize) -> Result<Self, String> {
        let resampler = StreamingResampler::with_quality(
            channels,
            rate.from_hz,
            rate.to_hz,
            PhaseResponse::Linear,
            ResampleQuality::High,
        )
        .map_err(|error| format!("audio-engine-core resampler setup failed: {error}"))?;
        let estimated_samples =
            resampler.max_output_len_for_input(chunk_frames.saturating_mul(channels));
        // Match the caller capacity supplied to the raw controls. The public
        // resampler accepts arbitrary output capacity and exposes backpressure;
        // its 16K internal-step bound is not the current 512-frame workload.
        let max_output_frames = div_ceil(estimated_samples, channels).max(1);
        let api_buffering_latency_frames = Some(resampler.latency().frames());
        Ok(Self {
            resampler,
            channels,
            from_hz: rate.from_hz,
            to_hz: rate.to_hz,
            max_output_frames,
            api_buffering_latency_frames,
        })
    }

    fn process(&mut self, input: &[f64], output: &mut [f64]) -> Result<AdapterProgress, String> {
        validate_interleaved(input.len(), self.channels, "project input")?;
        validate_interleaved(output.len(), self.channels, "project output")?;
        let input_frames = input.len() / self.channels;
        let output_frames = output.len() / self.channels;
        let input = AudioBlockRef::new(input, self.channels)
            .map_err(|error| format!("project input block failed: {error}"))?;
        let output = AudioBlockMut::new(output, self.channels)
            .map_err(|error| format!("project output block failed: {error}"))?;
        let progress = process_checked(
            &mut self.resampler,
            ProcessBuffers::out_of_place(input, output)
                .map_err(|error| format!("project process buffers failed: {error}"))?,
        )
        .map_err(|error| format!("project resampler process failed: {error}"))?;
        validate_progress(
            "project process",
            progress.consumed_frames(),
            progress.produced_frames(),
            input_frames,
            output_frames,
        )?;
        Ok(AdapterProgress {
            consumed_frames: progress.consumed_frames(),
            produced_frames: progress.produced_frames(),
            finished: progress.state() == ProcessState::Finished,
        })
    }

    fn drain(&mut self, output: &mut [f64]) -> Result<AdapterProgress, String> {
        validate_interleaved(output.len(), self.channels, "project drain output")?;
        let output_frames = output.len() / self.channels;
        let block = AudioBlockMut::new(output, self.channels)
            .map_err(|error| format!("project drain block failed: {error}"))?;
        let progress = finish_checked(&mut self.resampler, block)
            .map_err(|error| format!("project resampler drain failed: {error}"))?;
        validate_progress(
            "project drain",
            progress.consumed_frames(),
            progress.produced_frames(),
            0,
            output_frames,
        )?;
        Ok(AdapterProgress {
            consumed_frames: progress.consumed_frames(),
            produced_frames: progress.produced_frames(),
            finished: progress.state() == ProcessState::Finished,
        })
    }

    fn reset(&mut self) -> Result<(), String> {
        self.resampler
            .reset()
            .map_err(|error| format!("project resampler reset failed: {error}"))
    }

    fn expected_complete_output_frames(&self, input_frames: usize) -> usize {
        rounded_output_frames(input_frames, self.from_hz, self.to_hz)
    }
}

#[cfg(feature = "soxr")]
pub(crate) struct RawSoxrAdapter {
    resampler: Soxr<Stereo<f64>>,
    from_hz: u32,
    to_hz: u32,
    max_output_frames: usize,
    api_buffering_latency_frames: Option<usize>,
    finished: bool,
}

#[cfg(feature = "soxr")]
impl RawSoxrAdapter {
    fn new(rate: RatePair, channels: usize, chunk_frames: usize) -> Result<Self, String> {
        if channels != 2 {
            return Err(format!(
                "raw libsoxr stereo adapter requires 2 channels, received {channels}"
            ));
        }
        let quality = QualitySpec::configure(
            QualityRecipe::high(),
            Rolloff::default(),
            QualityFlags::HighPrecisionClock,
        )
        .with_phase_response(50.0);
        let runtime = RuntimeSpec::new(1);
        let resampler = Soxr::<Stereo<f64>>::new_with_params(
            rate.from_hz as f64,
            rate.to_hz as f64,
            quality,
            runtime,
        )
        .map_err(|error| format!("raw libsoxr setup failed: {error:?}"))?;
        Ok(Self {
            resampler,
            from_hz: rate.from_hz,
            to_hz: rate.to_hz,
            max_output_frames: generous_output_capacity(chunk_frames, rate),
            api_buffering_latency_frames: None,
            finished: false,
        })
    }

    fn process(&mut self, input: &[f64], output: &mut [f64]) -> Result<AdapterProgress, String> {
        if self.finished {
            return Err("raw libsoxr received input after terminal drain".to_string());
        }
        let input = stereo_frames(input, "raw libsoxr input")?;
        let output = stereo_frames_mut(output, "raw libsoxr output")?;
        let input_frames = input.len();
        let output_frames = output.len();
        let progress = self
            .resampler
            .process(input, output)
            .map_err(|error| format!("raw libsoxr process failed: {error:?}"))?;
        validate_progress(
            "raw libsoxr process",
            progress.input_frames,
            progress.output_frames,
            input_frames,
            output_frames,
        )?;
        if input_frames > 0 && progress.input_frames == 0 && progress.output_frames == 0 {
            return Err("raw libsoxr process made no progress".to_string());
        }
        Ok(AdapterProgress {
            consumed_frames: progress.input_frames,
            produced_frames: progress.output_frames,
            finished: false,
        })
    }

    fn drain(&mut self, output: &mut [f64]) -> Result<AdapterProgress, String> {
        if self.finished {
            return Ok(AdapterProgress {
                consumed_frames: 0,
                produced_frames: 0,
                finished: true,
            });
        }
        let output = stereo_frames_mut(output, "raw libsoxr drain output")?;
        let capacity = output.len();
        let produced = self
            .resampler
            .drain(output)
            .map_err(|error| format!("raw libsoxr drain failed: {error:?}"))?;
        validate_progress("raw libsoxr drain", 0, produced, 0, capacity)?;
        self.finished = produced == 0;
        Ok(AdapterProgress {
            consumed_frames: 0,
            produced_frames: produced,
            finished: self.finished,
        })
    }

    fn reset(&mut self) -> Result<(), String> {
        self.resampler
            .clear()
            .map_err(|error| format!("raw libsoxr reset failed: {error:?}"))?;
        self.finished = false;
        Ok(())
    }

    fn expected_complete_output_frames(&self, input_frames: usize) -> usize {
        rounded_output_frames(input_frames, self.from_hz, self.to_hz)
    }
}

#[cfg(feature = "rubato")]
struct FixedSampleRing {
    data: Box<[f64]>,
    head: usize,
    len: usize,
}

#[cfg(feature = "rubato")]
impl FixedSampleRing {
    fn new(capacity: usize) -> Self {
        Self {
            data: vec![0.0; capacity].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn free(&self) -> usize {
        self.data.len() - self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    fn push(&mut self, source: &[f64]) -> Result<(), String> {
        if source.len() > self.free() {
            return Err("raw Rubato fixed ring capacity exceeded".to_string());
        }
        if source.is_empty() {
            return Ok(());
        }
        let capacity = self.data.len();
        let tail = (self.head + self.len) % capacity;
        let first = source.len().min(capacity - tail);
        self.data[tail..tail + first].copy_from_slice(&source[..first]);
        let remaining = source.len() - first;
        if remaining > 0 {
            self.data[..remaining].copy_from_slice(&source[first..]);
        }
        self.len += source.len();
        Ok(())
    }

    fn front_contiguous(&self, samples: usize) -> Option<&[f64]> {
        let end = self.head.checked_add(samples)?;
        if samples > self.len || end > self.data.len() {
            return None;
        }
        Some(&self.data[self.head..end])
    }

    fn consume(&mut self, samples: usize) -> Result<(), String> {
        if samples > self.len {
            return Err("raw Rubato fixed ring underflow".to_string());
        }
        self.len -= samples;
        if self.len == 0 {
            self.head = 0;
        } else {
            self.head = (self.head + samples) % self.data.len();
        }
        Ok(())
    }

    fn pop_into(&mut self, output: &mut [f64]) -> usize {
        let samples = output.len().min(self.len);
        if samples == 0 {
            return 0;
        }
        let first = samples.min(self.data.len() - self.head);
        output[..first].copy_from_slice(&self.data[self.head..self.head + first]);
        let remaining = samples - first;
        if remaining > 0 {
            output[first..samples].copy_from_slice(&self.data[..remaining]);
        }
        self.len -= samples;
        if self.len == 0 {
            self.head = 0;
        } else {
            self.head = (self.head + samples) % self.data.len();
        }
        samples
    }
}

#[cfg(feature = "rubato")]
struct StrictSplitInterleavedOutput<'a> {
    direct: &'a mut [f64],
    spill: &'a mut [f64],
    channels: usize,
    native_frames: usize,
    drop_frames: usize,
    direct_frames: usize,
}

#[cfg(feature = "rubato")]
impl<'a> StrictSplitInterleavedOutput<'a> {
    fn new(
        direct: &'a mut [f64],
        spill: &'a mut [f64],
        channels: usize,
        native_frames: usize,
        drop_frames: usize,
        direct_frames: usize,
    ) -> Result<Self, String> {
        if channels == 0 || drop_frames > native_frames {
            return Err("raw Rubato split output received invalid geometry".to_string());
        }
        let kept_frames = native_frames - drop_frames;
        if direct_frames > kept_frames {
            return Err("raw Rubato split output direct prefix exceeded native output".to_string());
        }
        let direct_samples = direct_frames
            .checked_mul(channels)
            .ok_or_else(|| "raw Rubato split direct length overflowed".to_string())?;
        let spill_samples = (kept_frames - direct_frames)
            .checked_mul(channels)
            .ok_or_else(|| "raw Rubato split spill length overflowed".to_string())?;
        if direct.len() < direct_samples || spill.len() < spill_samples {
            return Err("raw Rubato split output backing storage was too small".to_string());
        }
        Ok(Self {
            direct: &mut direct[..direct_samples],
            spill: &mut spill[..spill_samples],
            channels,
            native_frames,
            drop_frames,
            direct_frames,
        })
    }
}

#[cfg(feature = "rubato")]
unsafe impl Adapter<f64> for StrictSplitInterleavedOutput<'_> {
    unsafe fn read_sample_unchecked(&self, channel: usize, frame: usize) -> f64 {
        if frame < self.drop_frames {
            return 0.0;
        }
        let kept_frame = frame - self.drop_frames;
        if kept_frame < self.direct_frames {
            let index = kept_frame * self.channels + channel;
            unsafe { *self.direct.get_unchecked(index) }
        } else {
            let index = (kept_frame - self.direct_frames) * self.channels + channel;
            unsafe { *self.spill.get_unchecked(index) }
        }
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn frames(&self) -> usize {
        self.native_frames
    }
}

#[cfg(feature = "rubato")]
unsafe impl AdapterMut<f64> for StrictSplitInterleavedOutput<'_> {
    unsafe fn write_sample_unchecked(&mut self, channel: usize, frame: usize, value: &f64) -> bool {
        if frame < self.drop_frames {
            return false;
        }
        let kept_frame = frame - self.drop_frames;
        if kept_frame < self.direct_frames {
            let index = kept_frame * self.channels + channel;
            unsafe {
                *self.direct.get_unchecked_mut(index) = *value;
            }
        } else {
            let index = (kept_frame - self.direct_frames) * self.channels + channel;
            unsafe {
                *self.spill.get_unchecked_mut(index) = *value;
            }
        }
        false
    }

    fn copy_from_slice_to_channel(
        &mut self,
        channel: usize,
        skip: usize,
        slice: &[f64],
    ) -> (usize, usize) {
        if channel >= self.channels || skip >= self.native_frames {
            return (0, 0);
        }
        let frames = slice.len().min(self.native_frames - skip);
        let source_end = skip + frames;
        let direct_native_start = skip.max(self.drop_frames);
        let direct_native_end = source_end.min(self.drop_frames + self.direct_frames);
        for native_frame in direct_native_start..direct_native_end {
            let source_index = native_frame - skip;
            let direct_frame = native_frame - self.drop_frames;
            let direct_index = direct_frame * self.channels + channel;
            unsafe {
                *self.direct.get_unchecked_mut(direct_index) = *slice.get_unchecked(source_index);
            }
        }
        let spill_native_start = skip.max(self.drop_frames + self.direct_frames);
        for native_frame in spill_native_start..source_end {
            let source_index = native_frame - skip;
            let spill_frame = native_frame - self.drop_frames - self.direct_frames;
            let spill_index = spill_frame * self.channels + channel;
            unsafe {
                *self.spill.get_unchecked_mut(spill_index) = *slice.get_unchecked(source_index);
            }
        }
        (frames, 0)
    }
}

#[cfg(feature = "rubato")]
#[allow(clippy::too_many_arguments)]
fn run_strict_rubato_chunk(
    resampler: &mut Fft<f64>,
    channels: usize,
    delay_remaining: &mut usize,
    output_ring: &mut FixedSampleRing,
    spill_stage: &mut [f64],
    emitted_frames: &mut usize,
    input: &[f64],
    output: &mut [f64],
    authorized_total_frames: usize,
    indexing: Option<&Indexing>,
    retain_spill: bool,
) -> Result<(usize, usize, usize), String> {
    validate_interleaved(input.len(), channels, "raw Rubato native input")?;
    validate_interleaved(output.len(), channels, "raw Rubato native output")?;
    let input_frames = input.len() / channels;
    let caller_frames = output.len() / channels;
    let native_frames = resampler.output_frames_next();
    let budget = authorized_total_frames.saturating_sub(*emitted_frames);
    let pending_frames = output_ring.len() / channels;
    let pending_direct = pending_frames.min(budget).min(caller_frames);
    let drop_frames = (*delay_remaining).min(native_frames);
    let kept_frames = native_frames - drop_frames;
    let current_direct = kept_frames
        .min(budget.saturating_sub(pending_direct))
        .min(caller_frames.saturating_sub(pending_direct));
    let spill_frames = kept_frames - current_direct;
    let pending_samples = pending_direct
        .checked_mul(channels)
        .ok_or_else(|| "raw Rubato pending output length overflowed".to_string())?;
    let spill_samples = spill_frames
        .checked_mul(channels)
        .ok_or_else(|| "raw Rubato spill output length overflowed".to_string())?;
    if spill_samples > spill_stage.len() {
        return Err("raw Rubato spill stage was too small".to_string());
    }
    if retain_spill
        && spill_samples
            > output_ring
                .free()
                .checked_add(pending_samples)
                .ok_or_else(|| "raw Rubato output capacity overflowed".to_string())?
    {
        return Err("raw Rubato output ring lacked spill capacity".to_string());
    }
    let direct_samples = current_direct * channels;
    let direct_start = pending_samples;
    let direct_end = direct_start + direct_samples;
    let input_adapter = InterleavedSlice::new(input, channels, input_frames)
        .map_err(|error| format!("raw Rubato input view failed: {error}"))?;
    let (native_consumed, native_produced) = {
        let mut split_output = StrictSplitInterleavedOutput::new(
            &mut output[direct_start..direct_end],
            &mut spill_stage[..spill_samples],
            channels,
            native_frames,
            drop_frames,
            current_direct,
        )?;
        resampler
            .process_into_buffer(&input_adapter, &mut split_output, indexing)
            .map_err(|error| format!("raw Rubato process failed: {error}"))?
    };
    validate_progress(
        "raw Rubato native process",
        native_consumed,
        native_produced,
        input_frames,
        native_frames,
    )?;

    let actual_drop = (*delay_remaining).min(native_produced);
    *delay_remaining -= actual_drop;
    let actual_kept = native_produced - actual_drop;
    let actual_current_direct = current_direct.min(actual_kept);
    let actual_spill = actual_kept - actual_current_direct;
    let emitted_pending = output_ring.pop_into(&mut output[..pending_samples]) / channels;
    if emitted_pending != pending_direct {
        return Err("raw Rubato pending output changed during split processing".to_string());
    }
    *emitted_frames = emitted_frames
        .checked_add(emitted_pending + actual_current_direct)
        .ok_or_else(|| "raw Rubato emitted-frame total overflowed".to_string())?;
    if retain_spill && actual_spill > 0 {
        output_ring.push(&spill_stage[..actual_spill * channels])?;
    }
    Ok((
        emitted_pending + actual_current_direct,
        native_consumed,
        native_produced,
    ))
}

#[cfg(feature = "rubato")]
pub(crate) struct RawRubatoAdapter {
    resampler: Fft<f64>,
    geometry: RawRubatoGeometry,
    channels: usize,
    from_hz: u32,
    to_hz: u32,
    input_ring: FixedSampleRing,
    output_ring: FixedSampleRing,
    spill_stage: Vec<f64>,
    zero_input: Vec<f64>,
    max_output_frames: usize,
    api_buffering_latency_frames: Option<usize>,
    initial_output_delay_frames: usize,
    delay_remaining: usize,
    total_input_frames: usize,
    processed_real_input_frames: usize,
    emitted_frames: usize,
    draining: bool,
    finished: bool,
}

#[cfg(feature = "rubato")]
impl RawRubatoAdapter {
    fn new(
        rate: RatePair,
        channels: usize,
        caller_chunk_frames: usize,
        geometry: RawRubatoGeometry,
    ) -> Result<Self, String> {
        if channels == 0 || caller_chunk_frames == 0 {
            return Err("raw Rubato requires non-zero caller geometry".to_string());
        }
        let resampler = Fft::<f64>::new_custom(
            rate.from_hz as usize,
            rate.to_hz as usize,
            geometry.chunk_frames,
            geometry.sub_chunks,
            channels,
            WindowFunction::BlackmanHarris2,
            FixedSync::Input,
        )
        .map_err(|error| format!("raw Rubato FFT setup failed: {error}"))?;
        if resampler.input_frames_next() != geometry.chunk_frames {
            return Err(format!(
                "raw Rubato fixed-input size changed: expected {}, got {}",
                geometry.chunk_frames,
                resampler.input_frames_next()
            ));
        }
        if caller_chunk_frames > geometry.chunk_frames * 2 {
            return Err(format!(
                "raw Rubato caller chunk {caller_chunk_frames} exceeds the bounded {}-frame staging geometry",
                geometry.chunk_frames * 2
            ));
        }
        let max_output_frames = resampler.output_frames_max().max(1);
        let initial_output_delay_frames = resampler.output_delay();
        Ok(Self {
            input_ring: FixedSampleRing::new(geometry.chunk_frames * 2 * channels),
            output_ring: FixedSampleRing::new(max_output_frames * 2 * channels),
            spill_stage: vec![0.0; max_output_frames * channels],
            zero_input: vec![0.0; resampler.input_frames_max() * channels],
            resampler,
            geometry,
            channels,
            from_hz: rate.from_hz,
            to_hz: rate.to_hz,
            max_output_frames,
            api_buffering_latency_frames: Some(0),
            initial_output_delay_frames,
            delay_remaining: initial_output_delay_frames,
            total_input_frames: 0,
            processed_real_input_frames: 0,
            emitted_frames: 0,
            draining: false,
            finished: false,
        })
    }

    fn authorized_output_frames(&self, input_frames: usize) -> usize {
        rounded_output_frames(input_frames, self.from_hz, self.to_hz)
    }

    fn emit_pending(&mut self, output: &mut [f64], authorized_total: usize) -> usize {
        let output_frames = output.len() / self.channels;
        let frames = (self.output_ring.len() / self.channels)
            .min(authorized_total.saturating_sub(self.emitted_frames))
            .min(output_frames);
        let samples = frames * self.channels;
        let copied = self.output_ring.pop_into(&mut output[..samples]);
        let copied_frames = copied / self.channels;
        self.emitted_frames += copied_frames;
        copied_frames
    }

    fn process(&mut self, input: &[f64], output: &mut [f64]) -> Result<AdapterProgress, String> {
        if self.draining || self.finished {
            return Err("raw Rubato received input after drain started".to_string());
        }
        validate_interleaved(input.len(), self.channels, "raw Rubato input")?;
        validate_interleaved(output.len(), self.channels, "raw Rubato output")?;
        let input_frames = input.len() / self.channels;
        let output_frames = output.len() / self.channels;
        let mut consumed = 0usize;
        let mut produced = self.emit_pending(
            output,
            self.authorized_output_frames(self.processed_real_input_frames),
        );

        while self.input_ring.is_empty()
            && input_frames - consumed >= self.geometry.chunk_frames
            && produced < output_frames
        {
            let start = consumed * self.channels;
            let end = (consumed + self.geometry.chunk_frames) * self.channels;
            let next_processed = self
                .processed_real_input_frames
                .checked_add(self.geometry.chunk_frames)
                .ok_or_else(|| "raw Rubato processed-input total overflowed".to_string())?;
            let authorized = self.authorized_output_frames(next_processed);
            let (written, native_consumed, _) = run_strict_rubato_chunk(
                &mut self.resampler,
                self.channels,
                &mut self.delay_remaining,
                &mut self.output_ring,
                &mut self.spill_stage,
                &mut self.emitted_frames,
                &input[start..end],
                &mut output[produced * self.channels..],
                authorized,
                None,
                true,
            )?;
            if native_consumed != self.geometry.chunk_frames {
                return Err(format!(
                    "raw Rubato consumed {native_consumed} of {} direct frames",
                    self.geometry.chunk_frames
                ));
            }
            consumed += native_consumed;
            produced += written;
            self.processed_real_input_frames = next_processed;
        }

        let remaining_samples = (input_frames - consumed) * self.channels;
        if remaining_samples > 0 {
            self.input_ring.push(&input[consumed * self.channels..])?;
            consumed = input_frames;
        }
        self.total_input_frames = self
            .total_input_frames
            .checked_add(consumed)
            .ok_or_else(|| "raw Rubato input total overflowed".to_string())?;

        while self.input_ring.len() / self.channels >= self.geometry.chunk_frames
            && produced < output_frames
        {
            let chunk_samples = self.geometry.chunk_frames * self.channels;
            let input_chunk = self
                .input_ring
                .front_contiguous(chunk_samples)
                .ok_or_else(|| "raw Rubato input ring lost chunk contiguity".to_string())?;
            let next_processed = self
                .processed_real_input_frames
                .checked_add(self.geometry.chunk_frames)
                .ok_or_else(|| "raw Rubato processed-input total overflowed".to_string())?;
            let authorized = self.authorized_output_frames(next_processed);
            let (written, native_consumed, _) = run_strict_rubato_chunk(
                &mut self.resampler,
                self.channels,
                &mut self.delay_remaining,
                &mut self.output_ring,
                &mut self.spill_stage,
                &mut self.emitted_frames,
                input_chunk,
                &mut output[produced * self.channels..],
                authorized,
                None,
                true,
            )?;
            if native_consumed != self.geometry.chunk_frames {
                return Err(format!(
                    "raw Rubato consumed {native_consumed} of {} staged frames",
                    self.geometry.chunk_frames
                ));
            }
            self.input_ring.consume(chunk_samples)?;
            self.processed_real_input_frames = next_processed;
            produced += written;
        }

        Ok(AdapterProgress {
            consumed_frames: consumed,
            produced_frames: produced,
            finished: false,
        })
    }

    fn drain(&mut self, output: &mut [f64]) -> Result<AdapterProgress, String> {
        if self.finished {
            return Ok(AdapterProgress {
                consumed_frames: 0,
                produced_frames: 0,
                finished: true,
            });
        }
        self.draining = true;
        validate_interleaved(output.len(), self.channels, "raw Rubato drain output")?;
        let target = self.expected_complete_output_frames(self.total_input_frames);
        let output_frames = output.len() / self.channels;
        let mut produced = self.emit_pending(output, target);
        if self.emitted_frames == target {
            self.finished = true;
            return Ok(AdapterProgress {
                consumed_frames: 0,
                produced_frames: produced,
                finished: true,
            });
        }

        if !self.input_ring.is_empty() && produced < output_frames {
            let staged_frames = self.input_ring.len() / self.channels;
            let pad_frames = self.geometry.chunk_frames.saturating_sub(staged_frames);
            self.input_ring
                .push(&self.zero_input[..pad_frames * self.channels])?;
            let chunk_samples = self.geometry.chunk_frames * self.channels;
            let input_chunk = self
                .input_ring
                .front_contiguous(chunk_samples)
                .ok_or_else(|| "raw Rubato drain input ring lost contiguity".to_string())?;
            let (written, native_consumed, _) = run_strict_rubato_chunk(
                &mut self.resampler,
                self.channels,
                &mut self.delay_remaining,
                &mut self.output_ring,
                &mut self.spill_stage,
                &mut self.emitted_frames,
                input_chunk,
                &mut output[produced * self.channels..],
                target,
                None,
                false,
            )?;
            if native_consumed != self.geometry.chunk_frames {
                return Err(format!(
                    "raw Rubato drain consumed {native_consumed} of {} padded frames",
                    self.geometry.chunk_frames
                ));
            }
            self.input_ring.consume(chunk_samples)?;
            self.processed_real_input_frames = self.total_input_frames;
            produced += written;
        }

        let mut zero_output_steps = 0usize;
        while self.emitted_frames < target && produced < output_frames {
            let input_frames = self.resampler.input_frames_next();
            let indexing = Indexing::new().partial_len(0);
            let (written, _native_consumed, native_produced) = run_strict_rubato_chunk(
                &mut self.resampler,
                self.channels,
                &mut self.delay_remaining,
                &mut self.output_ring,
                &mut self.spill_stage,
                &mut self.emitted_frames,
                &self.zero_input[..input_frames * self.channels],
                &mut output[produced * self.channels..],
                target,
                Some(&indexing),
                false,
            )?;
            produced += written;
            if native_produced > 0 {
                zero_output_steps = 0;
            } else {
                zero_output_steps += 1;
                if zero_output_steps > RAW_RUBATO_MAX_ZERO_OUTPUT_DRAIN_STEPS {
                    return Err(format!(
                        "raw Rubato drain exceeded {} zero-output state transitions at {} of {target} required frames",
                        RAW_RUBATO_MAX_ZERO_OUTPUT_DRAIN_STEPS, self.emitted_frames
                    ));
                }
            }
        }
        self.finished = self.emitted_frames == target;
        Ok(AdapterProgress {
            consumed_frames: 0,
            produced_frames: produced,
            finished: self.finished,
        })
    }

    fn reset(&mut self) -> Result<(), String> {
        self.resampler.reset();
        self.input_ring.clear();
        self.output_ring.clear();
        self.delay_remaining = self.initial_output_delay_frames;
        self.total_input_frames = 0;
        self.processed_real_input_frames = 0;
        self.emitted_frames = 0;
        self.draining = false;
        self.finished = false;
        Ok(())
    }

    fn expected_complete_output_frames(&self, input_frames: usize) -> usize {
        rounded_output_frames(input_frames, self.from_hz, self.to_hz)
    }
}

const NATIVE_SHIM_ABI_VERSION: u32 = 2;
const NATIVE_SHIM_UNKNOWN_LATENCY: u32 = u32::MAX;
const NATIVE_SHIM_SAMPLE_F32: c_int = 1;
const NATIVE_SHIM_SAMPLE_F64: c_int = 2;

type ShimAbiVersionFn = unsafe extern "C" fn() -> u32;
type ShimStringFn = unsafe extern "C" fn() -> *const c_char;
type ShimSampleFormatFn = unsafe extern "C" fn() -> c_int;
type ShimDependencyCountFn = unsafe extern "C" fn() -> u32;
type ShimDependencyPathFn = unsafe extern "C" fn(u32) -> *const c_char;
type ShimCreateFn = unsafe extern "C" fn(u32, u32, u32, u32, *mut c_int) -> *mut c_void;
type ShimDestroyFn = unsafe extern "C" fn(*mut c_void);
type ShimMaxOutputFramesFn = unsafe extern "C" fn(*mut c_void, u32) -> u32;
type ShimLatencyFramesFn = unsafe extern "C" fn(*mut c_void) -> u32;
type ShimExpectedOutputFramesFn = unsafe extern "C" fn(*mut c_void, u64) -> u64;
type ShimProcessFn = unsafe extern "C" fn(
    *mut c_void,
    *const c_void,
    u32,
    *mut c_void,
    u32,
    c_int,
    *mut u32,
    *mut u32,
    *mut c_int,
) -> c_int;
type ShimResetFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type ShimLastErrorFn = unsafe extern "C" fn(*mut c_void) -> *const c_char;

struct NativeShimLibrary {
    _library: Library,
    engine_identity: EngineIdentity,
    sample_format: SampleFormat,
    create: ShimCreateFn,
    destroy: ShimDestroyFn,
    max_output_frames: ShimMaxOutputFramesFn,
    latency_frames: ShimLatencyFramesFn,
    expected_output_frames: ShimExpectedOutputFramesFn,
    process: ShimProcessFn,
    reset: ShimResetFn,
    last_error: ShimLastErrorFn,
}

impl NativeShimLibrary {
    fn load(path: &Path, expected_engine_id: &str) -> Result<Self, String> {
        let canonical = fs::canonicalize(path).map_err(|error| {
            format!(
                "failed to canonicalize explicit {expected_engine_id} shim path '{}': {error}",
                path.display()
            )
        })?;
        let metadata = fs::metadata(&canonical).map_err(|error| {
            format!(
                "failed to inspect explicit {expected_engine_id} shim '{}': {error}",
                canonical.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "explicit {expected_engine_id} shim '{}' is not a file",
                canonical.display()
            ));
        }
        let sha256 = sha256_file(&canonical)?;
        // SAFETY: the path is explicitly selected by the benchmark caller.
        // Every ABI symbol and self-reported engine identity is validated
        // before the library is admitted into discovery.
        let library = unsafe { load_explicit_library(&canonical) }.map_err(|error| {
            format!(
                "failed to load explicit {expected_engine_id} shim '{}': {error}",
                canonical.display()
            )
        })?;

        // SAFETY: each symbol is resolved with the exact v1 shim signature,
        // copied only while `library` remains owned by this structure.
        let abi_version = unsafe {
            load_native_symbol::<ShimAbiVersionFn>(
                &library,
                b"aeb_resampler_abi_version\0",
                &canonical,
            )?
        };
        let engine_id_fn = unsafe {
            load_native_symbol::<ShimStringFn>(&library, b"aeb_resampler_engine_id\0", &canonical)?
        };
        let version_fn = unsafe {
            load_native_symbol::<ShimStringFn>(
                &library,
                b"aeb_resampler_upstream_version\0",
                &canonical,
            )?
        };
        let source_revision_fn = unsafe {
            load_native_symbol::<ShimStringFn>(
                &library,
                b"aeb_resampler_source_revision\0",
                &canonical,
            )?
        };
        let build_provenance_fn = unsafe {
            load_native_symbol::<ShimStringFn>(
                &library,
                b"aeb_resampler_build_provenance\0",
                &canonical,
            )?
        };
        let implementation_fn = unsafe {
            load_native_symbol::<ShimStringFn>(
                &library,
                b"aeb_resampler_implementation\0",
                &canonical,
            )?
        };
        let quality_recipe_fn = unsafe {
            load_native_symbol::<ShimStringFn>(
                &library,
                b"aeb_resampler_quality_recipe\0",
                &canonical,
            )?
        };
        let phase_response_fn = unsafe {
            load_native_symbol::<ShimStringFn>(
                &library,
                b"aeb_resampler_phase_response\0",
                &canonical,
            )?
        };
        let sample_format_fn = unsafe {
            load_native_symbol::<ShimSampleFormatFn>(
                &library,
                b"aeb_resampler_sample_format\0",
                &canonical,
            )?
        };
        let dependency_count = unsafe {
            load_native_symbol::<ShimDependencyCountFn>(
                &library,
                b"aeb_resampler_dependency_count\0",
                &canonical,
            )?
        };
        let dependency_path = unsafe {
            load_native_symbol::<ShimDependencyPathFn>(
                &library,
                b"aeb_resampler_dependency_path\0",
                &canonical,
            )?
        };
        let create = unsafe {
            load_native_symbol::<ShimCreateFn>(&library, b"aeb_resampler_create\0", &canonical)?
        };
        let destroy = unsafe {
            load_native_symbol::<ShimDestroyFn>(&library, b"aeb_resampler_destroy\0", &canonical)?
        };
        let max_output_frames = unsafe {
            load_native_symbol::<ShimMaxOutputFramesFn>(
                &library,
                b"aeb_resampler_max_output_frames\0",
                &canonical,
            )?
        };
        let latency_frames = unsafe {
            load_native_symbol::<ShimLatencyFramesFn>(
                &library,
                b"aeb_resampler_latency_frames\0",
                &canonical,
            )?
        };
        let expected_output_frames = unsafe {
            load_native_symbol::<ShimExpectedOutputFramesFn>(
                &library,
                b"aeb_resampler_expected_output_frames\0",
                &canonical,
            )?
        };
        let process = unsafe {
            load_native_symbol::<ShimProcessFn>(&library, b"aeb_resampler_process\0", &canonical)?
        };
        let reset = unsafe {
            load_native_symbol::<ShimResetFn>(&library, b"aeb_resampler_reset\0", &canonical)?
        };
        let last_error = unsafe {
            load_native_symbol::<ShimLastErrorFn>(
                &library,
                b"aeb_resampler_last_error\0",
                &canonical,
            )?
        };

        // SAFETY: the ABI version function takes no arguments and was resolved
        // from the loaded module using the v1 signature.
        let reported_abi = unsafe { abi_version() };
        if reported_abi != NATIVE_SHIM_ABI_VERSION {
            return Err(format!(
                "{expected_engine_id} shim '{}' reports ABI {reported_abi}, expected {NATIVE_SHIM_ABI_VERSION}",
                canonical.display()
            ));
        }
        let engine_id = read_native_string(engine_id_fn, "engine id", &canonical)?;
        if engine_id != expected_engine_id {
            return Err(format!(
                "shim '{}' reports engine id '{engine_id}', expected '{expected_engine_id}'",
                canonical.display()
            ));
        }
        let upstream_version = read_native_string(version_fn, "upstream version", &canonical)?;
        let source_revision =
            read_native_string(source_revision_fn, "source revision", &canonical)?;
        let build_provenance =
            read_native_string(build_provenance_fn, "build provenance", &canonical)?;
        let implementation = read_native_string(implementation_fn, "implementation", &canonical)?;
        let quality_recipe = read_native_string(quality_recipe_fn, "quality recipe", &canonical)?;
        let phase_response = read_native_string(phase_response_fn, "phase response", &canonical)?;
        // SAFETY: the function takes no arguments and returns the documented
        // integer sample-format tag.
        let sample_format = match unsafe { sample_format_fn() } {
            NATIVE_SHIM_SAMPLE_F32 => SampleFormat::InterleavedF32,
            NATIVE_SHIM_SAMPLE_F64 => SampleFormat::InterleavedF64,
            other => {
                return Err(format!(
                    "{engine_id} shim '{}' reports unsupported sample format {other}",
                    canonical.display()
                ));
            }
        };

        let mut linked_artifacts = Vec::new();
        // SAFETY: dependency_count and dependency_path are immutable metadata
        // queries exported by the admitted ABI version.
        let dependency_count_value = unsafe { dependency_count() };
        for index in 0..dependency_count_value {
            // SAFETY: indices below dependency_count are required to return a
            // static NUL-terminated path string.
            let pointer = unsafe { dependency_path(index) };
            let dependency = read_native_pointer_string(
                pointer,
                &format!("dependency path {index}"),
                &canonical,
            )?;
            let candidate = Path::new(&dependency);
            let resolved = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                canonical
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(candidate)
            };
            let dependency_canonical = fs::canonicalize(&resolved).map_err(|error| {
                format!(
                    "failed to canonicalize {engine_id} dependency '{}': {error}",
                    resolved.display()
                )
            })?;
            let dependency_metadata = fs::metadata(&dependency_canonical).map_err(|error| {
                format!(
                    "failed to inspect {engine_id} dependency '{}': {error}",
                    dependency_canonical.display()
                )
            })?;
            if !dependency_metadata.is_file() {
                return Err(format!(
                    "{engine_id} dependency '{}' is not a file",
                    dependency_canonical.display()
                ));
            }
            linked_artifacts.push(NativeArtifactIdentity {
                canonical_path: dependency_canonical.display().to_string(),
                sha256: sha256_file(&dependency_canonical)?,
                file_bytes: dependency_metadata.len(),
            });
        }

        let native_identity = NativeLibraryIdentity {
            canonical_path: canonical.display().to_string(),
            upstream_version: upstream_version.clone(),
            sha256,
            file_bytes: metadata.len(),
            source_revision: Some(source_revision.clone()),
            build_provenance: Some(build_provenance),
            linked_artifacts,
            provenance_verified: true,
        };
        let engine_identity = EngineIdentity {
            engine_id: engine_id.clone(),
            display_name: native_display_name(&engine_id).to_string(),
            implementation,
            upstream_version,
            adapter_schema: ADAPTER_SCHEMA.to_string(),
            algorithm_id: format!("{engine_id}_benchmark_shim_v2"),
            sample_format,
            quality_recipe,
            phase_response,
            native_library: Some(native_identity),
        };

        Ok(Self {
            _library: library,
            engine_identity,
            sample_format,
            create,
            destroy,
            max_output_frames,
            latency_frames,
            expected_output_frames,
            process,
            reset,
            last_error,
        })
    }

    fn error_message(&self, state: *mut c_void, operation: &str, code: c_int) -> String {
        // SAFETY: the state is either null or was created by this exact shim;
        // the function returns static or state-owned text valid for this call.
        let pointer = unsafe { (self.last_error)(state) };
        if pointer.is_null() {
            format!(
                "{} {operation} failed with native shim error {code}",
                self.engine_identity.engine_id
            )
        } else {
            // SAFETY: null was rejected and the shim ABI requires a
            // NUL-terminated string.
            let message = unsafe { CStr::from_ptr(pointer) }.to_string_lossy();
            format!(
                "{} {operation} failed with native shim error {code}: {message}",
                self.engine_identity.engine_id
            )
        }
    }
}

fn native_display_name(engine_id: &str) -> &str {
    match engine_id {
        "ffmpeg_libswresample" => "FFmpeg libswresample",
        "speexdsp" => "SpeexDSP quality 10",
        "r8brain" => "r8brain-free-src",
        "zita_resampler" => "zita-resampler",
        "webrtc" => "WebRTC PushResampler",
        "wdl" => "WDL Resampler",
        "libresample" => "libresample high quality",
        _ => "native benchmark shim",
    }
}

fn read_native_string(function: ShimStringFn, label: &str, path: &Path) -> Result<String, String> {
    // SAFETY: the function takes no arguments and the shim ABI requires a
    // static NUL-terminated string.
    let pointer = unsafe { function() };
    read_native_pointer_string(pointer, label, path)
}

fn read_native_pointer_string(
    pointer: *const c_char,
    label: &str,
    path: &Path,
) -> Result<String, String> {
    if pointer.is_null() {
        return Err(format!(
            "native shim '{}' returned null {label}",
            path.display()
        ));
    }
    // SAFETY: null was rejected and the ABI requires a NUL-terminated string.
    let value = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .trim()
        .to_string();
    if value.is_empty() {
        Err(format!(
            "native shim '{}' returned empty {label}",
            path.display()
        ))
    } else {
        Ok(value)
    }
}

unsafe fn load_native_symbol<T: Copy>(
    library: &Library,
    symbol: &[u8],
    path: &Path,
) -> Result<T, String> {
    // SAFETY: callers provide the exact v1 shim signature for each symbol and
    // retain the Library for longer than the copied function pointer.
    unsafe { library.get::<T>(symbol) }
        .map(|value| *value)
        .map_err(|error| {
            let printable = String::from_utf8_lossy(symbol)
                .trim_end_matches('\0')
                .to_string();
            format!(
                "native shim '{}' is missing required symbol {printable}: {error}",
                path.display()
            )
        })
}

pub(crate) struct NativeShimAdapter {
    library: Arc<NativeShimLibrary>,
    state: Option<NonNull<c_void>>,
    channels: usize,
    sample_format: SampleFormat,
    max_output_frames: usize,
    api_buffering_latency_frames: Option<usize>,
    end_signalled: bool,
    finished: bool,
}

impl NativeShimAdapter {
    fn new(
        library: Arc<NativeShimLibrary>,
        rate: RatePair,
        channels: usize,
        chunk_frames: usize,
    ) -> Result<Self, String> {
        let channels_u32 = u32::try_from(channels)
            .map_err(|_| format!("native shim channel count {channels} exceeds u32"))?;
        let chunk_frames_u32 = u32::try_from(chunk_frames)
            .map_err(|_| format!("native shim chunk size {chunk_frames} exceeds u32"))?;
        let mut error = 0;
        // SAFETY: the function belongs to this loaded shim and all scalar
        // geometry was range-checked before the call.
        let state = unsafe {
            (library.create)(
                rate.from_hz,
                rate.to_hz,
                channels_u32,
                chunk_frames_u32,
                &mut error,
            )
        };
        let Some(state) = NonNull::new(state) else {
            return Err(library.error_message(ptr::null_mut(), "create", error));
        };
        if error != 0 {
            // SAFETY: this state was just returned by the same shim.
            unsafe { (library.destroy)(state.as_ptr()) };
            return Err(library.error_message(ptr::null_mut(), "create", error));
        }
        // SAFETY: the state belongs to this shim and remains live.
        let max_output_frames =
            unsafe { (library.max_output_frames)(state.as_ptr(), chunk_frames_u32) };
        // SAFETY: the state belongs to this shim and remains live.
        let reported_latency_frames = unsafe { (library.latency_frames)(state.as_ptr()) };
        let api_buffering_latency_frames = (reported_latency_frames != NATIVE_SHIM_UNKNOWN_LATENCY)
            .then_some(reported_latency_frames as usize);
        if max_output_frames == 0 {
            // SAFETY: state is live and uniquely owned by this adapter.
            unsafe { (library.destroy)(state.as_ptr()) };
            return Err(format!(
                "{} shim reported zero output capacity",
                library.engine_identity.engine_id
            ));
        }
        Ok(Self {
            sample_format: library.sample_format,
            library,
            state: Some(state),
            channels,
            max_output_frames: max_output_frames as usize,
            api_buffering_latency_frames,
            end_signalled: false,
            finished: false,
        })
    }

    fn process_f64(
        &mut self,
        input: &[f64],
        output: &mut [f64],
        end_of_input: bool,
    ) -> Result<AdapterProgress, String> {
        if self.sample_format != SampleFormat::InterleavedF64 {
            return Err(format!(
                "{} uses interleaved_f32, not interleaved_f64",
                self.library.engine_identity.engine_id
            ));
        }
        self.process_inner(
            input.len(),
            input.as_ptr().cast(),
            output.len(),
            output.as_mut_ptr().cast(),
            end_of_input,
        )
    }

    fn process_f32(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        end_of_input: bool,
    ) -> Result<AdapterProgress, String> {
        if self.sample_format != SampleFormat::InterleavedF32 {
            return Err(format!(
                "{} uses interleaved_f64, not interleaved_f32",
                self.library.engine_identity.engine_id
            ));
        }
        self.process_inner(
            input.len(),
            input.as_ptr().cast(),
            output.len(),
            output.as_mut_ptr().cast(),
            end_of_input,
        )
    }

    fn process_inner(
        &mut self,
        input_samples: usize,
        input: *const c_void,
        output_samples: usize,
        output: *mut c_void,
        end_of_input: bool,
    ) -> Result<AdapterProgress, String> {
        validate_interleaved(input_samples, self.channels, "native shim input")?;
        validate_interleaved(output_samples, self.channels, "native shim output")?;
        if self.end_signalled || self.finished {
            return Err(format!(
                "{} received input after end-of-input",
                self.library.engine_identity.engine_id
            ));
        }
        let input_frames = input_samples / self.channels;
        let output_frames = output_samples / self.channels;
        let input_frames_u32 = u32::try_from(input_frames)
            .map_err(|_| format!("native shim input frame count {input_frames} exceeds u32"))?;
        let output_frames_u32 = u32::try_from(output_frames)
            .map_err(|_| format!("native shim output frame count {output_frames} exceeds u32"))?;
        let state = self
            .state
            .ok_or_else(|| "native shim state was already destroyed".to_string())?;
        let mut consumed = 0u32;
        let mut produced = 0u32;
        let mut finished = 0;
        // SAFETY: state belongs to this shim; sample pointers cover the
        // interleaved frame counts supplied; progress outputs are writable.
        let error = unsafe {
            (self.library.process)(
                state.as_ptr(),
                input,
                input_frames_u32,
                output,
                output_frames_u32,
                c_int::from(end_of_input),
                &mut consumed,
                &mut produced,
                &mut finished,
            )
        };
        if error != 0 {
            return Err(self.library.error_message(state.as_ptr(), "process", error));
        }
        let consumed = consumed as usize;
        let produced = produced as usize;
        validate_progress(
            &format!("{} process", self.library.engine_identity.engine_id),
            consumed,
            produced,
            input_frames,
            output_frames,
        )?;
        self.end_signalled = end_of_input;
        self.finished = finished != 0;
        Ok(AdapterProgress {
            consumed_frames: consumed,
            produced_frames: produced,
            finished: self.finished,
        })
    }

    fn drain_f64(&mut self, output: &mut [f64]) -> Result<AdapterProgress, String> {
        if self.sample_format != SampleFormat::InterleavedF64 {
            return Err(format!(
                "{} uses interleaved_f32, not interleaved_f64",
                self.library.engine_identity.engine_id
            ));
        }
        self.drain_inner(output.len(), output.as_mut_ptr().cast())
    }

    fn drain_f32(&mut self, output: &mut [f32]) -> Result<AdapterProgress, String> {
        if self.sample_format != SampleFormat::InterleavedF32 {
            return Err(format!(
                "{} uses interleaved_f64, not interleaved_f32",
                self.library.engine_identity.engine_id
            ));
        }
        self.drain_inner(output.len(), output.as_mut_ptr().cast())
    }

    fn drain_inner(
        &mut self,
        output_samples: usize,
        output: *mut c_void,
    ) -> Result<AdapterProgress, String> {
        validate_interleaved(output_samples, self.channels, "native shim drain output")?;
        if self.finished {
            return Ok(AdapterProgress {
                consumed_frames: 0,
                produced_frames: 0,
                finished: true,
            });
        }
        if !self.end_signalled {
            return Err(format!(
                "{} drain requires end-of-input on the final real input block",
                self.library.engine_identity.engine_id
            ));
        }
        let output_frames = output_samples / self.channels;
        let output_frames_u32 = u32::try_from(output_frames)
            .map_err(|_| format!("native shim output frame count {output_frames} exceeds u32"))?;
        let state = self
            .state
            .ok_or_else(|| "native shim state was already destroyed".to_string())?;
        let mut consumed = 0u32;
        let mut produced = 0u32;
        let mut finished = 0;
        // SAFETY: state belongs to this shim; a null input with zero frames is
        // the v1 drain contract; output and progress storage are writable.
        let error = unsafe {
            (self.library.process)(
                state.as_ptr(),
                ptr::null(),
                0,
                output,
                output_frames_u32,
                1,
                &mut consumed,
                &mut produced,
                &mut finished,
            )
        };
        if error != 0 {
            return Err(self.library.error_message(state.as_ptr(), "drain", error));
        }
        let consumed = consumed as usize;
        let produced = produced as usize;
        validate_progress(
            &format!("{} drain", self.library.engine_identity.engine_id),
            consumed,
            produced,
            0,
            output_frames,
        )?;
        self.finished = finished != 0;
        Ok(AdapterProgress {
            consumed_frames: consumed,
            produced_frames: produced,
            finished: self.finished,
        })
    }

    fn reset(&mut self) -> Result<(), String> {
        let state = self
            .state
            .ok_or_else(|| "native shim state was already destroyed".to_string())?;
        // SAFETY: state belongs to this shim and remains live.
        let error = unsafe { (self.library.reset)(state.as_ptr()) };
        if error != 0 {
            return Err(self.library.error_message(state.as_ptr(), "reset", error));
        }
        self.end_signalled = false;
        self.finished = false;
        Ok(())
    }

    fn expected_complete_output_frames(&self, input_frames: usize) -> usize {
        let Some(state) = self.state else {
            return 0;
        };
        let input_frames = u64::try_from(input_frames).unwrap_or(u64::MAX);
        // SAFETY: state belongs to this shim; this immutable query does not
        // change stream progress.
        let frames = unsafe { (self.library.expected_output_frames)(state.as_ptr(), input_frames) };
        usize::try_from(frames).unwrap_or(usize::MAX)
    }
}

impl Drop for NativeShimAdapter {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            // SAFETY: state was created by this exact shim and is destroyed
            // once before the Library Arc can be dropped.
            unsafe { (self.library.destroy)(state.as_ptr()) };
        }
    }
}

#[repr(C)]
struct SrcState {
    _private: [u8; 0],
}

#[repr(C)]
struct SrcData {
    data_in: *const f32,
    data_out: *mut f32,
    input_frames: c_long,
    output_frames: c_long,
    input_frames_used: c_long,
    output_frames_gen: c_long,
    end_of_input: c_int,
    src_ratio: f64,
}

type SrcNewFn = unsafe extern "C" fn(c_int, c_int, *mut c_int) -> *mut SrcState;
type SrcProcessFn = unsafe extern "C" fn(*mut SrcState, *mut SrcData) -> c_int;
type SrcResetFn = unsafe extern "C" fn(*mut SrcState) -> c_int;
type SrcDeleteFn = unsafe extern "C" fn(*mut SrcState) -> *mut SrcState;
type SrcGetVersionFn = unsafe extern "C" fn() -> *const c_char;
type SrcStrErrorFn = unsafe extern "C" fn(c_int) -> *const c_char;

struct LibSamplerateLibrary {
    _library: Library,
    src_new: SrcNewFn,
    src_process: SrcProcessFn,
    src_reset: SrcResetFn,
    src_delete: SrcDeleteFn,
    src_strerror: SrcStrErrorFn,
    native_identity: NativeLibraryIdentity,
}

impl LibSamplerateLibrary {
    fn load(path: &Path) -> Result<Self, String> {
        let canonical = fs::canonicalize(path).map_err(|error| {
            format!(
                "failed to canonicalize explicit libsamplerate path '{}': {error}",
                path.display()
            )
        })?;
        let metadata = fs::metadata(&canonical).map_err(|error| {
            format!(
                "failed to inspect explicit libsamplerate path '{}': {error}",
                canonical.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "explicit libsamplerate path '{}' is not a file",
                canonical.display()
            ));
        }
        let sha256 = sha256_file(&canonical)?;
        // SAFETY: the caller explicitly selected this path.  Every required
        // symbol is resolved before the Library is admitted into a factory,
        // and the Library is retained for longer than all copied function
        // pointers and every state created from it.
        let library = unsafe { load_explicit_library(&canonical) }.map_err(|error| {
            format!(
                "failed to load explicit libsamplerate library '{}': {error}",
                canonical.display()
            )
        })?;
        // SAFETY: symbol names and signatures match samplerate.h's stable C
        // API.  `load_symbol` copies function pointers while `library` stays
        // owned by this structure.
        let src_new = unsafe { load_symbol::<SrcNewFn>(&library, b"src_new\0", &canonical)? };
        let src_process =
            unsafe { load_symbol::<SrcProcessFn>(&library, b"src_process\0", &canonical)? };
        let src_reset = unsafe { load_symbol::<SrcResetFn>(&library, b"src_reset\0", &canonical)? };
        let src_delete =
            unsafe { load_symbol::<SrcDeleteFn>(&library, b"src_delete\0", &canonical)? };
        let src_get_version =
            unsafe { load_symbol::<SrcGetVersionFn>(&library, b"src_get_version\0", &canonical)? };
        let src_strerror =
            unsafe { load_symbol::<SrcStrErrorFn>(&library, b"src_strerror\0", &canonical)? };
        // SAFETY: a successfully resolved src_get_version returns a static C
        // string for the lifetime of the loaded library.
        let version_ptr = unsafe { src_get_version() };
        if version_ptr.is_null() {
            return Err(format!(
                "libsamplerate '{}' returned a null version string",
                canonical.display()
            ));
        }
        // SAFETY: null was rejected and samplerate.h specifies a NUL-terminated
        // static string.
        let upstream_version = unsafe { CStr::from_ptr(version_ptr) }
            .to_string_lossy()
            .trim()
            .to_string();
        if upstream_version.is_empty() {
            return Err(format!(
                "libsamplerate '{}' returned an empty version string",
                canonical.display()
            ));
        }
        let pinned_payload = sha256.eq_ignore_ascii_case(PINNED_LIBSAMPLERATE_DLL_SHA256);
        let (source_revision, build_provenance) = if pinned_payload {
            (
                Some(format!(
                    "MSYS2 mingw-w64-x86_64-libsamplerate 0.2.2-1; package-sha256={PINNED_LIBSAMPLERATE_PACKAGE_SHA256}"
                )),
                Some(format!(
                    "loaded DLL matches pinned MSYS2 package payload sha256={PINNED_LIBSAMPLERATE_DLL_SHA256}"
                )),
            )
        } else {
            (
                None,
                Some(format!(
                    "caller-provided DLL; payload sha256={sha256}; provenance not verified"
                )),
            )
        };
        Ok(Self {
            _library: library,
            src_new,
            src_process,
            src_reset,
            src_delete,
            src_strerror,
            native_identity: NativeLibraryIdentity {
                canonical_path: canonical.display().to_string(),
                upstream_version,
                sha256,
                file_bytes: metadata.len(),
                source_revision,
                build_provenance,
                linked_artifacts: Vec::new(),
                provenance_verified: pinned_payload,
            },
        })
    }

    fn engine_identity(&self) -> EngineIdentity {
        EngineIdentity {
            engine_id: LIBSAMPLERATE_ENGINE_ID.to_string(),
            display_name: "libsamplerate SINC_BEST_QUALITY".to_string(),
            implementation: "runtime-loaded libsamplerate streaming C API".to_string(),
            upstream_version: self.native_identity.upstream_version.clone(),
            adapter_schema: ADAPTER_SCHEMA.to_string(),
            algorithm_id: "libsamplerate_sinc_best_quality_streaming_f32_v3".to_string(),
            sample_format: SampleFormat::InterleavedF32,
            quality_recipe:
                "SRC_SINC_BEST_QUALITY (converter type 0); final real input uses end_of_input=1; complete length uses ceil(input_frames * ratio)"
                    .to_string(),
            phase_response: "library-defined linear-phase sinc; latency not exposed by API"
                .to_string(),
            native_library: Some(self.native_identity.clone()),
        }
    }

    fn error_message(&self, operation: &str, code: c_int) -> String {
        // SAFETY: src_strerror is a resolved stable API symbol and accepts any
        // libsamplerate error code.  A defensive null check handles malformed
        // libraries without dereferencing their output.
        let pointer = unsafe { (self.src_strerror)(code) };
        if pointer.is_null() {
            format!("{operation} failed with libsamplerate error {code}")
        } else {
            // SAFETY: samplerate.h specifies a static NUL-terminated string.
            let message = unsafe { CStr::from_ptr(pointer) }.to_string_lossy();
            format!("{operation} failed with libsamplerate error {code}: {message}")
        }
    }
}

unsafe fn load_symbol<T: Copy>(library: &Library, symbol: &[u8], path: &Path) -> Result<T, String> {
    // SAFETY: callers supply the exact samplerate.h signature for each named
    // function, and the Library outlives the returned copied function pointer.
    unsafe { library.get::<T>(symbol) }
        .map(|value| *value)
        .map_err(|error| {
            let printable = String::from_utf8_lossy(symbol)
                .trim_end_matches('\0')
                .to_string();
            format!(
                "libsamplerate '{}' is missing required symbol {printable}: {error}",
                path.display()
            )
        })
}

pub(crate) struct LibSamplerateAdapter {
    library: Arc<LibSamplerateLibrary>,
    state: Option<NonNull<SrcState>>,
    channels: usize,
    from_hz: u32,
    to_hz: u32,
    ratio: f64,
    max_output_frames: usize,
    api_buffering_latency_frames: Option<usize>,
    end_signalled: bool,
    draining: bool,
    finished: bool,
}

impl LibSamplerateAdapter {
    fn new(
        library: Arc<LibSamplerateLibrary>,
        rate: RatePair,
        channels: usize,
        chunk_frames: usize,
    ) -> Result<Self, String> {
        let channels_c = c_int::try_from(channels)
            .map_err(|_| format!("libsamplerate channel count {channels} exceeds c_int"))?;
        let mut error = 0;
        // SAFETY: function pointer was validated at library load; arguments
        // follow samplerate.h and `error` is valid writable storage.
        let state =
            unsafe { (library.src_new)(LIBSAMPLERATE_SINC_BEST_QUALITY, channels_c, &mut error) };
        let Some(state) = NonNull::new(state) else {
            return Err(library.error_message("src_new", error));
        };
        if error != 0 {
            // SAFETY: state was just returned by src_new from this library.
            unsafe { (library.src_delete)(state.as_ptr()) };
            return Err(library.error_message("src_new", error));
        }
        Ok(Self {
            library,
            state: Some(state),
            channels,
            from_hz: rate.from_hz,
            to_hz: rate.to_hz,
            ratio: rate.to_hz as f64 / rate.from_hz as f64,
            max_output_frames: generous_output_capacity(chunk_frames, rate),
            api_buffering_latency_frames: None,
            end_signalled: false,
            draining: false,
            finished: false,
        })
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) -> Result<AdapterProgress, String> {
        if self.draining || self.finished {
            return Err("libsamplerate received input after drain started".to_string());
        }
        let progress = self.process_inner(input, output, false)?;
        if !input.is_empty() && progress.consumed_frames == 0 && progress.produced_frames == 0 {
            return Err("libsamplerate process made no progress".to_string());
        }
        Ok(progress)
    }

    fn process_final(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<AdapterProgress, String> {
        if self.draining || self.finished {
            return Err("libsamplerate received final input after drain started".to_string());
        }
        if input.is_empty() {
            return Err("libsamplerate final input must not be empty".to_string());
        }
        let progress = self.process_inner(input, output, true)?;
        let input_frames = input.len() / self.channels;
        if progress.consumed_frames != input_frames {
            return Err(format!(
                "libsamplerate final input consumed {} of {input_frames} frames",
                progress.consumed_frames
            ));
        }
        self.end_signalled = true;
        self.draining = true;
        Ok(progress)
    }

    fn drain(&mut self, output: &mut [f32]) -> Result<AdapterProgress, String> {
        if self.finished {
            return Ok(AdapterProgress {
                consumed_frames: 0,
                produced_frames: 0,
                finished: true,
            });
        }
        if !self.end_signalled {
            return Err(
                "libsamplerate drain requires end_of_input on the final real input block"
                    .to_string(),
            );
        }
        let mut progress = self.process_inner(&[], output, true)?;
        self.finished = progress.produced_frames == 0;
        progress.finished = self.finished;
        Ok(progress)
    }

    fn process_inner(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        end_of_input: bool,
    ) -> Result<AdapterProgress, String> {
        validate_interleaved(input.len(), self.channels, "libsamplerate input")?;
        validate_interleaved(output.len(), self.channels, "libsamplerate output")?;
        let input_frames = input.len() / self.channels;
        let output_frames = output.len() / self.channels;
        let input_frames_c = c_long::try_from(input_frames).map_err(|_| {
            format!("libsamplerate input frame count {input_frames} exceeds c_long")
        })?;
        let output_frames_c = c_long::try_from(output_frames).map_err(|_| {
            format!("libsamplerate output frame count {output_frames} exceeds c_long")
        })?;
        let mut data = SrcData {
            data_in: if input.is_empty() {
                ptr::null()
            } else {
                input.as_ptr()
            },
            data_out: output.as_mut_ptr(),
            input_frames: input_frames_c,
            output_frames: output_frames_c,
            input_frames_used: 0,
            output_frames_gen: 0,
            end_of_input: c_int::from(end_of_input),
            src_ratio: self.ratio,
        };
        let state = self
            .state
            .ok_or_else(|| "libsamplerate state was already deleted".to_string())?;
        // SAFETY: state belongs to this loaded library, SrcData matches
        // samplerate.h, and both sample slices stay alive and exclusively
        // borrowed for the duration of the call.
        let error = unsafe { (self.library.src_process)(state.as_ptr(), &mut data) };
        if error != 0 {
            return Err(self.library.error_message("src_process", error));
        }
        let consumed = checked_native_frames(
            "libsamplerate input_frames_used",
            data.input_frames_used,
            input_frames,
        )?;
        let produced = checked_native_frames(
            "libsamplerate output_frames_gen",
            data.output_frames_gen,
            output_frames,
        )?;
        Ok(AdapterProgress {
            consumed_frames: consumed,
            produced_frames: produced,
            finished: false,
        })
    }

    fn reset(&mut self) -> Result<(), String> {
        let state = self
            .state
            .ok_or_else(|| "libsamplerate state was already deleted".to_string())?;
        // SAFETY: state belongs to this loaded library and remains live.
        let error = unsafe { (self.library.src_reset)(state.as_ptr()) };
        if error != 0 {
            return Err(self.library.error_message("src_reset", error));
        }
        self.end_signalled = false;
        self.draining = false;
        self.finished = false;
        Ok(())
    }

    fn expected_complete_output_frames(&self, input_frames: usize) -> usize {
        libsamplerate_complete_output_frames(input_frames, self.from_hz, self.to_hz)
    }
}

impl Drop for LibSamplerateAdapter {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            // SAFETY: state was created by this exact loaded library and is
            // deleted once before the Library's Arc can be dropped.
            unsafe { (self.library.src_delete)(state.as_ptr()) };
        }
    }
}

fn validate_interleaved(samples: usize, channels: usize, label: &str) -> Result<(), String> {
    if channels == 0 {
        return Err(format!("{label} has zero channels"));
    }
    if !samples.is_multiple_of(channels) {
        return Err(format!(
            "{label} has {samples} samples, not divisible by {channels} channels"
        ));
    }
    Ok(())
}

fn validate_progress(
    label: &str,
    consumed: usize,
    produced: usize,
    input_capacity: usize,
    output_capacity: usize,
) -> Result<(), String> {
    if consumed > input_capacity || produced > output_capacity {
        Err(format!(
            "{label} returned consumed={consumed}/{input_capacity}, produced={produced}/{output_capacity}"
        ))
    } else {
        Ok(())
    }
}

fn checked_native_frames(label: &str, frames: c_long, capacity: usize) -> Result<usize, String> {
    let frames = usize::try_from(frames)
        .map_err(|_| format!("{label} returned negative or unrepresentable value {frames}"))?;
    if frames > capacity {
        Err(format!(
            "{label} returned {frames} beyond capacity {capacity}"
        ))
    } else {
        Ok(frames)
    }
}

fn generous_output_capacity(chunk_frames: usize, rate: RatePair) -> usize {
    let nominal = div_ceil(
        chunk_frames.saturating_mul(rate.to_hz as usize),
        rate.from_hz as usize,
    );
    nominal.saturating_add(chunk_frames.max(512)).max(1)
}

fn div_ceil(value: usize, divisor: usize) -> usize {
    if divisor == 0 {
        0
    } else {
        value / divisor + usize::from(!value.is_multiple_of(divisor))
    }
}

fn libsamplerate_complete_output_frames(input_frames: usize, from_hz: u32, to_hz: u32) -> usize {
    if from_hz == 0 {
        return 0;
    }
    let numerator = (input_frames as u128) * (to_hz as u128);
    let frames = numerator.div_ceil(from_hz as u128);
    usize::try_from(frames).unwrap_or(usize::MAX)
}

#[cfg(feature = "soxr")]
fn stereo_frames<'a>(samples: &'a [f64], label: &str) -> Result<&'a [[f64; 2]], String> {
    validate_interleaved(samples.len(), 2, label)?;
    // SAFETY: `[f64; 2]` has the same alignment and contiguous element layout
    // as two adjacent f64 values, and divisibility by two was checked above.
    Ok(unsafe { std::slice::from_raw_parts(samples.as_ptr().cast(), samples.len() / 2) })
}

#[cfg(feature = "soxr")]
fn stereo_frames_mut<'a>(
    samples: &'a mut [f64],
    label: &str,
) -> Result<&'a mut [[f64; 2]], String> {
    validate_interleaved(samples.len(), 2, label)?;
    // SAFETY: `[f64; 2]` has the same alignment and contiguous element layout
    // as two adjacent f64 values, the mutable slice is uniquely borrowed, and
    // divisibility by two was checked above.
    Ok(unsafe { std::slice::from_raw_parts_mut(samples.as_mut_ptr().cast(), samples.len() / 2) })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("failed to hash '{}': {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("failed to hash '{}': {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

unsafe fn load_explicit_library(path: &Path) -> Result<Library, libloading::Error> {
    #[cfg(windows)]
    {
        use libloading::os::windows::{
            Library as WindowsLibrary, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        };

        // SAFETY: callers explicitly select and hash the library path. The
        // DLL-load-directory flag ensures its adjacent, recorded dependencies
        // win over unrelated copies beside the Rust benchmark executable.
        unsafe {
            WindowsLibrary::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map(Into::into)
    }
    #[cfg(not(windows))]
    {
        // SAFETY: callers explicitly select and hash the library path.
        unsafe { Library::new(path) }
    }
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use std::env;

    use super::*;

    const NATIVE_EVIDENCE_DIRECTORY: &str = "AUDIO_BENCH_NATIVE_SHIM_TEST_DIR";

    #[test]
    fn missing_libsamplerate_path_is_visible_and_preserves_required_flag() {
        let required = BTreeSet::from([LIBSAMPLERATE_ENGINE_ID.to_string()]);
        let discovery = discover(
            None,
            &BTreeMap::new(),
            &required,
            #[cfg(feature = "rubato")]
            RawRubatoGeometry::FFT_512_1,
        );
        let unavailable = discovery
            .unavailable
            .iter()
            .find(|entry| entry.engine_id == LIBSAMPLERATE_ENGINE_ID)
            .expect("missing libsamplerate must be reported");
        assert_eq!(unavailable.classification, MetricClassification::Skipped);
        assert!(unavailable.required);
        assert!(unavailable.reason.contains("--libsamplerate"));
    }

    #[test]
    fn native_progress_rejects_negative_and_over_capacity_values() {
        assert!(checked_native_frames("used", -1, 8).is_err());
        assert!(checked_native_frames("used", 9, 8).is_err());
        assert_eq!(checked_native_frames("used", 8, 8).unwrap(), 8);
    }

    #[test]
    fn libsamplerate_complete_output_uses_duration_ceiling() {
        assert_eq!(
            libsamplerate_complete_output_frames(2_560, 44_100, 48_000),
            2_787
        );
        assert_eq!(
            libsamplerate_complete_output_frames(16_384, 44_100, 48_000),
            17_833
        );
        assert_eq!(
            libsamplerate_complete_output_frames(2_560, 48_000, 44_100),
            2_352
        );
    }

    #[test]
    fn discovered_engine_ids_are_unique() {
        let discovery = discover(
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            #[cfg(feature = "rubato")]
            RawRubatoGeometry::FFT_512_1,
        );
        let ids = discovery
            .factories
            .iter()
            .map(|factory| factory.identity().engine_id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), discovery.factories.len());
        assert!(ids.contains(PROJECT_ENGINE_ID));
        for engine_id in NATIVE_SHIM_ENGINE_IDS {
            assert!(
                discovery
                    .unavailable
                    .iter()
                    .any(|entry| entry.engine_id == engine_id),
                "missing native shim must remain visible for {engine_id}"
            );
        }
    }

    #[test]
    fn canonical_create_probe_failure_becomes_unavailable() {
        let factory = EngineFactory::create_failure_test();
        let engine_id = factory.identity().engine_id.clone();
        let required = BTreeSet::from([engine_id.clone()]);
        let discovery = probe_canonical_factories(vec![factory], Vec::new(), &required);

        assert!(discovery.factories.is_empty());
        assert_eq!(discovery.unavailable.len(), 1);
        let unavailable = &discovery.unavailable[0];
        assert_eq!(unavailable.engine_id, engine_id);
        assert!(unavailable.required);
        assert!(
            unavailable
                .reason
                .contains("music_44k1_to_48k canonical create probe failed"),
            "{}",
            unavailable.reason
        );
    }

    #[cfg(feature = "soxr")]
    #[test]
    fn raw_soxr_consumes_a_complete_canonical_chunk_and_resets() {
        let rate = RatePair {
            id: "test",
            from_hz: 44_100,
            to_hz: 48_000,
        };
        let mut adapter = RawSoxrAdapter::new(rate, 2, 512).unwrap();
        let input = vec![0.0; 512 * 2];
        let mut output = vec![0.0; adapter.max_output_frames * 2];
        let progress = adapter.process(&input, &mut output).unwrap();
        assert_eq!(progress.consumed_frames, 512);
        assert!(progress.produced_frames <= adapter.max_output_frames);
        adapter.reset().unwrap();
    }

    #[cfg(feature = "rubato")]
    #[test]
    fn raw_rubato_compensates_native_delay_and_uses_exact_duration() {
        let rate = RatePair {
            id: "test",
            from_hz: 44_100,
            to_hz: 48_000,
        };
        let adapter = RawRubatoAdapter::new(rate, 2, 512, RawRubatoGeometry::FFT_512_1).unwrap();
        let latency = adapter
            .api_buffering_latency_frames
            .expect("strict raw Rubato control exposes compensated API latency");
        assert_eq!(latency, 0);
        assert!(adapter.initial_output_delay_frames > 0);
        assert_eq!(adapter.expected_complete_output_frames(44_100), 48_000);
    }

    #[cfg(feature = "rubato")]
    #[test]
    fn raw_rubato_subchunk1_drain_advances_across_zero_output_states() {
        let rate = RatePair {
            id: "test",
            from_hz: 44_100,
            to_hz: 48_000,
        };
        let mut adapter =
            RawRubatoAdapter::new(rate, 2, 512, RawRubatoGeometry::FFT_512_1).unwrap();
        let input = vec![0.0; 512 * 2];
        let mut output = vec![0.0; adapter.max_output_frames * 2];
        for _ in 0..253 {
            let progress = adapter.process(&input, &mut output).unwrap();
            assert_eq!(progress.consumed_frames, 512);
        }
        let expected = adapter.expected_complete_output_frames(253 * 512);

        for call in 0..32 {
            let progress = adapter.drain(&mut output).unwrap();
            assert!(progress.produced_frames > 0 || progress.finished);
            if progress.finished {
                assert_eq!(adapter.emitted_frames, expected);
                return;
            }
            assert!(call < 31, "raw Rubato drain did not terminate");
        }
        unreachable!("bounded raw Rubato drain loop must return");
    }

    #[cfg(all(feature = "rubato", not(feature = "soxr")))]
    #[test]
    fn strict_raw_rubato_1024_matches_production_complete_stream_bit_exactly() {
        let project_factory = EngineFactory {
            identity: project_identity(),
            kind: EngineFactoryKind::Project,
        };
        let raw_factory = EngineFactory {
            identity: raw_rubato_identity(RawRubatoGeometry::FFT_1024_2),
            kind: EngineFactoryKind::RawRubato(RawRubatoGeometry::FFT_1024_2),
        };

        for rate in RATE_PAIRS {
            let mut project = project_factory.create(rate, 2, 512).unwrap();
            let mut raw = raw_factory.create(rate, 2, 512).unwrap();
            let project_output = drive_native_test_stream(&project_factory, &mut project, 4_097);
            let raw_output = drive_native_test_stream(&raw_factory, &mut raw, 4_097);
            assert_native_streams_equal(
                &project_output,
                &raw_output,
                "strict_raw_rubato_1024_vs_production",
                rate.id,
            );
        }
    }

    #[cfg(feature = "rubato")]
    #[test]
    fn strict_raw_rubato_1024_stages_512_callers_and_resets_fresh() {
        let factory = EngineFactory {
            identity: raw_rubato_identity(RawRubatoGeometry::FFT_1024_2),
            kind: EngineFactoryKind::RawRubato(RawRubatoGeometry::FFT_1024_2),
        };
        for rate in RATE_PAIRS {
            let mut adapter = factory.create(rate, 2, 512).unwrap();
            let first = drive_native_test_stream(&factory, &mut adapter, 4_097);
            adapter.reset().unwrap();
            let second = drive_native_test_stream(&factory, &mut adapter, 4_097);
            let mut fresh = factory.create(rate, 2, 512).unwrap();
            let fresh_output = drive_native_test_stream(&factory, &mut fresh, 4_097);
            assert_native_streams_equal(&first, &second, "raw_rubato_1024_reset", rate.id);
            assert_native_streams_equal(&second, &fresh_output, "raw_rubato_1024_fresh", rate.id);
        }
    }

    #[test]
    #[ignore = "requires shims built by benches/native/build_resampler_shims.ps1"]
    fn native_shim_evidence_exercises_abi_metadata_progress_reset_and_drain() {
        let directory = env::var_os(NATIVE_EVIDENCE_DIRECTORY)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                panic!(
                    "set {NATIVE_EVIDENCE_DIRECTORY} to the directory containing all native shims"
                )
            });
        let filenames = [
            ("ffmpeg_libswresample", "ffmpeg_libswresample_shim.dll"),
            ("speexdsp", "speexdsp_shim.dll"),
            ("r8brain", "r8brain_shim.dll"),
            ("zita_resampler", "zita_resampler_shim.dll"),
            ("webrtc", "webrtc_shim.dll"),
            ("wdl", "wdl_shim.dll"),
            ("libresample", "libresample_shim.dll"),
        ];
        let native_paths = filenames
            .into_iter()
            .map(|(engine, filename)| (engine.to_string(), directory.join(filename)))
            .collect::<BTreeMap<_, _>>();
        let required = NATIVE_SHIM_ENGINE_IDS
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let discovery = discover(
            None,
            &native_paths,
            &required,
            #[cfg(feature = "rubato")]
            RawRubatoGeometry::FFT_512_1,
        );

        let unavailable_native = discovery
            .unavailable
            .iter()
            .filter(|entry| NATIVE_SHIM_ENGINE_IDS.contains(&entry.engine_id.as_str()))
            .collect::<Vec<_>>();
        assert!(
            unavailable_native.is_empty(),
            "required native shims were unavailable: {unavailable_native:#?}"
        );
        let native_factories = discovery
            .factories
            .iter()
            .filter(|factory| {
                NATIVE_SHIM_ENGINE_IDS.contains(&factory.identity().engine_id.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(native_factories.len(), NATIVE_SHIM_ENGINE_IDS.len());

        let rates = [
            RatePair {
                id: "native_44k1_to_48k",
                from_hz: 44_100,
                to_hz: 48_000,
            },
            RatePair {
                id: "native_48k_to_44k1",
                from_hz: 48_000,
                to_hz: 44_100,
            },
        ];
        for factory in native_factories {
            assert_native_identity(factory.identity());
            for rate in rates {
                let mut adapter = factory.create(rate, 2, 512).unwrap_or_else(|error| {
                    panic!(
                        "{} failed to create for {}: {error}",
                        factory.identity().engine_id,
                        rate.id
                    )
                });
                let first = drive_native_test_stream(factory, &mut adapter, 4_097);
                adapter.reset().unwrap_or_else(|error| {
                    panic!("{} reset failed: {error}", factory.identity().engine_id)
                });
                let second = drive_native_test_stream(factory, &mut adapter, 4_097);
                assert_native_streams_equal(
                    &first,
                    &second,
                    &factory.identity().engine_id,
                    rate.id,
                );
            }
        }
    }

    fn assert_native_identity(identity: &EngineIdentity) {
        assert!(!identity.implementation.trim().is_empty());
        assert!(!identity.upstream_version.trim().is_empty());
        assert!(!identity.quality_recipe.trim().is_empty());
        assert!(!identity.phase_response.trim().is_empty());
        assert_eq!(identity.adapter_schema, ADAPTER_SCHEMA);
        let native = identity
            .native_library
            .as_ref()
            .expect("native shim identity must include its loaded library");
        assert!(std::path::Path::new(&native.canonical_path).is_absolute());
        assert_eq!(native.sha256.len(), 64);
        assert!(native.file_bytes > 0);
        assert!(native
            .source_revision
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()));
        assert!(native
            .build_provenance
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()));
        assert!(!native.linked_artifacts.is_empty());
        for artifact in &native.linked_artifacts {
            assert!(std::path::Path::new(&artifact.canonical_path).is_absolute());
            assert_eq!(artifact.sha256.len(), 64);
            assert!(artifact.file_bytes > 0);
        }
    }

    fn assert_native_streams_equal(first: &[f64], second: &[f64], engine_id: &str, rate_id: &str) {
        assert_eq!(
            first.len(),
            second.len(),
            "{engine_id} reset changed the complete sample count for {rate_id}"
        );
        if let Some((sample_index, (first_sample, second_sample))) = first
            .iter()
            .zip(second)
            .enumerate()
            .find(|(_, (first_sample, second_sample))| first_sample != second_sample)
        {
            panic!(
                "{engine_id} reset did not reproduce the fresh stream for {rate_id}: \
                 first difference at sample {sample_index} (frame {}, channel {}): \
                 first={first_sample:?}, second={second_sample:?}",
                sample_index / 2,
                sample_index % 2,
            );
        }
    }

    fn drive_native_test_stream(
        factory: &EngineFactory,
        adapter: &mut EngineAdapter,
        input_frames: usize,
    ) -> Vec<f64> {
        const CHANNELS: usize = 2;
        const CHUNKS: [usize; 6] = [511, 7, 193, 512, 1, 257];

        let input_f64 = (0..input_frames)
            .flat_map(|frame| {
                let sample = ((frame as f64) * 0.013).sin() * 0.25 + f64::from(frame == 37) * 0.5;
                [sample, sample * -0.5]
            })
            .collect::<Vec<_>>();
        let input_f32 = input_f64
            .iter()
            .map(|sample| *sample as f32)
            .collect::<Vec<_>>();
        let output_capacity = adapter.max_output_frames().max(1);
        let mut output_f64 = vec![0.0; output_capacity * CHANNELS];
        let mut output_f32 = vec![0.0; output_capacity * CHANNELS];
        let mut rendered = Vec::new();
        let mut cursor = 0usize;
        let mut chunk_index = 0usize;

        while cursor < input_frames {
            let supplied = CHUNKS[chunk_index % CHUNKS.len()]
                .min(input_frames - cursor)
                .max(1);
            let end = cursor + supplied;
            let progress = match adapter.sample_format() {
                SampleFormat::InterleavedF64 => {
                    let input = &input_f64[cursor * CHANNELS..end * CHANNELS];
                    if end == input_frames {
                        adapter.process_final_f64(input, &mut output_f64)
                    } else {
                        adapter.process_f64(input, &mut output_f64)
                    }
                }
                SampleFormat::InterleavedF32 => {
                    let input = &input_f32[cursor * CHANNELS..end * CHANNELS];
                    if end == input_frames {
                        adapter.process_final_f32(input, &mut output_f32)
                    } else {
                        adapter.process_f32(input, &mut output_f32)
                    }
                }
            }
            .unwrap_or_else(|error| {
                panic!(
                    "{} process failed at {cursor}/{input_frames}: {error}",
                    factory.identity().engine_id
                )
            });
            assert!(progress.consumed_frames <= supplied);
            assert!(progress.produced_frames <= output_capacity);
            assert!(!progress.finished);
            assert!(progress.consumed_frames > 0 || progress.produced_frames > 0);
            append_native_test_output(
                adapter.sample_format(),
                progress.produced_frames,
                &output_f64,
                &output_f32,
                &mut rendered,
            );
            cursor += progress.consumed_frames;
            chunk_index += 1;
        }

        let mut terminal = false;
        for _ in 0..64 {
            let progress = match adapter.sample_format() {
                SampleFormat::InterleavedF64 => adapter.drain_f64(&mut output_f64),
                SampleFormat::InterleavedF32 => adapter.drain_f32(&mut output_f32),
            }
            .unwrap_or_else(|error| {
                panic!("{} drain failed: {error}", factory.identity().engine_id)
            });
            assert_eq!(progress.consumed_frames, 0);
            assert!(progress.produced_frames <= output_capacity);
            append_native_test_output(
                adapter.sample_format(),
                progress.produced_frames,
                &output_f64,
                &output_f32,
                &mut rendered,
            );
            if progress.finished {
                terminal = true;
                break;
            }
            assert!(progress.produced_frames > 0, "native drain stalled");
        }
        assert!(
            terminal,
            "{} did not reach terminal drain",
            factory.identity().engine_id
        );
        assert_eq!(
            rendered.len() / CHANNELS,
            adapter.expected_complete_output_frames(input_frames),
            "{} produced the wrong complete length",
            factory.identity().engine_id
        );
        assert!(rendered.iter().all(|sample| sample.is_finite()));

        let repeated = match adapter.sample_format() {
            SampleFormat::InterleavedF64 => adapter.drain_f64(&mut output_f64),
            SampleFormat::InterleavedF32 => adapter.drain_f32(&mut output_f32),
        }
        .unwrap();
        assert!(repeated.finished);
        assert_eq!(repeated.consumed_frames, 0);
        assert_eq!(repeated.produced_frames, 0);
        rendered
    }

    fn append_native_test_output(
        format: SampleFormat,
        produced_frames: usize,
        output_f64: &[f64],
        output_f32: &[f32],
        rendered: &mut Vec<f64>,
    ) {
        let samples = produced_frames * 2;
        match format {
            SampleFormat::InterleavedF64 => rendered.extend_from_slice(&output_f64[..samples]),
            SampleFormat::InterleavedF32 => {
                rendered.extend(
                    output_f32[..samples]
                        .iter()
                        .map(|sample| f64::from(*sample)),
                );
            }
        }
    }
}
