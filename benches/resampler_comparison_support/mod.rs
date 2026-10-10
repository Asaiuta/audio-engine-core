//! Cross-project resampler benchmark plumbing.
//!
//! This module deliberately stays under `benches/`: external adapters, native
//! library loading, and report types are evidence tooling rather than public
//! audio-engine API.

mod adapters;
mod diagnostics;
mod latency;
mod measurement;
mod quality;
mod quality_grid;
mod resources;
mod signal;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::support::{
    compare_case_medians, environment_json, generated_unix_ms, parse_pinned_probe_args,
    pin_current_thread, read_json, regression_gate_error, validate_performance_baseline,
    write_json, BenchEnvironment, BenchMode, PerfArgs, PerformanceReportIdentity,
    PinnedSchedulingState, RegressionComparison, TrialDistribution, REPORT_SCHEMA_VERSION,
};

use self::adapters::{Discovery, EngineAdapter, EngineFactory};
use self::measurement::{
    collect_scheduled_trials, validate_case_trials, CaseAccumulator, MeasurementIdentity,
    TrialMeasurement, TrialRecord,
};

const PROBE: &str = "audio_resampler_comparison_perf";
const ADAPTER_SCHEMA: &str = "cross_project_resampler_v7_identified_trials_quality_grid";
const CHUNK_FRAMES: usize = 512;
const WARMUP_BUFFERS: usize = 32;
const QUALITY_INPUT_FRAMES: usize = 16_384;
const MAX_DRAIN_CALLS: usize = 4_096;

pub(crate) const PROJECT_ENGINE_ID: &str = "audio_engine_core";
pub(crate) const RAW_SOXR_ENGINE_ID: &str = "raw_libsoxr";
pub(crate) const RAW_RUBATO_ENGINE_ID: &str = "raw_rubato";
pub(crate) const LIBSAMPLERATE_ENGINE_ID: &str = "libsamplerate";
pub(crate) const FFMPEG_SWRESAMPLE_ENGINE_ID: &str = "ffmpeg_libswresample";
pub(crate) const SPEEXDSP_ENGINE_ID: &str = "speexdsp";
pub(crate) const R8BRAIN_ENGINE_ID: &str = "r8brain";
pub(crate) const ZITA_RESAMPLER_ENGINE_ID: &str = "zita_resampler";
pub(crate) const WEBRTC_ENGINE_ID: &str = "webrtc";
pub(crate) const WDL_ENGINE_ID: &str = "wdl";
pub(crate) const LIBRESAMPLE_ENGINE_ID: &str = "libresample";

const ALL_ENGINE_IDS: [&str; 11] = [
    PROJECT_ENGINE_ID,
    RAW_SOXR_ENGINE_ID,
    RAW_RUBATO_ENGINE_ID,
    LIBSAMPLERATE_ENGINE_ID,
    FFMPEG_SWRESAMPLE_ENGINE_ID,
    SPEEXDSP_ENGINE_ID,
    R8BRAIN_ENGINE_ID,
    ZITA_RESAMPLER_ENGINE_ID,
    WEBRTC_ENGINE_ID,
    WDL_ENGINE_ID,
    LIBRESAMPLE_ENGINE_ID,
];

const NATIVE_SHIM_ENGINE_IDS: [&str; 7] = [
    FFMPEG_SWRESAMPLE_ENGINE_ID,
    SPEEXDSP_ENGINE_ID,
    R8BRAIN_ENGINE_ID,
    ZITA_RESAMPLER_ENGINE_ID,
    WEBRTC_ENGINE_ID,
    WDL_ENGINE_ID,
    LIBRESAMPLE_ENGINE_ID,
];

#[derive(Clone, Copy, Debug)]
pub(crate) struct RatePair {
    pub(crate) id: &'static str,
    pub(crate) from_hz: u32,
    pub(crate) to_hz: u32,
    pub(crate) channels: usize,
}

