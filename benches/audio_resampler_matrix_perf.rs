//! Resampler configuration matrix: rates × quality × phase × channels.
//!
//! Complements `audio_resampler_streaming_perf` (default High/Linear streaming
//! path only) with a broader process_checked matrix plus setup cost. Case keys
//! and algorithm labels include the compile-time backend so soxr/rubato reports
//! stay baseline-incompatible by design.
//!
//! Coverage is intentional, not exhaustive: decoder, device write, and full
//! DSP chains remain out of scope (see report `excludes`).

use std::hint::black_box;
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub mod support;

use support::signals::resampler_test_buffer;
use support::{
    compare_case_medians, environment_json, generated_unix_ms, read_json, regression_gate_error,
    summarize_trials, validate_performance_baseline, write_json, BenchEnvironment, BenchMode,
    PerfArgs, PerformanceReportIdentity, RegressionComparison, TrialDistribution,
    REPORT_SCHEMA_VERSION,
};

use audio_engine_core::config::{PhaseResponse, ResampleQuality};
use audio_engine_core::processor::{
    process_checked, AudioBlockMut, AudioBlockRef, ProcessBuffers, StreamingResampler,
    RESAMPLER_BACKEND_NAME,
};

const WARMUP_BUFFERS: usize = 32;
/// Absolute output-frame slack on top of the ±5% relative bound. Downsampling
/// filters buffer/release around a block of latency per run, which dominates
/// the relative bound at quick-mode iteration counts.
const OUTPUT_FRAMES_ABS_SLACK: usize = 2048;
const VALIDATION_BUFFERS: usize = 4;
const MATRIX_PROBE: &str = "audio_resampler_matrix_perf";
const MATRIX_ALGORITHM_ID: &str = "matrix_process_checked_v4_nonlinear_polyphase_up16";

#[derive(Clone, Copy)]
struct RatePair {
    name: &'static str,
    from_rate: u32,
    to_rate: u32,
}

#[derive(Clone, Copy)]
struct QualityTier {
    name: &'static str,
    quality: ResampleQuality,
}

#[derive(Clone, Copy)]
struct PhaseTier {
    name: &'static str,
    phase: PhaseResponse,
}

#[derive(Clone, Copy)]
struct ChannelTier {
    name: &'static str,
    channels: usize,
}

/// Full/heavy: broader commercial rate grid (still not every pathological pair).
/// Quick mode uses the explicit decision set in `quick_matrix_specs` instead.
const FULL_RATE_PAIRS: [RatePair; 8] = [
    RatePair {
        name: "music_44k1_to_48k",
        from_rate: 44_100,
        to_rate: 48_000,
    },
    RatePair {
        name: "music_48k_to_44k1",
        from_rate: 48_000,
        to_rate: 44_100,
    },
    RatePair {
        name: "upsample_48k_to_96k",
        from_rate: 48_000,
        to_rate: 96_000,
    },
    RatePair {
        name: "hires_96k_to_48k",
        from_rate: 96_000,
        to_rate: 48_000,
    },
    RatePair {
        name: "upsample_44k1_to_88k2",
        from_rate: 44_100,
        to_rate: 88_200,
    },
    RatePair {
        name: "hires_192k_to_48k",
        from_rate: 192_000,
        to_rate: 48_000,
    },
    RatePair {
        name: "upsample_48k_to_192k",
        from_rate: 48_000,
        to_rate: 192_000,
    },
    RatePair {
        name: "equal_rate_48k",
        from_rate: 48_000,
        to_rate: 48_000,
    },
];

const FULL_QUALITIES: [QualityTier; 4] = [
    QualityTier {
        name: "low",
        quality: ResampleQuality::Low,
    },
    QualityTier {
        name: "standard",
        quality: ResampleQuality::Standard,
    },
    QualityTier {
        name: "high",
        quality: ResampleQuality::High,
    },
    QualityTier {
        name: "ultrahigh",
        quality: ResampleQuality::UltraHigh,
    },
];

const FULL_PHASES: [PhaseTier; 3] = [
    PhaseTier {
        name: "linear",
        phase: PhaseResponse::Linear,
    },
    PhaseTier {
        name: "minimum",
        phase: PhaseResponse::Minimum,
    },
    PhaseTier {
        name: "maximum",
        phase: PhaseResponse::Maximum,
    },
];