const RATE_PAIRS: [RatePair; 5] = [
    RatePair {
        id: "music_44k1_to_48k",
        from_hz: 44_100,
        to_hz: 48_000,
        channels: 2,
    },
    RatePair {
        id: "music_48k_to_44k1",
        from_hz: 48_000,
        to_hz: 44_100,
        channels: 2,
    },
    RatePair {
        id: "music_44k1_to_96k",
        from_hz: 44_100,
        to_hz: 96_000,
        channels: 2,
    },
    RatePair {
        id: "music_96k_to_44k1",
        from_hz: 96_000,
        to_hz: 44_100,
        channels: 2,
    },
    RatePair {
        id: "music_44k1_to_48k_6ch",
        from_hz: 44_100,
        to_hz: 48_000,
        channels: 6,
    },
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SampleFormat {
    InterleavedF32,
    InterleavedF64,
}

impl SampleFormat {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::InterleavedF32 => "interleaved_f32",
            Self::InterleavedF64 => "interleaved_f64",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricClassification {
    Gate,
    Report,
    Skipped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CoverageState {
    Measured,
    NotComparable,
    InfeasibleWithEvidence,
    Unavailable,
}

impl CoverageState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::NotComparable => "not_comparable",
            Self::InfeasibleWithEvidence => "infeasible_with_evidence",
            Self::Unavailable => "unavailable",
        }
    }

    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Measured | Self::NotComparable | Self::InfeasibleWithEvidence
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EngineCoverage {
    pub(crate) engine_id: String,
    pub(crate) state: CoverageState,
    pub(crate) terminal: bool,
    pub(crate) measured_rate_pairs: Vec<String>,
    pub(crate) case_keys: Vec<String>,
    pub(crate) evidence: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CoverageTable {
    pub(crate) all_terminal: bool,
    pub(crate) entries: Vec<EngineCoverage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct NativeLibraryIdentity {
    pub(crate) canonical_path: String,
    pub(crate) upstream_version: String,
    pub(crate) sha256: String,
    pub(crate) file_bytes: u64,
    #[serde(default)]
    pub(crate) source_revision: Option<String>,
    #[serde(default)]
    pub(crate) build_provenance: Option<String>,
    #[serde(default)]
    pub(crate) linked_artifacts: Vec<NativeArtifactIdentity>,
    #[serde(default)]
    pub(crate) provenance_verified: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct NativeArtifactIdentity {
    pub(crate) canonical_path: String,
    pub(crate) sha256: String,
    pub(crate) file_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EngineIdentity {
    pub(crate) engine_id: String,
    pub(crate) display_name: String,
    pub(crate) implementation: String,
    pub(crate) upstream_version: String,
    pub(crate) adapter_schema: String,
    pub(crate) algorithm_id: String,
    pub(crate) sample_format: SampleFormat,
    pub(crate) quality_recipe: String,
    pub(crate) phase_response: String,
    pub(crate) native_library: Option<NativeLibraryIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct UnavailableEngine {
    pub(crate) engine_id: String,
    pub(crate) classification: MetricClassification,
    pub(crate) required: bool,
    pub(crate) reason: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct QualitySummary {
    pub(crate) classification: MetricClassification,
    pub(crate) valid: bool,
    pub(crate) input_frames: usize,
    pub(crate) expected_complete_output_frames: usize,
    pub(crate) actual_complete_output_frames: usize,
    pub(crate) all_output_samples_finite: bool,
    pub(crate) reported_api_buffering_latency_frames: Option<usize>,
    /// Input cursor before the first producing call (not physical latency).
    pub(crate) observed_input_frames_before_first_output: Option<usize>,
    pub(crate) observed_input_frames_through_first_output: Option<usize>,
    pub(crate) first_output_during_drain: bool,
    pub(crate) measured_impulse_peak_frame: usize,
    pub(crate) measured_impulse_peak_magnitude: f64,
    pub(crate) gain_997_hz_db: Option<f64>,
    pub(crate) gain_18_khz_db: Option<f64>,
    pub(crate) passband_max_abs_deviation_db: Option<f64>,
    pub(crate) thdn_997_hz_db: Option<f64>,
    pub(crate) stopband_input_hz: Option<f64>,
    pub(crate) folded_alias_hz: Option<f64>,
    pub(crate) alias_attenuation_db: Option<f64>,
    pub(crate) validity_errors: Vec<String>,
    pub(crate) grid: quality_grid::QualityGrid,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct WorkValidation {
    pub(crate) classification: MetricClassification,
    pub(crate) valid: bool,
    pub(crate) expected_warmup_consumed_frames_per_trial: usize,
    pub(crate) warmup_consumed_frames_per_trial: Vec<usize>,
    pub(crate) expected_timed_consumed_frames_per_trial: usize,
    pub(crate) timed_consumed_frames_per_trial: Vec<usize>,
    pub(crate) expected_complete_output_frames_per_trial: Vec<usize>,
    pub(crate) warmup_output_frames_per_trial: Vec<usize>,
    pub(crate) steady_output_frames_per_trial: Vec<usize>,
    pub(crate) drain_frames_per_trial: Vec<usize>,
    pub(crate) actual_complete_output_frames_per_trial: Vec<usize>,
    pub(crate) drain_terminal_per_trial: Vec<bool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ComparisonCase {
    pub(crate) case_key: String,
    pub(crate) engine: EngineIdentity,
    pub(crate) rate_pair: String,
    pub(crate) from_rate_hz: u32,
    pub(crate) to_rate_hz: u32,
    pub(crate) channels: usize,
    pub(crate) chunk_frames: usize,
    pub(crate) output_capacity_frames: usize,
    pub(crate) setup_ns: TrialDistribution,
    pub(crate) steady_ns_per_input_sample: TrialDistribution,
    pub(crate) steady_ns_per_input_buffer: TrialDistribution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) steady_cycles_per_input_sample: Option<TrialDistribution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) steady_cycles_per_input_buffer: Option<TrialDistribution>,
    pub(crate) reset_ns: TrialDistribution,
    pub(crate) drain_ns: TrialDistribution,
    pub(crate) source_buffer_duration_ns: f64,
    pub(crate) median_source_realtime_utilization_pct: f64,
    pub(crate) p95_source_realtime_utilization_pct: f64,
    pub(crate) quality: QualitySummary,
    pub(crate) work_validation: WorkValidation,
    pub(crate) trial_records: Vec<TrialRecord>,
    pub(crate) process_call_latency: Option<latency::CallLatency>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ComparisonConditions {
    pub(crate) adapter_schema: String,
    pub(crate) workload_id: String,
    pub(crate) channels_by_rate: BTreeMap<String, usize>,
    pub(crate) chunk_frames: usize,
    pub(crate) rate_pairs: Vec<String>,
    pub(crate) warmup_buffers: usize,
    pub(crate) iterations_per_trial: usize,
    pub(crate) trials: usize,
    pub(crate) trial_order: String,
    pub(crate) quality_input_frames: usize,
    pub(crate) quality_policy: String,
    pub(crate) output_capacity_policy: String,
    pub(crate) output_capacity_frames_by_rate: BTreeMap<String, usize>,
    pub(crate) pinned: bool,
    pub(crate) pin_core: Option<usize>,
    pub(crate) scheduling: Option<PinnedSchedulingState>,
    pub(crate) require_complete_matrix: bool,
    pub(crate) measured_engines: Vec<EngineIdentity>,
    pub(crate) unavailable_engine_ids: Vec<String>,
    /// Measured engine id -> rate pairs the engine supports (canonical create
    /// probe succeeded).  Measured coverage is validated against this support
    /// set instead of the full matrix.
    pub(crate) supported_rate_pairs_by_engine: BTreeMap<String, Vec<String>>,
    pub(crate) scope_boundaries: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct BaselineReference {
    pub(crate) path: String,
    pub(crate) revision: String,
    pub(crate) dirty: Option<bool>,
    pub(crate) generated_unix_ms: u128,
    pub(crate) max_median_regression_pct: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct SoakSummary {
    pub(crate) rounds_per_case: usize,
    pub(crate) iterations_per_round: usize,
    pub(crate) cases_soaked: usize,
    pub(crate) all_round_outputs_bit_identical: bool,
    pub(crate) working_set_before_bytes: Option<u64>,
    pub(crate) working_set_after_bytes: Option<u64>,
    pub(crate) warmup_buffers_per_round: usize,
    pub(crate) persistent_adapter_per_case: bool,
    pub(crate) elapsed_seconds: f64,
    pub(crate) memory_classification: MetricClassification,
    pub(crate) cases: Vec<SoakCase>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct SoakCase {
    pub(crate) identity: MeasurementIdentity,
    pub(crate) rounds_completed: usize,
    pub(crate) compared_output_samples: usize,
    pub(crate) complete_output_frames_per_round: usize,
    pub(crate) output_digest_fnv1a64: Vec<String>,
    pub(crate) working_set_after_round_bytes: Vec<Option<u64>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ComparisonReport {
    pub(crate) schema_version: u32,
    pub(crate) probe: String,
    pub(crate) generated_unix_ms: u128,
    pub(crate) mode: BenchMode,
    pub(crate) environment: BenchEnvironment,
    pub(crate) conditions: ComparisonConditions,
    pub(crate) coverage: CoverageTable,
    pub(crate) unavailable_engines: Vec<UnavailableEngine>,
    pub(crate) cases: Vec<ComparisonCase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) soak: Option<SoakSummary>,
    pub(crate) baseline: Option<BaselineReference>,
    pub(crate) comparisons: Vec<RegressionComparison>,
    pub(crate) run_failures: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LibraryPathSource {
    Argument,
    Environment,
}

impl LibraryPathSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Argument => "argument",
            Self::Environment => "environment",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ComparisonArgs {
    pub(crate) perf: PerfArgs,
    pub(crate) libsamplerate_path: Option<PathBuf>,
    libsamplerate_path_source: Option<LibraryPathSource>,
    pub(crate) native_library_paths: BTreeMap<String, PathBuf>,
    pub(crate) required_engines: BTreeSet<String>,
    pub(crate) require_complete_matrix: bool,
    pub(crate) soak_rounds: usize,
    pub(crate) pinned: bool,
    pub(crate) pin_core: usize,
}

impl ComparisonArgs {
    pub(crate) fn parse(argv: Vec<String>) -> Result<Self, String> {
        Self::parse_with_environment(argv, std::env::var_os("AUDIO_BENCH_LIBSAMPLERATE_PATH"))
    }

    fn parse_with_environment(
        argv: Vec<String>,
        environment_path: Option<OsString>,
    ) -> Result<Self, String> {
        let mut remaining = Vec::with_capacity(argv.len());
        let mut libsamplerate_path = None;
        let mut libsamplerate_path_source = None;
        let mut native_library_paths = BTreeMap::new();
        let mut required_engines = BTreeSet::new();
        let mut require_complete_matrix = false;
        let mut soak_rounds = 0usize;

        let mut index = 0usize;

        while index < argv.len() {
            let arg = &argv[index];
            match arg.as_str() {
                "--libsamplerate" => {
                    if libsamplerate_path.is_some() {
                        return Err("--libsamplerate may be supplied only once".to_string());
                    }
                    index += 1;
                    let value = argv
                        .get(index)
                        .ok_or_else(|| "--libsamplerate requires a path".to_string())?;
                    if value.is_empty() {
                        return Err("--libsamplerate requires a non-empty path".to_string());
                    }
                    libsamplerate_path = Some(PathBuf::from(value));
                    libsamplerate_path_source = Some(LibraryPathSource::Argument);
                }
                "--require-engine" => {
                    index += 1;
                    let value = argv
                        .get(index)
                        .ok_or_else(|| "--require-engine requires an engine id".to_string())?;
                    validate_engine_id(value)?;
                    required_engines.insert(value.clone());
                }
                "--engine-library" => {
                    index += 1;
                    let value = argv.get(index).ok_or_else(|| {
                        "--engine-library requires <engine-id>=<path>".to_string()
                    })?;
                    insert_engine_library(&mut native_library_paths, value)?;
                }
                "--require-complete-matrix" => {
                    require_complete_matrix = true;
                }
                "--soak-rounds" => {
                    index += 1;
                    let value = argv
                        .get(index)
                        .ok_or_else(|| "--soak-rounds requires a count".to_string())?;
                    soak_rounds = parse_soak_rounds(value)?;
                }

                _ => {
                    if let Some(value) = arg.strip_prefix("--libsamplerate=") {
                        if libsamplerate_path.is_some() {
                            return Err("--libsamplerate may be supplied only once".to_string());
                        }
                        if value.is_empty() {
                            return Err("--libsamplerate requires a non-empty path".to_string());
                        }
                        libsamplerate_path = Some(PathBuf::from(value));
                        libsamplerate_path_source = Some(LibraryPathSource::Argument);
                    } else if let Some(value) = arg.strip_prefix("--require-engine=") {
                        validate_engine_id(value)?;
                        required_engines.insert(value.to_string());
                    } else if let Some(value) = arg.strip_prefix("--engine-library=") {
                        insert_engine_library(&mut native_library_paths, value)?;
                    } else if let Some(value) = arg.strip_prefix("--soak-rounds=") {
                        soak_rounds = parse_soak_rounds(value)?;
                    } else {
                        remaining.push(arg.clone());
                    }
                }
            }
            index += 1;
        }

        if libsamplerate_path.is_none() {
            if let Some(path) = environment_path.filter(|path| !path.is_empty()) {
                libsamplerate_path = Some(PathBuf::from(path));
                libsamplerate_path_source = Some(LibraryPathSource::Environment);
            }
        }

        let pinned = parse_pinned_probe_args(remaining)?;
        Ok(Self {
            perf: PerfArgs::parse(pinned.remaining)?,
            libsamplerate_path,
            libsamplerate_path_source,
            native_library_paths,
            required_engines,
            require_complete_matrix,
            soak_rounds,
            pinned: pinned.enabled,
            pin_core: pinned.core,
        })
    }
}

const SOAK_ROUNDS_LIMIT: usize = 4096;
const SOAK_ITERATIONS_PER_ROUND: usize = 64;
/// Compact record accompanying an actual complete-sample bit comparison.
/// Hash equality alone is not used as the determinism gate.
struct SoakDigest(u64);

impl SoakDigest {
    fn of_bits(samples: &[u64]) -> String {
        let mut digest = Self(0xcbf2_9ce4_8422_2325);
        for sample in samples {
            for byte in sample.to_le_bytes() {
                digest.0 ^= u64::from(byte);
                digest.0 = digest.0.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        format!("{:016x}", digest.0)
    }
}

fn parse_soak_rounds(value: &str) -> Result<usize, String> {
    let rounds = value
        .parse::<usize>()
        .map_err(|_| format!("--soak-rounds expects a non-negative integer, got '{value}'"))?;
    if rounds > SOAK_ROUNDS_LIMIT {
        return Err(format!(
            "--soak-rounds {rounds} exceeds the {SOAK_ROUNDS_LIMIT}-round limit"
        ));
    }
    Ok(rounds)
}

fn run_soak(
    factories: &[EngineFactory],
    prepared_inputs: &[PreparedInputs],
    output_capacity_frames_by_rate: &BTreeMap<String, usize>,
    rounds: usize,
) -> Result<(SoakSummary, Vec<String>), String> {
    let started = Instant::now();
    let mut failures = Vec::new();
    let working_set_before = process_working_set_bytes();
    let mut cases = Vec::new();
    for (rate_index, rate) in RATE_PAIRS.into_iter().enumerate() {
        let capacity = *output_capacity_frames_by_rate
            .get(rate.id)
            .ok_or_else(|| format!("soak capacity missing for {}", rate.id))?;
        let inputs = prepared_inputs
            .get(rate_index)
            .ok_or_else(|| format!("soak inputs missing for {}", rate.id))?;
        for factory in factories
            .iter()
            .filter(|factory| factory.supports_rate(rate.id))
        {
            let mut adapter = match factory.create(rate, rate.channels, CHUNK_FRAMES) {
                Ok(adapter) => adapter,
                Err(error) => {
                    failures.push(format!(
                        "{} {} soak create: {error}",
                        factory.identity().engine_id,
                        rate.id
                    ));
                    continue;
                }
            };
            let mut reference = None;
            let mut case = SoakCase {
                identity: MeasurementIdentity::new(factory.identity(), rate, capacity),
                rounds_completed: 0,
                compared_output_samples: 0,
                complete_output_frames_per_round: 0,
                output_digest_fnv1a64: Vec::with_capacity(rounds),
                working_set_after_round_bytes: Vec::with_capacity(rounds),
            };
            for round in 0..rounds {
                let outcome = (|| {
                    if round > 0 {
                        adapter.reset()?;
                    }
                    drive_complete_oracle_stream(
                        &mut adapter,
                        inputs,
                        capacity,
                        WARMUP_BUFFERS + SOAK_ITERATIONS_PER_ROUND,
                    )
                })();
                let bits = match outcome {
                    Ok(bits) => bits,
                    Err(error) => {
                        failures.push(format!(
                            "{} {} soak round {round}: {error}",
                            case.identity.engine_id, rate.id
                        ));
                        break;
                    }
                };
                case.rounds_completed += 1;
                case.output_digest_fnv1a64.push(SoakDigest::of_bits(&bits));
                case.working_set_after_round_bytes
                    .push(process_working_set_bytes());
                if let Some(expected) = &reference {
                    case.compared_output_samples += bits.len();
                    if &bits != expected {
                        failures.push(format!(
                            "{} {} soak round {round}: complete output bits differ after reset",
                            case.identity.engine_id, rate.id
                        ));
                    }
                } else {
                    case.complete_output_frames_per_round = bits.len() / rate.channels;
                    reference = Some(bits);
                }
            }
            cases.push(case);
        }
    }
    Ok((
        SoakSummary {
            rounds_per_case: rounds,
            iterations_per_round: SOAK_ITERATIONS_PER_ROUND,
            cases_soaked: cases.len(),
            all_round_outputs_bit_identical: failures.is_empty(),
            working_set_before_bytes: working_set_before,
            working_set_after_bytes: process_working_set_bytes(),
            warmup_buffers_per_round: WARMUP_BUFFERS,
            persistent_adapter_per_case: true,
            elapsed_seconds: started.elapsed().as_secs_f64(),
            memory_classification: MetricClassification::Report,
            cases,
        },
        failures,
    ))
}

fn insert_engine_library(
    paths: &mut BTreeMap<String, PathBuf>,
    specification: &str,
) -> Result<(), String> {
    let (engine_id, path) = specification.split_once('=').ok_or_else(|| {
        format!("invalid --engine-library '{specification}'; expected <engine-id>=<path>")
    })?;
    validate_engine_id(engine_id)?;
    if !NATIVE_SHIM_ENGINE_IDS.contains(&engine_id) {
        return Err(format!(
            "engine '{engine_id}' does not use --engine-library; expected one of {}",
            NATIVE_SHIM_ENGINE_IDS.join(", ")
        ));
    }
    if path.is_empty() {
        return Err(format!(
            "--engine-library path for '{engine_id}' must not be empty"
        ));
    }
    if paths
        .insert(engine_id.to_string(), PathBuf::from(path))
        .is_some()
    {
        return Err(format!(
            "--engine-library for '{engine_id}' may be supplied only once"
        ));
    }
    Ok(())
}

fn validate_engine_id(value: &str) -> Result<(), String> {
    if ALL_ENGINE_IDS.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "unknown engine id '{value}'; expected one of {}",
            ALL_ENGINE_IDS.join(", ")
        ))
    }
}

#[derive(Clone)]
struct PreparedInputs {
    channels: usize,
    f64_samples: Vec<f64>,
    f32_samples: Vec<f32>,
}

impl PreparedInputs {
    fn new(rate: RatePair) -> Self {
        Self::with_chunk(rate, CHUNK_FRAMES)
    }

    fn with_chunk(rate: RatePair, chunk_frames: usize) -> Self {
        let f64_samples = synthetic_buffer(chunk_frames, rate.channels, rate.from_hz);
        let f32_samples = f64_samples.iter().map(|sample| *sample as f32).collect();
        Self {
            channels: rate.channels,
            f64_samples,
            f32_samples,
        }
    }
}

pub(crate) fn run(argv: Vec<String>) -> Result<(), String> {
    if argv.iter().any(|arg| arg == "--diagnostics") {
        return diagnostics::run(argv);
    }
    if argv.iter().any(|arg| arg == "--resources") {
        return resources::run(argv);
    }
    let args = ComparisonArgs::parse(argv)?;
    if args.perf.help {
        print_help();
        return Ok(());
    }

    let scheduling = if args.pinned {
        Some(pin_current_thread(args.pin_core)?)
    } else {
        None
    };

    let (iterations, trials) = workload(args.perf.mode);
    let Discovery {
        factories,
        mut unavailable,
    } = adapters::discover(
        args.libsamplerate_path.as_deref(),
        &args.native_library_paths,
        &args.required_engines,
    );

    let prepared_inputs = RATE_PAIRS.map(PreparedInputs::new);
    let output_capacity_frames_by_rate = RATE_PAIRS
        .iter()
        .map(|rate| {
            common_output_capacity_frames(&factories, *rate)
                .map(|capacity| (rate.id.to_string(), capacity))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut cases = Vec::with_capacity(factories.len() * RATE_PAIRS.len());
    let mut runtime_failures = BTreeMap::<String, String>::new();
    for (rate_index, rate) in RATE_PAIRS.into_iter().enumerate() {
        let output_capacity_frames = *output_capacity_frames_by_rate
            .get(rate.id)
            .ok_or_else(|| format!("missing common output capacity for {}", rate.id))?;
        let supported_indices = factories
            .iter()
            .enumerate()
            .filter(|(_, factory)| factory.supports_rate(rate.id))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let mut accumulators = supported_indices
            .iter()
            .map(|&index| {
                (
                    index,
                    CaseAccumulator::new(
                        factories[index].identity().clone(),
                        rate,
                        output_capacity_frames,
                        trials,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();

        for index in &supported_indices {
            let factory = &factories[*index];
            if runtime_failures.contains_key(&factory.identity().engine_id) {
                continue;
            }
            if let Err(error) = validate_reset_matches_fresh(
                factory,
                rate,
                &prepared_inputs[rate_index],
                output_capacity_frames,
            ) {
                runtime_failures.insert(
                    factory.identity().engine_id.clone(),
                    format!("{} reset/fresh oracle failed: {error}", rate.id),
                );
            }
        }

        accumulators.retain(|index, _| {
            !runtime_failures.contains_key(&factories[*index].identity().engine_id)
        });
        let failures =
            collect_scheduled_trials(&mut accumulators, trials, |factory_index, trial| {
                measure_trial(
                    &factories[factory_index],
                    rate,
                    &prepared_inputs[rate_index],
                    iterations,
                    output_capacity_frames,
                    trial,
                )
            });
        for (factory_index, error) in failures {
            runtime_failures.insert(
                factories[factory_index].identity().engine_id.clone(),
                format!("{} {error}", rate.id),
            );
        }

        for (factory_index, accumulator) in accumulators {
            let factory = &factories[factory_index];
            if runtime_failures.contains_key(&factory.identity().engine_id) {
                continue;
            }
            let quality = match quality::measure_quality(
                factory,
                rate,
                rate.channels,
                CHUNK_FRAMES,
                QUALITY_INPUT_FRAMES,
                output_capacity_frames,
            ) {
                Ok(quality) => quality,
                Err(error) => {
                    runtime_failures.insert(
                        factory.identity().engine_id.clone(),
                        format!("{} quality render failed: {error}", rate.id),
                    );
                    continue;
                }
            };
            let call_latency = match latency::measure(
                factory,
                rate,
                &prepared_inputs[rate_index],
                iterations,
                output_capacity_frames,
            ) {
                Ok(measured) => measured,
                Err(error) => {
                    runtime_failures.insert(
                        factory.identity().engine_id.clone(),
                        format!("{} process-call latency: {error}", rate.id),
                    );
                    continue;
                }
            };
            match accumulator.finish(iterations, quality) {
                Ok(mut case) => {
                    case.process_call_latency = Some(call_latency);
                    cases.push(case);
                }
                Err(error) => {
                    runtime_failures.insert(
                        factory.identity().engine_id.clone(),
                        format!("{} case aggregation failed: {error}", rate.id),
                    );
                }
            }
        }
    }

    if !runtime_failures.is_empty() {
        cases.retain(|case| !runtime_failures.contains_key(&case.engine.engine_id));
        unavailable.extend(
            runtime_failures
                .iter()
                .map(|(engine_id, reason)| UnavailableEngine {
                    engine_id: engine_id.clone(),
                    classification: MetricClassification::Skipped,
                    required: args.required_engines.contains(engine_id),
                    reason: reason.clone(),
                }),
        );
    }
    unavailable.sort_by(|left, right| left.engine_id.cmp(&right.engine_id));

    let measured_engines = factories
        .iter()
        .filter(|factory| !runtime_failures.contains_key(&factory.identity().engine_id))
        .map(|factory| factory.identity().clone())
        .collect::<Vec<_>>();
    let unavailable_engine_ids = unavailable
        .iter()
        .map(|engine| engine.engine_id.clone())
        .collect::<Vec<_>>();
    let supported_rate_pairs_by_engine = factories
        .iter()
        .map(|factory| {
            (
                factory.identity().engine_id.to_string(),
                RATE_PAIRS
                    .iter()
                    .filter(|rate| factory.supports_rate(rate.id))
                    .map(|rate| rate.id.to_string())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let supported_rate_pairs = supported_rate_pairs_by_engine
        .iter()
        .map(|(engine_id, pairs)| {
            (
                engine_id.clone(),
                pairs.iter().cloned().collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let (soak, soak_failures) = if args.soak_rounds > 0 {
        let (summary, failures) = run_soak(
            &factories,
            &prepared_inputs,
            &output_capacity_frames_by_rate,
            args.soak_rounds,
        )?;
        (Some(summary), failures)
    } else {
        (None, Vec::new())
    };
    let coverage = build_coverage(
        cases.iter().map(|case| {
            (
                case.engine.engine_id.clone(),
                case.rate_pair.clone(),
                case.case_key.clone(),
            )
        }),
        &unavailable,
        &supported_rate_pairs,
    )?;
    let path_source = args
        .libsamplerate_path_source
        .map_or("none", LibraryPathSource::as_str);
    let conditions = ComparisonConditions {
        adapter_schema: ADAPTER_SCHEMA.to_string(),
        workload_id: "quality_latency_throughput_pareto_v4_identified_complete_stream".to_string(),
        channels_by_rate: RATE_PAIRS
            .iter()
            .map(|rate| (rate.id.to_string(), rate.channels))
            .collect(),
        chunk_frames: CHUNK_FRAMES,
        rate_pairs: RATE_PAIRS.iter().map(|rate| rate.id.to_string()).collect(),
        warmup_buffers: WARMUP_BUFFERS,
        iterations_per_trial: iterations,
        trials,
        trial_order: "alternating_forward_reverse_by_stable_factory_identity".to_string(),
        quality_input_frames: QUALITY_INPUT_FRAMES,
        quality_policy: format!(
            "explicit_quality_grid_report_only;gate_complete_finite_work;separate_process_call_tails;explicit_native_paths;libsamplerate_path_source={path_source}"
        ),
        output_capacity_policy:
            "common_max_per_rate_across_all_measured_factories;identical_for_process_reset_quality_and_drain"
                .to_string(),
        output_capacity_frames_by_rate,

        pinned: args.pinned,
        pin_core: args.pinned.then_some(args.pin_core),
        scheduling,
        require_complete_matrix: args.require_complete_matrix,
        measured_engines,
        unavailable_engine_ids,
        supported_rate_pairs_by_engine,
        scope_boundaries: [
            "cross_format_speed_gate",
            "device_driver_latency",
            "decoder_pipeline",
            "continuous_frequency_response",
            "cross_engine_equal_quality_acceptance",
            "long_duration_leak_proof",
            "cross_platform_runtime_performance",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    };
    let mut report = ComparisonReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: PROBE.to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.perf.mode,
        environment: BenchEnvironment::capture(),
        conditions,
        coverage,
        unavailable_engines: unavailable,
        cases,
        soak,
        baseline: None,
        comparisons: Vec::new(),
        run_failures: Vec::new(),
    };
    report.run_failures.extend(soak_failures);
    report.run_failures.extend(
        runtime_failures
            .iter()
            .map(|(engine, error)| format!("{engine}: {error}")),
    );
    for case in &report.cases {
        if let Err(error) = validate_case_trials(case, &report.conditions) {
            report.run_failures.push(error);
        }
        if let Err(error) = latency::validate(case, iterations) {
            report.run_failures.push(error);
        }
    }
    validate_coverage(&report)?;

    if let Some(path) = &args.perf.baseline {
        let baseline_result = read_json(path, "resampler comparison baseline").and_then(
            |baseline: ComparisonReport| {
                let comparisons =
                    compare_with_baseline(&report, &baseline, args.perf.max_median_regression_pct)?;
                Ok((baseline, comparisons))
            },
        );
        match baseline_result {
            Ok((baseline, comparisons)) => {
                report.comparisons = comparisons;
                report.baseline = Some(BaselineReference {
                    path: path.display().to_string(),
                    revision: baseline.environment.revision,
                    dirty: baseline.environment.dirty,
                    generated_unix_ms: baseline.generated_unix_ms,
                    max_median_regression_pct: args.perf.max_median_regression_pct,
                });
            }
            Err(error) => report
                .run_failures
                .push(format!("baseline validation failed: {error}")),
        }
    }

    if let Err(error) = enforce_required_engines(&report) {
        report.run_failures.push(error);
    }
    if args.require_complete_matrix {
        if let Err(error) = enforce_complete_coverage(&report.coverage) {
            report.run_failures.push(error);
        }
        if let Err(error) = enforce_formal_provenance(&report) {
            report.run_failures.push(error);
        }
    }
    if args.perf.enforce {
        if let Err(error) = enforce_report(&report) {
            report.run_failures.push(error);
        }
    }

    if let Some(path) = &args.perf.out {
        write_json(path, &report, "resampler comparison report")?;
        validate_written_report(path, &report)?;
    }
    print_report(&report)?;

    if report.run_failures.is_empty() {
        Ok(())
    } else {
        Err(report.run_failures.join("\n"))
    }
}

fn workload(mode: BenchMode) -> (usize, usize) {
    match mode {
        BenchMode::Quick => (200, 7),
        BenchMode::Full => (1_000, 11),
        BenchMode::Heavy => (4_000, 15),
    }
}

fn common_output_capacity_frames(
    factories: &[EngineFactory],
    rate: RatePair,
) -> Result<usize, String> {
    factories
        .iter()
        .filter(|factory| factory.supports_rate(rate.id))
        .map(|factory| {
            factory
                .create(rate, rate.channels, CHUNK_FRAMES)
                .map(|adapter| adapter.max_output_frames().max(1))
                .map_err(|error| {
                    format!(
                        "{} common-capacity probe failed for {}: {error}",
                        factory.identity().engine_id,
                        rate.id
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .ok_or_else(|| format!("no supported factory available for {}", rate.id))
}

fn measure_trial(
    factory: &EngineFactory,
    rate: RatePair,
    inputs: &PreparedInputs,
    iterations: usize,
    output_frames: usize,
    trial_index: usize,
) -> Result<TrialRecord, String> {
    let setup_start = Instant::now();
    let mut adapter = factory.create(rate, rate.channels, CHUNK_FRAMES)?;
    let setup_ns = positive_elapsed_ns(setup_start, "setup")?;
    if output_frames < adapter.max_output_frames().max(1) {
        return Err(format!(
            "common output capacity {output_frames} is below {} required frames for {}",
            adapter.max_output_frames(),
            factory.identity().engine_id
        ));
    }
    let mut output_f64 = vec![0.0; output_frames * rate.channels];
    let mut output_f32 = vec![0.0; output_frames * rate.channels];

    let mut warmup_consumed_frames = 0usize;
    let mut warmup_output_frames = 0usize;
    for _ in 0..WARMUP_BUFFERS {
        let progress = process_prepared(
            &mut adapter,
            inputs,
            &mut output_f64,
            &mut output_f32,
            false,
        )?;
        if progress.consumed_frames != CHUNK_FRAMES {
            return Err(format!(
                "{} warmup consumed {} of {} frames",
                factory.identity().engine_id,
                progress.consumed_frames,
                CHUNK_FRAMES
            ));
        }
        if progress.finished {
            return Err(format!(
                "{} reached terminal state during warmup",
                factory.identity().engine_id
            ));
        }
        warmup_consumed_frames = warmup_consumed_frames
            .checked_add(progress.consumed_frames)
            .ok_or_else(|| "warmup consumed-frame total overflowed usize".to_string())?;
        warmup_output_frames = warmup_output_frames
            .checked_add(progress.produced_frames)
            .ok_or_else(|| "warmup output-frame total overflowed usize".to_string())?;
    }

    let mut timed_consumed_frames = 0usize;
    let mut steady_output_frames = 0usize;
    let steady_cycles_start = query_thread_cycles();
    let steady_start = Instant::now();
    for iteration in 0..iterations {
        let progress = process_prepared(
            &mut adapter,
            black_box(inputs),
            &mut output_f64,
            &mut output_f32,
            iteration + 1 == iterations,
        )?;
        if progress.finished && iteration + 1 != iterations {
            return Err(format!(
                "{} reached terminal state at timed iteration {iteration}/{iterations}",
                factory.identity().engine_id
            ));
        }
        timed_consumed_frames = timed_consumed_frames
            .checked_add(progress.consumed_frames)
            .ok_or_else(|| "timed consumed-frame total overflowed usize".to_string())?;
        steady_output_frames = steady_output_frames
            .checked_add(progress.produced_frames)
            .ok_or_else(|| "timed output-frame total overflowed usize".to_string())?;
        black_box(progress.produced_frames);
    }
    let steady_ns = positive_elapsed_ns(steady_start, "steady process")?;
    let steady_ns_per_input_buffer = steady_ns / iterations as f64;
    let steady_ns_per_input_sample = steady_ns / (iterations * CHUNK_FRAMES * rate.channels) as f64;
    let (steady_cycles_per_input_sample, steady_cycles_per_input_buffer) =
        match (steady_cycles_start, query_thread_cycles()) {
            (Some(start), Some(end)) if end >= start => {
                let total = (end - start) as f64;
                (
                    Some(total / (iterations * CHUNK_FRAMES * rate.channels) as f64),
                    Some(total / iterations as f64),
                )
            }
            _ => (None, None),
        };

    let total_input_frames = warmup_consumed_frames
        .checked_add(timed_consumed_frames)
        .ok_or_else(|| "complete input-frame total overflowed usize".to_string())?;
    let expected_complete_output_frames =
        adapter.expected_complete_output_frames(total_input_frames);
    let drain_start = Instant::now();
    let (drain_frames, drain_terminal) =
        drain_adapter(&mut adapter, &mut output_f64, &mut output_f32)?;
    let drain_ns = positive_elapsed_ns(drain_start, "drain")?;

    let reset_start = Instant::now();
    adapter.reset()?;
    let reset_ns = positive_elapsed_ns(reset_start, "reset")?;

    let actual_complete_output_frames = warmup_output_frames
        .checked_add(steady_output_frames)
        .and_then(|frames| frames.checked_add(drain_frames))
        .ok_or_else(|| "complete output-frame total overflowed usize".to_string())?;

    Ok(TrialRecord {
        identity: MeasurementIdentity::new(factory.identity(), rate, output_frames),
        trial_index,
        measurement: TrialMeasurement {
            setup_ns,
            steady_ns_per_input_sample,
            steady_ns_per_input_buffer,
            steady_cycles_per_input_sample,
            steady_cycles_per_input_buffer,
            reset_ns,
            drain_ns,
            warmup_consumed_frames,
            timed_consumed_frames,
            expected_complete_output_frames,
            warmup_output_frames,
            steady_output_frames,
            drain_frames,
            actual_complete_output_frames,
            drain_terminal,
        },
    })
}

fn validate_reset_matches_fresh(
    factory: &EngineFactory,
    rate: RatePair,
    inputs: &PreparedInputs,
    output_frames: usize,
) -> Result<(), String> {
    let mut reset_adapter = factory.create(rate, rate.channels, CHUNK_FRAMES)?;
    if output_frames < reset_adapter.max_output_frames().max(1) {
        return Err(format!(
            "common reset output capacity {output_frames} is below {} required frames",
            reset_adapter.max_output_frames()
        ));
    }
    let mut output_f64 = vec![0.0; output_frames * rate.channels];
    let mut output_f32 = vec![0.0; output_frames * rate.channels];
    for prefix_index in 0..3 {
        let progress = process_prepared(
            &mut reset_adapter,
            inputs,
            &mut output_f64,
            &mut output_f32,
            false,
        )?;
        if progress.consumed_frames != CHUNK_FRAMES || progress.finished {
            return Err(format!(
                "dirty prefix {prefix_index} returned consumed={}/{CHUNK_FRAMES}, finished={}",
                progress.consumed_frames, progress.finished
            ));
        }
    }
    reset_adapter.reset()?;

    let mut fresh_adapter = factory.create(rate, rate.channels, CHUNK_FRAMES)?;
    let reset_output = drive_complete_oracle_stream(&mut reset_adapter, inputs, output_frames, 5)?;
    let fresh_output = drive_complete_oracle_stream(&mut fresh_adapter, inputs, output_frames, 5)?;
    if reset_output == fresh_output {
        return Ok(());
    }

    let first_difference = reset_output
        .iter()
        .zip(&fresh_output)
        .position(|(reset, fresh)| reset != fresh);
    Err(match first_difference {
        Some(sample) => format!(
            "reset output differs from fresh output at sample {sample} (frame {}, channel {}): reset_bits=0x{:016x}, fresh_bits=0x{:016x}",
            sample / rate.channels,
            sample % rate.channels,
            reset_output[sample],
            fresh_output[sample]
        ),
        None => format!(
            "reset output length {} differs from fresh output length {}",
            reset_output.len(),
            fresh_output.len()
        ),
    })
}

fn drive_complete_oracle_stream(
    adapter: &mut EngineAdapter,
    inputs: &PreparedInputs,
    output_frames: usize,
    input_buffers: usize,
) -> Result<Vec<u64>, String> {
    if input_buffers == 0 || output_frames < adapter.max_output_frames().max(1) {
        return Err(
            "complete oracle requires nonzero input and sufficient output capacity".to_string(),
        );
    }
    let channels = inputs.channels;
    let mut output_f64 = vec![0.0; output_frames * channels];
    let mut output_f32 = vec![0.0; output_frames * channels];
    let expected_frames = adapter.expected_complete_output_frames(input_buffers * CHUNK_FRAMES);
    let mut output_bits = Vec::with_capacity(expected_frames * channels);

    for index in 0..input_buffers {
        let progress = process_prepared(
            adapter,
            inputs,
            &mut output_f64,
            &mut output_f32,
            index + 1 == input_buffers,
        )?;
        if progress.consumed_frames != CHUNK_FRAMES {
            return Err(format!(
                "oracle input {index} consumed {} of {CHUNK_FRAMES} frames",
                progress.consumed_frames
            ));
        }
        append_oracle_output(
            adapter.sample_format(),
            channels,
            progress.produced_frames,
            &output_f64,
            &output_f32,
            &mut output_bits,
        );
    }

    let mut terminal = false;
    for _ in 0..MAX_DRAIN_CALLS {
        let progress = match adapter.sample_format() {
            SampleFormat::InterleavedF64 => adapter.drain_f64(&mut output_f64)?,
            SampleFormat::InterleavedF32 => adapter.drain_f32(&mut output_f32)?,
        };
        append_oracle_output(
            adapter.sample_format(),
            channels,
            progress.produced_frames,
            &output_f64,
            &output_f32,
            &mut output_bits,
        );
        if progress.finished {
            terminal = true;
            break;
        }
        if progress.produced_frames == 0 {
            return Err("complete oracle drain stalled before terminal state".to_string());
        }
    }
    if !terminal {
        return Err(format!(
            "complete oracle drain exceeded {MAX_DRAIN_CALLS} calls"
        ));
    }
    let actual_frames = output_bits.len() / channels;
    if actual_frames != expected_frames {
        return Err(format!(
            "complete oracle produced {actual_frames} complete frames, expected {expected_frames}"
        ));
    }
    let finite = output_bits
        .iter()
        .all(|&bits| match adapter.sample_format() {
            SampleFormat::InterleavedF64 => f64::from_bits(bits).is_finite(),
            SampleFormat::InterleavedF32 => f32::from_bits(bits as u32).is_finite(),
        });
    if !finite {
        return Err("complete oracle output contains non-finite samples".to_string());
    }
    Ok(output_bits)
}

fn append_oracle_output(
    sample_format: SampleFormat,
    channels: usize,
    produced_frames: usize,
    output_f64: &[f64],
    output_f32: &[f32],
    output_bits: &mut Vec<u64>,
) {
    let samples = produced_frames * channels;
    match sample_format {
        SampleFormat::InterleavedF64 => {
            output_bits.extend(output_f64[..samples].iter().map(|sample| sample.to_bits()));
        }
        SampleFormat::InterleavedF32 => {
            output_bits.extend(
                output_f32[..samples]
                    .iter()
                    .map(|sample| u64::from(sample.to_bits())),
            );
        }
    }
}

fn process_prepared(
    adapter: &mut EngineAdapter,
    inputs: &PreparedInputs,
    output_f64: &mut [f64],
    output_f32: &mut [f32],
    end_of_input: bool,
) -> Result<adapters::AdapterProgress, String> {
    match adapter.sample_format() {
        SampleFormat::InterleavedF64 => {
            if end_of_input {
                adapter.process_final_f64(black_box(&inputs.f64_samples), black_box(output_f64))
            } else {
                adapter.process_f64(black_box(&inputs.f64_samples), black_box(output_f64))
            }
        }
        SampleFormat::InterleavedF32 => {
            if end_of_input {
                adapter.process_final_f32(black_box(&inputs.f32_samples), black_box(output_f32))
            } else {
                adapter.process_f32(black_box(&inputs.f32_samples), black_box(output_f32))
            }
        }
    }
}

fn drain_adapter(
    adapter: &mut EngineAdapter,
    output_f64: &mut [f64],
    output_f32: &mut [f32],
) -> Result<(usize, bool), String> {
    let mut frames = 0usize;
    for _ in 0..MAX_DRAIN_CALLS {
        let progress = match adapter.sample_format() {
            SampleFormat::InterleavedF64 => adapter.drain_f64(output_f64)?,
            SampleFormat::InterleavedF32 => adapter.drain_f32(output_f32)?,
        };
        frames = frames.saturating_add(progress.produced_frames);
        if progress.finished {
            return Ok((frames, true));
        }
    }
    Ok((frames, false))
}

fn positive_elapsed_ns(start: Instant, label: &str) -> Result<f64, String> {
    let elapsed = start.elapsed().as_nanos() as f64;
    if elapsed.is_finite() && elapsed > 0.0 {
        Ok(elapsed)
    } else {
        Err(format!("{label} timer produced invalid {elapsed} ns"))
    }
}

/// Current thread CPU cycle counter (`QueryThreadCycleTime`). Returns `None`
/// on platforms without a thread-cycle counter; callers must degrade to
/// wall-clock-only reporting instead of failing the run.
#[cfg(windows)]
pub(crate) fn query_thread_cycles() -> Option<u64> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> isize;
        fn QueryThreadCycleTime(thread: isize, cycles: *mut u64) -> i32;
    }

    let mut cycles = 0u64;
    // SAFETY: pseudo handle that requires no cleanup; out pointer targets a
    // local u64. Failure is reported by the return value, not the pointer.
    let succeeded = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
    if succeeded != 0 {
        Some(cycles)
    } else {
        None
    }
}

#[cfg(not(windows))]
pub(crate) fn query_thread_cycles() -> Option<u64> {
    None
}

/// Current process working set size in bytes, or `None` when unavailable.
pub(crate) fn process_working_set_bytes() -> Option<u64> {
    resources::working_set_bytes()
}

fn compare_with_baseline(
    candidate: &ComparisonReport,
    baseline: &ComparisonReport,
    threshold_pct: f64,
) -> Result<Vec<RegressionComparison>, String> {
    validate_performance_baseline(
        "resampler comparison",
        PerformanceReportIdentity {
            schema_version: candidate.schema_version,
            probe: &candidate.probe,
            mode: candidate.mode,
            environment: &candidate.environment,
            conditions: &candidate.conditions,
        },
        PerformanceReportIdentity {
            schema_version: baseline.schema_version,
            probe: &baseline.probe,
            mode: baseline.mode,
            environment: &baseline.environment,
            conditions: &baseline.conditions,
        },
    )?;
    compare_case_medians(
        candidate.cases.iter().map(|case| {
            (
                case.case_key.clone(),
                case.steady_ns_per_input_sample.median,
            )
        }),
        baseline.cases.iter().map(|case| {
            (
                case.case_key.clone(),
                case.steady_ns_per_input_sample.median,
            )
        }),
        threshold_pct,
    )
}

fn build_coverage(
    measured_cases: impl IntoIterator<Item = (String, String, String)>,
    unavailable: &[UnavailableEngine],
    supported_rate_pairs: &BTreeMap<String, BTreeSet<String>>,
) -> Result<CoverageTable, String> {
    let mut measured_by_engine = BTreeMap::<String, BTreeMap<String, String>>::new();
    let mut seen_case_keys = BTreeSet::new();
    for (engine_id, rate_pair, case_key) in measured_cases {
        validate_engine_id(&engine_id)
            .map_err(|error| format!("invalid measured coverage row: {error}"))?;
        if !seen_case_keys.insert(case_key.clone()) {
            return Err(format!(
                "coverage contains duplicate measured case key '{case_key}'"
            ));
        }
        if measured_by_engine
            .entry(engine_id.clone())
            .or_default()
            .insert(rate_pair.clone(), case_key)
            .is_some()
        {
            return Err(format!(
                "coverage contains duplicate rate pair '{rate_pair}' for engine '{engine_id}'"
            ));
        }
    }

    let mut unavailable_by_engine = BTreeMap::new();
    for entry in unavailable {
        validate_engine_id(&entry.engine_id)
            .map_err(|error| format!("invalid unavailable coverage row: {error}"))?;
        if entry.classification != MetricClassification::Skipped {
            return Err(format!(
                "unavailable engine '{}' must retain skipped metric classification",
                entry.engine_id
            ));
        }
        if unavailable_by_engine
            .insert(entry.engine_id.clone(), entry)
            .is_some()
        {
            return Err(format!(
                "coverage contains duplicate unavailable engine '{}'",
                entry.engine_id
            ));
        }
    }

    let default_supported_rate_pairs = RATE_PAIRS
        .iter()
        .map(|rate| rate.id.to_string())
        .collect::<BTreeSet<_>>();
    let mut entries = Vec::with_capacity(ALL_ENGINE_IDS.len());
    for engine_id in ALL_ENGINE_IDS {
        let measured = measured_by_engine.remove(engine_id);
        let unavailable = unavailable_by_engine.remove(engine_id);
        let supported = supported_rate_pairs
            .get(engine_id)
            .unwrap_or(&default_supported_rate_pairs);
        let entry = match (measured, unavailable) {
            (Some(_), Some(_)) => {
                return Err(format!(
                    "engine '{engine_id}' is both measured and unavailable"
                ));
            }
            (Some(measured), None) => {
                let actual_rate_pairs = measured.keys().cloned().collect::<BTreeSet<_>>();
                if actual_rate_pairs != *supported {
                    return Err(format!(
                        "engine '{engine_id}' has incomplete measured rate coverage: expected supported {:?}, found {:?}",
                        supported, actual_rate_pairs
                    ));
                }
                EngineCoverage {
                    engine_id: engine_id.to_string(),
                    state: CoverageState::Measured,
                    terminal: true,
                    measured_rate_pairs: RATE_PAIRS
                        .iter()
                        .filter(|rate| actual_rate_pairs.contains(rate.id))
                        .map(|rate| rate.id.to_string())
                        .collect(),
                    case_keys: RATE_PAIRS
                        .iter()
                        .filter(|rate| actual_rate_pairs.contains(rate.id))
                        .map(|rate| measured[rate.id].clone())
                        .collect(),
                    evidence: None,
                }
            }
            (None, Some(unavailable)) => EngineCoverage {
                engine_id: engine_id.to_string(),
                state: CoverageState::Unavailable,
                terminal: false,
                measured_rate_pairs: Vec::new(),
                case_keys: Vec::new(),
                evidence: Some(unavailable.reason.clone()),
            },
            (None, None) => {
                return Err(format!(
                    "coverage inventory omitted required engine '{engine_id}'"
                ));
            }
        };
        entries.push(entry);
    }

    let all_terminal = entries.iter().all(|entry| entry.terminal);
    Ok(CoverageTable {
        all_terminal,
        entries,
    })
}

fn validate_coverage(report: &ComparisonReport) -> Result<(), String> {
    let supported_rate_pairs = report
        .conditions
        .supported_rate_pairs_by_engine
        .iter()
        .map(|(engine_id, pairs)| {
            (
                engine_id.clone(),
                pairs.iter().cloned().collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let expected = build_coverage(
        report.cases.iter().map(|case| {
            (
                case.engine.engine_id.clone(),
                case.rate_pair.clone(),
                case.case_key.clone(),
            )
        }),
        &report.unavailable_engines,
        &supported_rate_pairs,
    )?;
    if report.coverage == expected {
        Ok(())
    } else {
        Err("coverage table does not match measured and unavailable report rows".to_string())
    }
}

fn enforce_complete_coverage(coverage: &CoverageTable) -> Result<(), String> {
    let nonterminal = coverage
        .entries
        .iter()
        .filter(|entry| !entry.state.is_terminal() || !entry.terminal)
        .map(|entry| {
            let evidence = entry.evidence.as_deref().unwrap_or("no evidence recorded");
            format!("{} ({evidence})", entry.engine_id)
        })
        .collect::<Vec<_>>();
    if coverage.all_terminal && nonterminal.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "representative resampler matrix has non-terminal coverage: {}",
            nonterminal.join("; ")
        ))
    }
}

fn enforce_required_engines(report: &ComparisonReport) -> Result<(), String> {
    let failures = report
        .unavailable_engines
        .iter()
        .filter(|engine| engine.required)
        .map(|engine| format!("{}: {}", engine.engine_id, engine.reason))
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "required comparison engines are unavailable: {}",
            failures.join("; ")
        ))
    }
}

fn enforce_formal_provenance(report: &ComparisonReport) -> Result<(), String> {
    enforce_identity_provenance(report.cases.iter().map(|case| &case.engine))
}

fn enforce_identity_provenance<'a>(
    identities: impl IntoIterator<Item = &'a EngineIdentity>,
) -> Result<(), String> {
    let mut invalid = BTreeMap::<String, String>::new();
    for identity in identities {
        let Some(native) = &identity.native_library else {
            continue;
        };
        if !native.provenance_verified {
            invalid.insert(
                identity.engine_id.clone(),
                "loaded native bytes do not match a pinned provenance identity".to_string(),
            );
        } else if native.source_revision.as_deref().is_none_or(str::is_empty)
            || native.build_provenance.as_deref().is_none_or(str::is_empty)
        {
            invalid.insert(
                identity.engine_id.clone(),
                "verified native identity is missing source revision or build provenance"
                    .to_string(),
            );
        }
    }
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "formal native provenance gate failed: {}",
            invalid
                .into_iter()
                .map(|(engine, reason)| format!("{engine}: {reason}"))
                .collect::<Vec<_>>()
                .join("; ")
        ))
    }
}

fn enforce_report(report: &ComparisonReport) -> Result<(), String> {
    validate_coverage(report)?;
    let mut seen = HashSet::new();
    let invalid = report
        .cases
        .iter()
        .filter(|case| {
            !seen.insert(case.case_key.clone())
                || validate_case_trials(case, &report.conditions).is_err()
                || latency::validate(case, report.conditions.iterations_per_trial).is_err()
                || !case.work_validation.valid
                || case.setup_ns.samples.len() != report.conditions.trials
                || case.steady_ns_per_input_sample.samples.len() != report.conditions.trials
                || case.steady_ns_per_input_buffer.samples.len() != report.conditions.trials
                || case.reset_ns.samples.len() != report.conditions.trials
                || case.drain_ns.samples.len() != report.conditions.trials
        })
        .map(|case| case.case_key.as_str())
        .collect::<Vec<_>>();
    if !invalid.is_empty() {
        return Err(format!(
            "resampler comparison validity gate failed for cases: {}",
            invalid.join(", ")
        ));
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "resampler comparison median regression gate failed",
        "ns/input-sample",
    ) {
        return Err(error);
    }
    Ok(())
}

fn validate_written_report(
    path: &std::path::Path,
    report: &ComparisonReport,
) -> Result<(), String> {
    let decoded: ComparisonReport = read_json(path, "resampler comparison report")?;
    let original = serde_json::to_value(report)
        .map_err(|error| format!("failed to normalize resampler comparison report: {error}"))?;
    let decoded = serde_json::to_value(decoded)
        .map_err(|error| format!("failed to normalize decoded comparison report: {error}"))?;
    if let Some(difference) = first_json_difference(&original, &decoded, "$") {
        return Err(format!(
            "resampler comparison JSON round trip changed '{}': {difference}",
            path.display(),
        ));
    }
    Ok(())
}

fn first_json_difference(
    original: &serde_json::Value,
    decoded: &serde_json::Value,
    path: &str,
) -> Option<String> {
    match (original, decoded) {
        (serde_json::Value::Object(original), serde_json::Value::Object(decoded)) => {
            for (key, value) in original {
                let next_path = format!("{path}.{key}");
                let Some(decoded_value) = decoded.get(key) else {
                    return Some(format!("{next_path} is missing after deserialization"));
                };
                if let Some(difference) = first_json_difference(value, decoded_value, &next_path) {
                    return Some(difference);
                }
            }
            decoded
                .keys()
                .find(|key| !original.contains_key(*key))
                .map(|key| format!("{path}.{key} was added after deserialization"))
        }
        (serde_json::Value::Array(original), serde_json::Value::Array(decoded)) => {
            if original.len() != decoded.len() {
                return Some(format!(
                    "{path} length changed from {} to {}",
                    original.len(),
                    decoded.len()
                ));
            }
            original
                .iter()
                .zip(decoded)
                .enumerate()
                .find_map(|(index, (original, decoded))| {
                    first_json_difference(original, decoded, &format!("{path}[{index}]"))
                })
        }
        (serde_json::Value::Number(original), serde_json::Value::Number(decoded))
            if original.is_f64() && decoded.is_f64() =>
        {
            let original = original.as_f64()?;
            let decoded = decoded.as_f64()?;
            (original.to_bits() != decoded.to_bits())
                .then(|| format!("{path} changed from {original} to {decoded}"))
        }
        _ if original == decoded => None,
        _ => Some(format!("{path} changed from {original} to {decoded}")),
    }
}

fn print_report(report: &ComparisonReport) -> Result<(), String> {
    println!(
        "{PROBE} mode={} engines={} unavailable={} coverage_terminal={} rates={} iterations={} trials={}",
        report.mode.as_str(),
        report.conditions.measured_engines.len(),
        report.unavailable_engines.len(),
        report.coverage.all_terminal,
        report.conditions.rate_pairs.join(","),
        report.conditions.iterations_per_trial,
        report.conditions.trials
    );
    println!(
        "audio_resampler_comparison_environment {}",
        environment_json(&report.environment)?
    );
    println!(
        "audio_resampler_comparison_note quality_policy={} output_capacity_policy={} output_capacities={:?} pinned={} pin_core={} scope_boundaries={}",
        report.conditions.quality_policy,
        report.conditions.output_capacity_policy,
        report.conditions.output_capacity_frames_by_rate,
        report.conditions.pinned,
        report
            .conditions
            .pin_core
            .map_or_else(|| "n/a".to_string(), |core| core.to_string()),
        report.conditions.scope_boundaries.join(",")
    );
    for entry in &report.coverage.entries {
        let evidence = entry
            .evidence
            .as_deref()
            .unwrap_or("measured_cases_in_report")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("_");
        println!(
            "resampler_comparison_coverage engine={} state={} terminal={} rate_pairs={} cases={} evidence={}",
            entry.engine_id,
            entry.state.as_str(),
            entry.terminal,
            entry.measured_rate_pairs.join(","),
            entry.case_keys.join(","),
            evidence
        );
    }
    for engine in &report.unavailable_engines {
        println!(
            "resampler_comparison_unavailable engine={} classification=skipped required={} reason={}",
            engine.engine_id,
            engine.required,
            engine.reason.replace(' ', "_")
        );
    }
    for case in &report.cases {
        let steady_cycles = case.steady_cycles_per_input_sample.as_ref().map_or_else(
            || "n/a".to_string(),
            |distribution| format!("{:.2}", distribution.median),
        );
        println!(
            "resampler_comparison case={} engine={} version={} format={} setup_us={:.3} steady_ns_per_input_sample={:.3} trial_average_p95={:.3} steady_cycles_per_input_sample={} reset_us={:.3} drain_us={:.3} api_buffering_latency_frames={} input_consumed_through_first_output={} impulse_peak_frame={} impulse_peak_magnitude={:.6e} gain_997_db={} gain_18k_db={} thdn_db={} alias_db={} quality_valid={} work_valid={}",
            case.case_key,
            case.engine.engine_id,
            case.engine.upstream_version.replace(' ', "_"),
            case.engine.sample_format.as_str(),
            case.setup_ns.median / 1_000.0,
            case.steady_ns_per_input_sample.median,
            case.steady_ns_per_input_sample.p95,
            steady_cycles,
            case.reset_ns.median / 1_000.0,
            case.drain_ns.median / 1_000.0,
            case.quality
                .reported_api_buffering_latency_frames
                .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
            case.quality
                .observed_input_frames_through_first_output
                .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
            case.quality.measured_impulse_peak_frame,
            case.quality.measured_impulse_peak_magnitude,
            case.quality
                .gain_997_hz_db
                .map_or_else(|| "n/a".to_string(), |value| format!("{value:.5}")),
            case.quality
                .gain_18_khz_db
                .map_or_else(|| "n/a".to_string(), |value| format!("{value:.5}")),
            case.quality
                .thdn_997_hz_db
                .map_or_else(|| "n/a".to_string(), |value| format!("{value:.2}")),
            case.quality.alias_attenuation_db.map_or_else(
                || "n/a".to_string(),
                |value| format!("{value:.2}")
            ),
            case.quality.valid,
            case.work_validation.valid
        );
        if let Some(latency) = &case.process_call_latency {
            let calls = &latency.process_call_ns;
            println!("resampler_comparison_calls case={} calls={} median_ns={:.1} p95_ns={:.1} p99_ns={:.1} max_ns={:.1} valid={}",
                case.case_key,calls.samples.len(),calls.median,calls.p95,calls.p99,calls.max,latency.valid);
        }
        let grid = &case.quality.grid;
        println!("resampler_comparison_grid case={} renders={} response_points={} alias_points={} image_points={} channel_pairs={} checked_output_samples={}",
            case.case_key,grid.renders_checked,grid.response.len(),grid.aliases.len(),grid.images.len(),grid.channel_isolation.len(),grid.output_samples_checked);
        for error in &case.quality.validity_errors {
            println!(
                "resampler_comparison_quality_error case={} reason={}",
                case.case_key,
                error.replace(' ', "_")
            );
        }
    }
    if let Some(soak) = &report.soak {
        println!(
            "resampler_comparison_soak rounds_per_case={} iterations_per_round={} cases_soaked={} all_round_outputs_bit_identical={} working_set_before_bytes={} working_set_after_bytes={} persistent_adapter={} memory=report_only",
            soak.rounds_per_case,
            soak.iterations_per_round,
            soak.cases_soaked,
            soak.all_round_outputs_bit_identical,
            soak.working_set_before_bytes
                .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
            soak.working_set_after_bytes
                .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
            soak.persistent_adapter_per_case
        );
    }
    for comparison in &report.comparisons {
        println!(
            "resampler_comparison_baseline case={} baseline_median={:.3} candidate_median={:.3} regression_pct={:.3} threshold_pct={:.3} passed={}",
            comparison.case_key,
            comparison.baseline_median,
            comparison.candidate_median,
            comparison.regression_pct,
            comparison.threshold_pct,
            comparison.passed
        );
    }
    for failure in &report.run_failures {
        println!(
            "resampler_comparison_run_failure reason={}",
            failure.replace(' ', "_")
        );
    }
    Ok(())
}

fn print_help() {
    println!("Resource-only report: add --resources [--resource-engines=id,id] [--resource-ms=250]. No frequency-response rerun; use --resources --help for details.");
    println!(
        "Usage: cargo bench --bench audio_resampler_comparison_perf --all-features -- \
         [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] \
         [--max-median-regression-pct <pct>] [--libsamplerate <absolute-path>] \
         [--engine-library <engine-id>=<absolute-shim-path>] \
         [--require-engine <engine-id>] [--require-complete-matrix] \
         [--pinned] [--pin-core <core>]\n\
         \n\
         Engines: audio_engine_core, raw_libsoxr, libsamplerate, \
         ffmpeg_libswresample, speexdsp, r8brain, zita_resampler, webrtc, wdl, \
         libresample.\n\
         Cross-engine quality/latency/throughput is report-only. --enforce gates \
         complete finite work and compatible same-engine baselines. A required \
         unavailable engine always fails after any requested JSON is written. \
         --require-complete-matrix likewise writes JSON first, then rejects any \
         non-terminal representative-project coverage row."
    );
}

pub(crate) fn rounded_output_frames(input_frames: usize, from_hz: u32, to_hz: u32) -> usize {
    let numerator = (input_frames as u128) * (to_hz as u128);
    let denominator = from_hz as u128;
    usize::try_from((numerator + denominator / 2) / denominator).unwrap_or(usize::MAX)
}

fn synthetic_buffer(frames: usize, channels: usize, sample_rate: u32) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    let sample_rate = sample_rate as f64;
    for frame in 0..frames {
        let t = frame as f64 / sample_rate;
        let left = 0.55 * (std::f64::consts::TAU * 997.0 * t).sin()
            + 0.08 * (std::f64::consts::TAU * 7_031.0 * t).sin();
        let right = 0.52 * (std::f64::consts::TAU * 1_331.0 * t).sin()
            - 0.07 * (std::f64::consts::TAU * 5_521.0 * t).cos();
        out.push(left);
        if channels > 1 {
            out.push(right);
        }
        for channel in 2..channels {
            out.push(left * (1.0 - channel as f64 * 0.05));
        }
    }
    out
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::*;

    #[test]
    fn complete_output_oracle_catches_early_drain_differences_and_reset_replays() {
        let rate = RATE_PAIRS[0];
        let inputs = PreparedInputs::new(rate);
        let mut left = EngineAdapter::chunked_drain_test(rate, 1.0);
        let mut right = EngineAdapter::chunked_drain_test(rate, 2.0);
        let a = drive_complete_oracle_stream(&mut left, &inputs, 2048, 1).unwrap();
        let b = drive_complete_oracle_stream(&mut right, &inputs, 2048, 1).unwrap();
        assert_eq!(
            a.len(),
            rounded_output_frames(CHUNK_FRAMES, rate.from_hz, rate.to_hz) * rate.channels
        );
        assert_eq!(a.last(), b.last());
        assert_ne!(a, b);
        assert_ne!(SoakDigest::of_bits(&a), SoakDigest::of_bits(&b));
        left.reset().unwrap();
        assert_eq!(
            a,
            drive_complete_oracle_stream(&mut left, &inputs, 2048, 1).unwrap()
        );
    }

    #[test]
    fn comparison_args_extract_native_options_before_perf_args() {
        let args = ComparisonArgs::parse_with_environment(
            vec![
                "--quick".to_string(),
                "--libsamplerate".to_string(),
                "C:/bench/libsamplerate.dll".to_string(),
                "--require-engine=libsamplerate".to_string(),
                "--engine-library".to_string(),
                "speexdsp=C:/bench/speexdsp-shim.dll".to_string(),
                "--require-complete-matrix".to_string(),
                "--enforce".to_string(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(args.perf.mode, BenchMode::Quick);
        assert!(args.perf.enforce);
        assert_eq!(
            args.libsamplerate_path,
            Some(PathBuf::from("C:/bench/libsamplerate.dll"))
        );
        assert_eq!(
            args.libsamplerate_path_source,
            Some(LibraryPathSource::Argument)
        );
        assert!(args.required_engines.contains(LIBSAMPLERATE_ENGINE_ID));
        assert!(args.require_complete_matrix);
        assert_eq!(
            args.native_library_paths.get(SPEEXDSP_ENGINE_ID),
            Some(&PathBuf::from("C:/bench/speexdsp-shim.dll"))
        );
    }

    #[test]
    fn comparison_args_use_explicit_environment_fallback() {
        let args = ComparisonArgs::parse_with_environment(
            vec!["--quick".to_string()],
            Some(OsString::from("D:/cache/libsamplerate.dll")),
        )
        .unwrap();
        assert_eq!(
            args.libsamplerate_path,
            Some(PathBuf::from("D:/cache/libsamplerate.dll"))
        );
        assert_eq!(
            args.libsamplerate_path_source,
            Some(LibraryPathSource::Environment)
        );
    }

    #[test]
    fn comparison_args_reject_unknown_required_engine() {
        let error = ComparisonArgs::parse_with_environment(
            vec!["--require-engine=imaginary".to_string()],
            None,
        )
        .unwrap_err();
        assert!(error.contains("unknown engine id 'imaginary'"), "{error}");
    }

    #[test]
    fn comparison_args_reject_duplicate_or_non_shim_library_paths() {
        let duplicate = ComparisonArgs::parse_with_environment(
            vec![
                "--engine-library=wdl=C:/bench/one.dll".to_string(),
                "--engine-library=wdl=C:/bench/two.dll".to_string(),
            ],
            None,
        )
        .unwrap_err();
        assert!(
            duplicate.contains("may be supplied only once"),
            "{duplicate}"
        );

        let direct = ComparisonArgs::parse_with_environment(
            vec!["--engine-library=libsamplerate=C:/bench/src.dll".to_string()],
            None,
        )
        .unwrap_err();
        assert!(direct.contains("does not use --engine-library"), "{direct}");
    }

    #[test]
    fn rounded_output_uses_exact_integer_ratio_math() {
        assert_eq!(rounded_output_frames(44_100, 44_100, 48_000), 48_000);
        assert_eq!(rounded_output_frames(48_000, 48_000, 44_100), 44_100);
        assert_eq!(rounded_output_frames(16_384, 44_100, 48_000), 17_833);
    }

    #[test]
    fn exact_work_gate_rejects_one_frame_complete_stream_truncation() {
        let rate = RATE_PAIRS[0];
        let expected_warmup = CHUNK_FRAMES * WARMUP_BUFFERS;
        let expected_timed = CHUNK_FRAMES;
        let expected_output =
            rounded_output_frames(expected_warmup + expected_timed, rate.from_hz, rate.to_hz);
        let identity = adapters::EngineFactory::silent_test().identity().clone();
        let mut accumulator = CaseAccumulator::new(identity.clone(), rate, 2_048, 1);
        accumulator
            .push(TrialRecord {
                identity: MeasurementIdentity::new(&identity, rate, 2_048),
                trial_index: 0,
                measurement: TrialMeasurement {
                    setup_ns: 1.0,
                    steady_ns_per_input_sample: 1.0,
                    steady_ns_per_input_buffer: 1.0,
                    steady_cycles_per_input_sample: None,
                    steady_cycles_per_input_buffer: None,
                    reset_ns: 1.0,
                    drain_ns: 1.0,
                    warmup_consumed_frames: expected_warmup,
                    timed_consumed_frames: expected_timed,
                    expected_complete_output_frames: expected_output,
                    warmup_output_frames: 0,
                    steady_output_frames: expected_output - 1,
                    drain_frames: 0,
                    actual_complete_output_frames: expected_output - 1,
                    drain_terminal: true,
                },
            })
            .unwrap();

        let case = accumulator.finish(1, valid_test_quality()).unwrap();
        assert!(!case.work_validation.valid);
        assert_eq!(
            case.work_validation
                .expected_complete_output_frames_per_trial,
            vec![expected_output]
        );
        assert_eq!(
            case.work_validation.actual_complete_output_frames_per_trial,
            vec![expected_output - 1]
        );
    }

    #[test]
    fn formal_provenance_rejects_unverified_native_identity() {
        let mut identity = adapters::EngineFactory::silent_test().identity().clone();
        identity.engine_id = "unverified_native_test".to_string();
        identity.native_library = Some(NativeLibraryIdentity {
            canonical_path: "C:/benchmark/unverified.dll".to_string(),
            upstream_version: "test".to_string(),
            sha256: "0".repeat(64),
            file_bytes: 1,
            source_revision: Some("test-revision".to_string()),
            build_provenance: Some("test-build".to_string()),
            linked_artifacts: Vec::new(),
            provenance_verified: false,
        });

        let error = enforce_identity_provenance([&identity]).unwrap_err();
        assert!(error.contains("unverified_native_test"), "{error}");
        assert!(
            error.contains("do not match a pinned provenance"),
            "{error}"
        );
    }

    #[test]
    fn baseline_and_incomplete_matrix_failures_still_persist_json() {
        let suffix = format!("{}-{}", std::process::id(), generated_unix_ms());
        let artifact_directory = PathBuf::from("target/benchmark-test-artifacts");
        let baseline_path = artifact_directory.join(format!("missing-baseline-{suffix}.json"));
        let output_path = artifact_directory.join(format!("partial-report-{suffix}.json"));
        let _ = std::fs::remove_file(&baseline_path);
        let _ = std::fs::remove_file(&output_path);

        let result = run(vec![
            "--quick".to_string(),
            "--baseline".to_string(),
            baseline_path.display().to_string(),
            "--out".to_string(),
            output_path.display().to_string(),
            "--require-complete-matrix".to_string(),
        ]);
        let error = result.unwrap_err();
        assert!(error.contains("baseline validation failed"), "{error}");
        assert!(
            error.contains("representative resampler matrix has non-terminal coverage"),
            "{error}"
        );

        let report: serde_json::Value =
            read_json(&output_path, "persisted partial benchmark report").unwrap();
        let failures = report["run_failures"].as_array().unwrap();
        assert!(failures.iter().any(|failure| {
            failure
                .as_str()
                .is_some_and(|failure| failure.contains("baseline validation failed"))
        }));
        assert!(failures.iter().any(|failure| {
            failure.as_str().is_some_and(|failure| {
                failure.contains("representative resampler matrix has non-terminal coverage")
            })
        }));
        let conditions = report["conditions"].as_object().unwrap();
        assert!(conditions.contains_key("scope_boundaries"));
        assert!(!conditions.contains_key("excludes"));

        let decoded: ComparisonReport = serde_json::from_value(report).unwrap();
        let original = decoded
            .cases
            .first()
            .expect("project case must be measured");
        validate_case_trials(original, &decoded.conditions).unwrap();
        let mut forged = original.clone();
        forged.trial_records[0]
            .identity
            .engine_id
            .push_str("_wrong");
        assert!(validate_case_trials(&forged, &decoded.conditions).is_err());
        let mut forged = original.clone();
        forged
            .work_validation
            .actual_complete_output_frames_per_trial[0] += 1;
        forged.work_validation.valid = true;
        assert!(validate_case_trials(&forged, &decoded.conditions).is_err());
        let mut forged = original.clone();
        forged.steady_ns_per_input_sample.median *= 0.5;
        assert!(validate_case_trials(&forged, &decoded.conditions).is_err());
        let mut forged = original.clone();
        forged.trial_records.pop();
        assert!(validate_case_trials(&forged, &decoded.conditions).is_err());

        std::fs::remove_file(&output_path).unwrap();
    }

    fn valid_test_quality() -> QualitySummary {
        QualitySummary {
            classification: MetricClassification::Gate,
            valid: true,
            input_frames: 1,
            expected_complete_output_frames: 1,
            actual_complete_output_frames: 1,
            all_output_samples_finite: true,
            reported_api_buffering_latency_frames: None,
            observed_input_frames_before_first_output: Some(0),
            observed_input_frames_through_first_output: Some(512),
            first_output_during_drain: false,
            measured_impulse_peak_frame: 0,
            measured_impulse_peak_magnitude: 1.0,
            gain_997_hz_db: Some(0.0),
            gain_18_khz_db: Some(0.0),
            passband_max_abs_deviation_db: Some(0.0),
            thdn_997_hz_db: Some(-100.0),
            stopband_input_hz: Some(30_000.0),
            folded_alias_hz: Some(18_000.0),
            alias_attenuation_db: Some(-100.0),
            validity_errors: Vec::new(),
            grid: quality_grid::QualityGrid::default(),
        }
    }

    #[test]
    fn coverage_table_has_one_terminal_row_per_representative_engine() {
        let supported_rate_pairs = ALL_ENGINE_IDS
            .into_iter()
            .map(|engine_id| {
                (
                    engine_id.to_string(),
                    RATE_PAIRS
                        .iter()
                        .map(|rate| rate.id.to_string())
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let measured_cases = ALL_ENGINE_IDS.into_iter().flat_map(|engine_id| {
            RATE_PAIRS.into_iter().map(move |rate| {
                (
                    engine_id.to_string(),
                    rate.id.to_string(),
                    format!("engine={engine_id};rate={}", rate.id),
                )
            })
        });
        let coverage = build_coverage(measured_cases, &[], &supported_rate_pairs).unwrap();

        assert!(coverage.all_terminal);
        assert_eq!(coverage.entries.len(), ALL_ENGINE_IDS.len());
        assert!(coverage.entries.iter().all(|entry| {
            entry.state == CoverageState::Measured
                && entry.terminal
                && entry.measured_rate_pairs.len() == RATE_PAIRS.len()
                && entry.case_keys.len() == RATE_PAIRS.len()
                && entry.evidence.is_none()
        }));
        enforce_complete_coverage(&coverage).unwrap();
    }

    #[test]
    fn coverage_table_keeps_partially_supported_engine_terminal_on_its_support_set() {
        let supported_rate_pairs = ALL_ENGINE_IDS
            .into_iter()
            .map(|engine_id| {
                let pairs = if engine_id == WEBRTC_ENGINE_ID {
                    RATE_PAIRS
                        .iter()
                        .filter(|rate| rate.channels <= 2)
                        .map(|rate| rate.id.to_string())
                        .collect::<BTreeSet<_>>()
                } else {
                    RATE_PAIRS
                        .iter()
                        .map(|rate| rate.id.to_string())
                        .collect::<BTreeSet<_>>()
                };
                (engine_id.to_string(), pairs)
            })
            .collect::<BTreeMap<_, _>>();
        let measured_cases = ALL_ENGINE_IDS.into_iter().flat_map(|engine_id| {
            let pairs = supported_rate_pairs[engine_id].clone();
            pairs.into_iter().map(move |rate_pair| {
                (
                    engine_id.to_string(),
                    rate_pair.clone(),
                    format!("engine={engine_id};rate={rate_pair}"),
                )
            })
        });
        let coverage = build_coverage(measured_cases, &[], &supported_rate_pairs).unwrap();

        assert!(coverage.all_terminal);
        let webrtc = coverage
            .entries
            .iter()
            .find(|entry| entry.engine_id == WEBRTC_ENGINE_ID)
            .unwrap();
        assert_eq!(webrtc.state, CoverageState::Measured);
        assert!(webrtc.terminal);
        assert!(webrtc.measured_rate_pairs.len() < RATE_PAIRS.len());
        assert_eq!(webrtc.case_keys.len(), webrtc.measured_rate_pairs.len());
        enforce_complete_coverage(&coverage).unwrap();
    }

    #[test]
    fn coverage_table_keeps_unavailable_engine_nonterminal() {
        let supported_rate_pairs = ALL_ENGINE_IDS
            .into_iter()
            .map(|engine_id| {
                (
                    engine_id.to_string(),
                    RATE_PAIRS
                        .iter()
                        .map(|rate| rate.id.to_string())
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let measured_cases = ALL_ENGINE_IDS
            .into_iter()
            .filter(|engine_id| *engine_id != LIBRESAMPLE_ENGINE_ID)
            .flat_map(|engine_id| {
                RATE_PAIRS.into_iter().map(move |rate| {
                    (
                        engine_id.to_string(),
                        rate.id.to_string(),
                        format!("engine={engine_id};rate={}", rate.id),
                    )
                })
            });
        let unavailable = [UnavailableEngine {
            engine_id: LIBRESAMPLE_ENGINE_ID.to_string(),
            classification: MetricClassification::Skipped,
            required: true,
            reason: "explicit libresample shim path was not supplied".to_string(),
        }];
        let coverage = build_coverage(measured_cases, &unavailable, &supported_rate_pairs).unwrap();

        assert!(!coverage.all_terminal);
        let libresample = coverage
            .entries
            .iter()
            .find(|entry| entry.engine_id == LIBRESAMPLE_ENGINE_ID)
            .unwrap();
        assert_eq!(libresample.state, CoverageState::Unavailable);
        assert!(!libresample.terminal);
        assert!(libresample.case_keys.is_empty());
        let error = enforce_complete_coverage(&coverage).unwrap_err();
        assert!(error.contains(LIBRESAMPLE_ENGINE_ID), "{error}");
    }

    #[test]
    fn coverage_table_rejects_partial_measured_rate_pairs() {
        let supported_rate_pairs = ALL_ENGINE_IDS
            .into_iter()
            .map(|engine_id| {
                (
                    engine_id.to_string(),
                    RATE_PAIRS
                        .iter()
                        .map(|rate| rate.id.to_string())
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let measured_cases = ALL_ENGINE_IDS.into_iter().flat_map(|engine_id| {
            RATE_PAIRS
                .into_iter()
                .filter(move |rate| engine_id != WDL_ENGINE_ID || rate.id == RATE_PAIRS[0].id)
                .map(move |rate| {
                    (
                        engine_id.to_string(),
                        rate.id.to_string(),
                        format!("engine={engine_id};rate={}", rate.id),
                    )
                })
        });
        let error = build_coverage(measured_cases, &[], &supported_rate_pairs).unwrap_err();
        assert!(error.contains(WDL_ENGINE_ID), "{error}");
        assert!(
            error.contains("incomplete measured rate coverage"),
            "{error}"
        );
    }

    #[test]
    fn json_round_trip_comparison_rejects_even_one_bit_changes() {
        let original = serde_json::json!({
            "identity": "engine",
            "samples": [1.0, 2.0],
        });
        assert!(first_json_difference(&original, &original, "$").is_none());
        let one_bit_change = serde_json::json!({
            "identity": "engine",
            "samples": [1.0 + f64::EPSILON, 2.0],
        });
        let difference = first_json_difference(&original, &one_bit_change, "$").unwrap();
        assert!(difference.contains("$.samples[0]"), "{difference}");

        let larger_change = serde_json::json!({
            "identity": "engine",
            "samples": [1.0 + 8.0 * f64::EPSILON, 2.0],
        });
        let difference = first_json_difference(&original, &larger_change, "$").unwrap();
        assert!(difference.contains("$.samples[0]"), "{difference}");
        assert!(
            first_json_difference(&serde_json::json!(-0.0), &serde_json::json!(0.0), "$").is_some()
        );

        let changed_identity = serde_json::json!({
            "identity": "other",
            "samples": [1.0, 2.0],
        });
        let difference = first_json_difference(&original, &changed_identity, "$").unwrap();
        assert!(difference.contains("$.identity"), "{difference}");
    }
}