const FULL_CHANNELS: [ChannelTier; 3] = [
    ChannelTier {
        name: "mono",
        channels: 1,
    },
    ChannelTier {
        name: "stereo",
        channels: 2,
    },
    ChannelTier {
        name: "surround51",
        channels: 6,
    },
];

const FULL_FRAMES: [usize; 3] = [128, 256, 512];

/// Cartesian product is too large; quick mode uses an explicit decision set.
#[derive(Clone, Copy)]
struct MatrixCaseSpec {
    rate: RatePair,
    quality: QualityTier,
    phase: PhaseTier,
    channels: ChannelTier,
    frames: usize,
}

fn quick_matrix_specs() -> Vec<MatrixCaseSpec> {
    // Decision-oriented set: default playback, exact-2x, downsample, multi-ch,
    // UltraHigh cost, nonlinear phase, equal-rate bypass.
    let high = QualityTier {
        name: "high",
        quality: ResampleQuality::High,
    };
    let ultra = QualityTier {
        name: "ultrahigh",
        quality: ResampleQuality::UltraHigh,
    };
    let standard = QualityTier {
        name: "standard",
        quality: ResampleQuality::Standard,
    };
    let linear = PhaseTier {
        name: "linear",
        phase: PhaseResponse::Linear,
    };
    let minimum = PhaseTier {
        name: "minimum",
        phase: PhaseResponse::Minimum,
    };
    let stereo = ChannelTier {
        name: "stereo",
        channels: 2,
    };
    let surround = ChannelTier {
        name: "surround51",
        channels: 6,
    };
    let r44_48 = RatePair {
        name: "music_44k1_to_48k",
        from_rate: 44_100,
        to_rate: 48_000,
    };
    let r48_96 = RatePair {
        name: "upsample_48k_to_96k",
        from_rate: 48_000,
        to_rate: 96_000,
    };
    let r96_48 = RatePair {
        name: "hires_96k_to_48k",
        from_rate: 96_000,
        to_rate: 48_000,
    };
    let r_eq = RatePair {
        name: "equal_rate_48k",
        from_rate: 48_000,
        to_rate: 48_000,
    };

    vec![
        // Default High/Linear stereo across primary rates + block sizes
        MatrixCaseSpec {
            rate: r44_48,
            quality: high,
            phase: linear,
            channels: stereo,
            frames: 256,
        },
        MatrixCaseSpec {
            rate: r44_48,
            quality: high,
            phase: linear,
            channels: stereo,
            frames: 512,
        },
        MatrixCaseSpec {
            rate: r48_96,
            quality: high,
            phase: linear,
            channels: stereo,
            frames: 512,
        },
        MatrixCaseSpec {
            rate: r96_48,
            quality: high,
            phase: linear,
            channels: stereo,
            frames: 512,
        },
        MatrixCaseSpec {
            rate: r_eq,
            quality: high,
            phase: linear,
            channels: stereo,
            frames: 512,
        },
        // Quality ladder at 44.1→48 / 512
        MatrixCaseSpec {
            rate: r44_48,
            quality: standard,
            phase: linear,
            channels: stereo,
            frames: 512,
        },
        MatrixCaseSpec {
            rate: r44_48,
            quality: ultra,
            phase: linear,
            channels: stereo,
            frames: 512,
        },
        // Nonlinear phase + multi-channel
        MatrixCaseSpec {
            rate: r48_96,
            quality: high,
            phase: minimum,
            channels: stereo,
            frames: 512,
        },
        MatrixCaseSpec {
            rate: r44_48,
            quality: high,
            phase: minimum,
            channels: stereo,
            frames: 512,
        },
        MatrixCaseSpec {
            rate: r44_48,
            quality: high,
            phase: linear,
            channels: surround,
            frames: 512,
        },
        // UltraHigh on exact-2x (offline-render relevant)
        MatrixCaseSpec {
            rate: r48_96,
            quality: ultra,
            phase: linear,
            channels: stereo,
            frames: 512,
        },
    ]
}

fn expanded_matrix_specs(mode: BenchMode) -> Vec<MatrixCaseSpec> {
    if matches!(mode, BenchMode::Quick) {
        return quick_matrix_specs();
    }

    // Full/heavy still avoid a pure cartesian product: quality×phase only on
    // stereo 512-frame primary rates; multi-channel only High/Linear; Low only
    // on 44.1→48 stereo.
    let mut specs = Vec::new();

    let primary_rates = [
        "music_44k1_to_48k",
        "upsample_48k_to_96k",
        "hires_96k_to_48k",
        "music_48k_to_44k1",
    ];

    for &rate in &FULL_RATE_PAIRS {
        for &ch in &FULL_CHANNELS {
            for &frame in &FULL_FRAMES {
                // Always cover High/Linear for every rate×channel×frame.
                specs.push(MatrixCaseSpec {
                    rate,
                    quality: QualityTier {
                        name: "high",
                        quality: ResampleQuality::High,
                    },
                    phase: PhaseTier {
                        name: "linear",
                        phase: PhaseResponse::Linear,
                    },
                    channels: ch,
                    frames: frame,
                });
            }
        }
    }

    // Quality + phase ladder on stereo 512 primary rates.
    for &rate in &FULL_RATE_PAIRS {
        if !primary_rates.contains(&rate.name) {
            continue;
        }
        for &quality in &FULL_QUALITIES {
            for &phase in &FULL_PHASES {
                if quality.name == "high" && phase.name == "linear" {
                    continue; // already covered
                }
                // Low only on 44.1→48 to keep runtime bounded.
                if quality.name == "low" && rate.name != "music_44k1_to_48k" {
                    continue;
                }
                // Maximum phase only with High on full grid.
                if phase.name == "maximum" && quality.name != "high" {
                    continue;
                }
                specs.push(MatrixCaseSpec {
                    rate,
                    quality,
                    phase,
                    channels: ChannelTier {
                        name: "stereo",
                        channels: 2,
                    },
                    frames: 512,
                });
            }
        }
    }

    specs
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct MatrixConditions {
    coverage: String,
    excludes: Vec<String>,
    algorithm_id: String,
    warmup_buffers: usize,
    iterations_per_trial: usize,
    trials: usize,
    case_count: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct MatrixWorkValidation {
    valid: bool,
    validation_buffers: usize,
    consumed_frames: usize,
    expected_consumed_frames: usize,
    produced_frames: usize,
    all_output_samples_finite: bool,
    init_ok: bool,
    init_error: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct MatrixCase {
    case_key: String,
    scenario: String,
    quality: String,
    phase: String,
    channels_label: String,
    channels: usize,
    frames: usize,
    input_samples: usize,
    from_rate_hz: u32,
    to_rate_hz: u32,
    backend: String,
    algorithm: String,
    /// Construction cost (StreamingResampler::with_quality), median over trials.
    setup_ns: TrialDistribution,
    ns_per_input_sample: TrialDistribution,
    ns_per_input_buffer: TrialDistribution,
    source_buffer_duration_ns: f64,
    median_source_realtime_utilization_pct: f64,
    p95_source_realtime_utilization_pct: f64,
    expected_output_frames_total: usize,
    minimum_output_frames_total: usize,
    maximum_output_frames_total: usize,
    output_frames_total_per_trial: Vec<usize>,
    consumed_frames_total_per_trial: Vec<usize>,
    work_validation: MatrixWorkValidation,
}

#[derive(Debug, Deserialize, Serialize)]
struct BaselineReference {
    path: String,
    revision: String,
    dirty: Option<bool>,
    generated_unix_ms: u128,
    max_median_regression_pct: f64,
}

#[derive(Debug, Deserialize, Serialize)]
struct MatrixReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: MatrixConditions,
    cases: Vec<MatrixCase>,
    baseline: Option<BaselineReference>,
    comparisons: Vec<RegressionComparison>,
}

struct TrialMeasurement {
    setup_ns: f64,
    ns_per_input_sample: f64,
    ns_per_input_buffer: f64,
    output_frames_total: usize,
    consumed_frames_total: usize,
}

fn main() -> Result<(), String> {
    let args = PerfArgs::parse(std::env::args().skip(1).collect())?;
    if args.help {
        print_help();
        return Ok(());
    }

    let (iterations, trials) = workload(args.mode);
    let specs = expanded_matrix_specs(args.mode);
    let environment = BenchEnvironment::capture();
    let mut cases = Vec::with_capacity(specs.len());
    for spec in &specs {
        cases.push(benchmark_case(spec, iterations, trials)?);
    }

    let conditions = MatrixConditions {
        coverage: "resampler_config_matrix_process_checked".to_string(),
        excludes: [
            "decoder",
            "callback_dsp_chain",
            "cpal_device_write",
            "gapless_state_machine",
            "trait_process_direct",
            "offline_output_render_chain",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        algorithm_id: MATRIX_ALGORITHM_ID.to_string(),
        warmup_buffers: WARMUP_BUFFERS,
        iterations_per_trial: iterations,
        trials,
        case_count: cases.len(),
    };

    let mut report = MatrixReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: MATRIX_PROBE.to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment,
        conditions,
        cases,
        baseline: None,
        comparisons: Vec::new(),
    };

    if let Some(path) = &args.baseline {
        let baseline: MatrixReport = read_json(path, "resampler matrix baseline report")?;
        report.comparisons =
            compare_with_baseline(&report, &baseline, args.max_median_regression_pct)?;
        report.baseline = Some(BaselineReference {
            path: path.display().to_string(),
            revision: baseline.environment.revision,
            dirty: baseline.environment.dirty,
            generated_unix_ms: baseline.generated_unix_ms,
            max_median_regression_pct: args.max_median_regression_pct,
        });
    }

    print_report(&report)?;
    if let Some(path) = &args.out {
        write_json(path, &report, "resampler matrix performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }

    Ok(())
}

fn workload(mode: BenchMode) -> (usize, usize) {
    match mode {
        BenchMode::Quick => (40, 5),
        BenchMode::Full => (200, 7),
        BenchMode::Heavy => (600, 11),
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_resampler_matrix_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>]\n\
         \n\
         Configuration matrix for StreamingResampler::process_checked across rates,\n\
         quality, phase, and channel counts, plus construction (setup) cost.\n\
         Complements audio_resampler_streaming_perf; does not replace it.\n\
         Timing is report-only unless a compatible same-machine baseline is supplied.\n\
         Build with default features for soxr, or:\n\
           cargo bench --bench audio_resampler_matrix_perf --no-default-features --features rubato -- --quick"
    );
}

fn print_report(report: &MatrixReport) -> Result<(), String> {
    println!(
        "audio_resampler_matrix_perf mode={} backend={} cases={} iterations={} trials={}",
        report.mode.as_str(),
        RESAMPLER_BACKEND_NAME,
        report.conditions.case_count,
        report.conditions.iterations_per_trial,
        report.conditions.trials
    );
    println!(
        "audio_resampler_matrix_environment {}",
        environment_json(&report.environment)?
    );
    println!(
        "audio_resampler_matrix_note excludes={} utilization_reference=source_buffer_duration coverage={}",
        report.conditions.excludes.join(","),
        report.conditions.coverage
    );
    for case in &report.cases {
        println!(
            "resampler_matrix case={} scenario={} quality={} phase={} channels={} frames={} from_rate={} to_rate={} setup_ns_median={:.0} ns_per_input_sample_min={:.3} ns_per_input_sample_median={:.3} ns_per_input_sample_p95={:.3} ns_per_input_sample_max={:.3} ns_per_input_buffer_median={:.3} median_source_realtime_utilization_pct={:.4} p95_source_realtime_utilization_pct={:.4} init_ok={}",
            case.case_key,
            case.scenario,
            case.quality,
            case.phase,
            case.channels,
            case.frames,
            case.from_rate_hz,
            case.to_rate_hz,
            case.setup_ns.median,
            case.ns_per_input_sample.min,
            case.ns_per_input_sample.median,
            case.ns_per_input_sample.p95,
            case.ns_per_input_sample.max,
            case.ns_per_input_buffer.median,
            case.median_source_realtime_utilization_pct,
            case.p95_source_realtime_utilization_pct,
            case.work_validation.init_ok
        );
    }
    for comparison in &report.comparisons {
        println!(
            "resampler_matrix_baseline case={} baseline_median={:.3} candidate_median={:.3} regression_pct={:.3} threshold_pct={:.3} passed={}",
            comparison.case_key,
            comparison.baseline_median,
            comparison.candidate_median,
            comparison.regression_pct,
            comparison.threshold_pct,
            comparison.passed
        );
    }
    Ok(())
}

fn compare_with_baseline(
    candidate: &MatrixReport,
    baseline: &MatrixReport,
    threshold_pct: f64,
) -> Result<Vec<RegressionComparison>, String> {
    validate_performance_baseline(
        "resampler_matrix",
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
        candidate
            .cases
            .iter()
            .map(|case| (case.case_key.clone(), case.ns_per_input_sample.median)),
        baseline
            .cases
            .iter()
            .map(|case| (case.case_key.clone(), case.ns_per_input_sample.median)),
        threshold_pct,
    )
}

fn enforce_report(report: &MatrixReport) -> Result<(), String> {
    if report.cases.is_empty() {
        return Err("resampler matrix report contains no cases".to_string());
    }
    if report.cases.len() != report.conditions.case_count {
        return Err(format!(
            "resampler matrix case_count mismatch: conditions {}, cases {}",
            report.conditions.case_count,
            report.cases.len()
        ));
    }

    let invalid_cases = report
        .cases
        .iter()
        .filter(|case| {
            !case.work_validation.valid
                || !case.work_validation.init_ok
                || case.setup_ns.samples.len() != report.conditions.trials
                || case.ns_per_input_sample.samples.len() != report.conditions.trials
                || case.ns_per_input_buffer.samples.len() != report.conditions.trials
                || case.output_frames_total_per_trial.len() != report.conditions.trials
                || case.consumed_frames_total_per_trial.len() != report.conditions.trials
                || case.output_frames_total_per_trial.iter().any(|frames| {
                    *frames < case.minimum_output_frames_total
                        || *frames > case.maximum_output_frames_total
                })
                || case
                    .consumed_frames_total_per_trial
                    .iter()
                    .any(|frames| *frames != case.frames * report.conditions.iterations_per_trial)
        })
        .map(|case| case.case_key.as_str())
        .collect::<Vec<_>>();
    if !invalid_cases.is_empty() {
        return Err(format!(
            "resampler matrix validity gate failed for cases: {}",
            invalid_cases.join(", ")
        ));
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "resampler matrix median regression gate failed",
        "ns/input-sample",
    ) {
        return Err(error);
    }
    Ok(())
}

fn algorithm_label(quality: &str, phase: &str) -> String {
    format!(
        "{RESAMPLER_BACKEND_NAME} matrix process_checked quality={quality} phase={phase} id={MATRIX_ALGORITHM_ID}"
    )
}

fn case_key(spec: &MatrixCaseSpec) -> String {
    format!(
        "scenario={};quality={};phase={};channels={};frames={};from={};to={};algorithm={RESAMPLER_BACKEND_NAME}_{MATRIX_ALGORITHM_ID}",
        spec.rate.name,
        spec.quality.name,
        spec.phase.name,
        spec.channels.name,
        spec.frames,
        spec.rate.from_rate,
        spec.rate.to_rate
    )
}

fn output_frame_bounds(expected_output_frames: usize) -> (usize, usize) {
    let slack = expected_output_frames * 5 / 100 + OUTPUT_FRAMES_ABS_SLACK;
    (
        expected_output_frames.saturating_sub(slack),
        expected_output_frames + slack,
    )
}

fn benchmark_case(
    spec: &MatrixCaseSpec,
    iterations: usize,
    trials: usize,
) -> Result<MatrixCase, String> {
    let channels = spec.channels.channels;
    let frames = spec.frames;
    let input = resampler_test_buffer(frames, channels, spec.rate.from_rate);
    let work_validation = validate_work(spec, &input);

    if !work_validation.init_ok {
        return Err(format!(
            "resampler matrix init failed for {}: {}",
            case_key(spec),
            work_validation
                .init_error
                .clone()
                .unwrap_or_else(|| "unknown init error".to_string())
        ));
    }

    let mut setup_ns = Vec::with_capacity(trials);
    let mut ns_per_input_sample = Vec::with_capacity(trials);
    let mut ns_per_input_buffer = Vec::with_capacity(trials);
    let mut output_frames_total_per_trial = Vec::with_capacity(trials);
    let mut consumed_frames_total_per_trial = Vec::with_capacity(trials);

    for _ in 0..trials {
        let measurement = measure_case(spec, &input, iterations)?;
        setup_ns.push(measurement.setup_ns);
        ns_per_input_sample.push(measurement.ns_per_input_sample);
        ns_per_input_buffer.push(measurement.ns_per_input_buffer);
        output_frames_total_per_trial.push(measurement.output_frames_total);
        consumed_frames_total_per_trial.push(measurement.consumed_frames_total);
    }

    let setup_ns = summarize_trials(setup_ns)?;
    let ns_per_input_sample = summarize_trials(ns_per_input_sample)?;
    let ns_per_input_buffer = summarize_trials(ns_per_input_buffer)?;
    let expected_output_frames_total =
        (frames as f64 * iterations as f64 * spec.rate.to_rate as f64 / spec.rate.from_rate as f64)
            .round() as usize;
    let (minimum_output_frames_total, maximum_output_frames_total) =
        output_frame_bounds(expected_output_frames_total);
    let source_buffer_duration_ns = frames as f64 / spec.rate.from_rate as f64 * 1.0e9;
    let median_source_realtime_utilization_pct =
        ns_per_input_buffer.median / source_buffer_duration_ns * 100.0;
    let p95_source_realtime_utilization_pct =
        ns_per_input_buffer.p95 / source_buffer_duration_ns * 100.0;

    Ok(MatrixCase {
        case_key: case_key(spec),
        scenario: spec.rate.name.to_string(),
        quality: spec.quality.name.to_string(),
        phase: spec.phase.name.to_string(),
        channels_label: spec.channels.name.to_string(),
        channels,
        frames,
        input_samples: frames * channels,
        from_rate_hz: spec.rate.from_rate,
        to_rate_hz: spec.rate.to_rate,
        backend: RESAMPLER_BACKEND_NAME.to_string(),
        algorithm: algorithm_label(spec.quality.name, spec.phase.name),
        setup_ns,
        ns_per_input_sample,
        ns_per_input_buffer,
        source_buffer_duration_ns,
        median_source_realtime_utilization_pct,
        p95_source_realtime_utilization_pct,
        expected_output_frames_total,
        minimum_output_frames_total,
        maximum_output_frames_total,
        output_frames_total_per_trial,
        consumed_frames_total_per_trial,
        work_validation,
    })
}

fn create_resampler(spec: &MatrixCaseSpec) -> Result<StreamingResampler, String> {
    StreamingResampler::with_quality(
        spec.channels.channels,
        spec.rate.from_rate,
        spec.rate.to_rate,
        spec.phase.phase,
        spec.quality.quality,
    )
    .map_err(|error| error.to_string())
}

fn validate_work(spec: &MatrixCaseSpec, input: &[f64]) -> MatrixWorkValidation {
    let channels = spec.channels.channels;
    let mut resampler = match create_resampler(spec) {
        Ok(resampler) => resampler,
        Err(error) => {
            return MatrixWorkValidation {
                valid: false,
                validation_buffers: VALIDATION_BUFFERS,
                consumed_frames: 0,
                expected_consumed_frames: input.len() / channels * VALIDATION_BUFFERS,
                produced_frames: 0,
                all_output_samples_finite: false,
                init_ok: false,
                init_error: Some(error),
            };
        }
    };

    warm_resampler(&mut resampler, channels, input);
    let mut output = vec![0.0; streaming_output_capacity(&resampler, channels, input.len())];
    let mut consumed_frames = 0usize;
    let mut produced_frames = 0usize;
    let mut all_output_samples_finite = true;
    for _ in 0..VALIDATION_BUFFERS {
        let run = run_process_checked(&mut resampler, channels, input, &mut output, true);
        consumed_frames += run.0;
        produced_frames += run.1;
        all_output_samples_finite &= run.2;
    }
    let expected_consumed_frames = input.len() / channels * VALIDATION_BUFFERS;
    let equal_rate = spec.rate.from_rate == spec.rate.to_rate;
    let produced_ok = if equal_rate {
        produced_frames == expected_consumed_frames
    } else {
        produced_frames > 0
    };
    MatrixWorkValidation {
        valid: consumed_frames == expected_consumed_frames
            && produced_ok
            && all_output_samples_finite,
        validation_buffers: VALIDATION_BUFFERS,
        consumed_frames,
        expected_consumed_frames,
        produced_frames,
        all_output_samples_finite,
        init_ok: true,
        init_error: None,
    }
}

fn measure_case(
    spec: &MatrixCaseSpec,
    input: &[f64],
    iterations: usize,
) -> Result<TrialMeasurement, String> {
    let channels = spec.channels.channels;
    let frames = spec.frames;

    let setup_start = Instant::now();
    let mut resampler = create_resampler(spec)?;
    let setup_ns = setup_start.elapsed().as_nanos() as f64;
    let setup_ns = setup_ns.max(1.0);

    warm_resampler(&mut resampler, channels, input);
    let mut output = vec![0.0; streaming_output_capacity(&resampler, channels, input.len())];
    let mut output_frames_total = 0usize;
    let mut consumed_frames_total = 0usize;
    let start = Instant::now();
    for _ in 0..iterations {
        let (consumed, produced, _) = run_process_checked(
            &mut resampler,
            channels,
            black_box(input),
            &mut output,
            false,
        );
        consumed_frames_total += consumed;
        output_frames_total += produced;
    }
    let elapsed = start.elapsed();

    let expected_output_frames = (frames as f64 * iterations as f64 * spec.rate.to_rate as f64
        / spec.rate.from_rate as f64)
        .round() as usize;
    let (minimum_output_frames, maximum_output_frames) =
        output_frame_bounds(expected_output_frames);
    if output_frames_total < minimum_output_frames || output_frames_total > maximum_output_frames {
        return Err(format!(
            "{} produced {} frames outside [{}, {}] for {} expected streaming frames",
            case_key(spec),
            output_frames_total,
            minimum_output_frames,
            maximum_output_frames,
            expected_output_frames
        ));
    }
    let expected_consumed_frames = frames * iterations;
    if consumed_frames_total != expected_consumed_frames {
        return Err(format!(
            "{} consumed {} frames for {} expected input frames",
            case_key(spec),
            consumed_frames_total,
            expected_consumed_frames
        ));
    }

    let ns_per_input_buffer = elapsed.as_nanos() as f64 / iterations as f64;
    let ns_per_input_sample = ns_per_input_buffer / (frames * channels) as f64;

    Ok(TrialMeasurement {
        setup_ns,
        ns_per_input_sample,
        ns_per_input_buffer,
        output_frames_total,
        consumed_frames_total,
    })
}

fn warm_resampler(resampler: &mut StreamingResampler, channels: usize, input: &[f64]) {
    let mut output = vec![0.0; streaming_output_capacity(resampler, channels, input.len())];
    for _ in 0..WARMUP_BUFFERS {
        let _ = run_process_checked(resampler, channels, input, &mut output, false);
    }
}

fn streaming_output_capacity(
    resampler: &StreamingResampler,
    channels: usize,
    input_samples: usize,
) -> usize {
    let capacity_frames = resampler
        .process_output_capacity_frames(input_samples / channels)
        .expect("benchmark resampler capacity must fit");
    let capacity = capacity_frames
        .saturating_mul(channels)
        .saturating_mul(8)
        .saturating_add(8192);
    // AudioBlockMut requires a whole number of frames.
    capacity.div_ceil(channels) * channels
}

fn run_process_checked(
    resampler: &mut StreamingResampler,
    channels: usize,
    input: &[f64],
    output: &mut [f64],
    validate_output: bool,
) -> (usize, usize, bool) {
    let input_frames = input.len() / channels;
    let mut consumed_frames = 0;
    let mut output_frames = 0;
    let mut all_output_samples_finite = true;
    while consumed_frames < input_frames {
        let input_block =
            AudioBlockRef::new(&input[consumed_frames * channels..], channels).unwrap();
        let output_block = AudioBlockMut::new(output, channels).unwrap();
        let buffers = ProcessBuffers::out_of_place(input_block, output_block).unwrap();
        let progress = process_checked(resampler, buffers).unwrap();
        consumed_frames += progress.consumed_frames();
        output_frames += progress.produced_frames();
        let produced = &output[..progress.produced_frames() * channels];
        if validate_output {
            all_output_samples_finite &= produced.iter().all(|sample| sample.is_finite());
        }
        black_box(produced);
    }
    (consumed_frames, output_frames, all_output_samples_finite)
}
