use std::hint::black_box;
use std::time::Instant;

use audio_engine_core::processor::{
    ConvolutionStrategy, FFTConvolver, PARTITIONED_CONVOLUTION_IR_THRESHOLD,
    PARTITIONED_CONVOLUTION_PARTITION_SIZE,
};
use serde::{Deserialize, Serialize};

pub mod support;

use support::{
    compare_case_medians, enforce_pinned_burst_limits, environment_json, generated_unix_ms,
    parse_pinned_probe_args, read_json, regression_gate_error, summarize_trials,
    validate_performance_baseline, write_json, BenchEnvironment, BenchMode, PerfArgs,
    PerformanceReportIdentity, RegressionComparison, TrialDistribution, DEFAULT_PINNED_PROBE_CORE,
    REPORT_SCHEMA_VERSION,
};

const SAMPLE_RATE: f64 = 48_000.0;
const CHANNEL_COUNTS: [usize; 2] = [2, 6];
const THROUGHPUT_IR_FRAMES: [usize; 7] = [256, 2_048, 4_097, 8_192, 16_384, 32_768, 65_536];
const CALLBACK_IR_FRAMES: [usize; 5] = [4_097, 8_192, 16_384, 32_768, 65_536];
const CALLBACK_FRAMES: [usize; 4] = [64, 128, 256, 512];
const WARMUP_ITERATIONS: usize = 4;

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct ConvolverConditions {
    sample_rate_hz: u32,
    partition_threshold: usize,
    partition_size: usize,
    throughput_frames: usize,
    throughput_ir_frames: Vec<usize>,
    callback_ir_frames: Vec<usize>,
    callback_frames: Vec<usize>,
    channel_counts: Vec<usize>,
    throughput_base_iterations: usize,
    callback_partition_cycles: usize,
    trials: usize,
    warmup_iterations: usize,
    coverage: String,
    excludes: Vec<String>,
    // Isolated-probe mode: the bench thread is pinned to one core with raised
    // priority so worst-case (max/p99) callback gates become enforceable on
    // this machine. Pinned and unpinned reports are baseline-incompatible.
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    pin_core: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkValidation {
    valid: bool,
    expected_strategy: String,
    actual_strategy: String,
    expected_partition_size: Option<usize>,
    actual_partition_size: Option<usize>,
    output_changed: bool,
    all_output_samples_finite: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct CallbackDistribution {
    samples: Vec<f64>,
    min: f64,
    median: f64,
    p95: f64,
    p99: f64,
    max: f64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ConvolverCase {
    Throughput {
        case_key: String,
        channels: usize,
        ir_frames: usize,
        frames: usize,
        samples: usize,
        strategy: String,
        fft_size: usize,
        partition_size: Option<usize>,
        iterations_per_trial: usize,
        process_into_ns_per_sample: TrialDistribution,
        process_inplace_ns_per_sample: TrialDistribution,
        allocating_process_ns_per_sample: TrialDistribution,
        work_validation: WorkValidation,
    },
    CallbackBurst {
        case_key: String,
        channels: usize,
        ir_frames: usize,
        frames: usize,
        samples: usize,
        strategy: String,
        fft_size: usize,
        partition_size: usize,
        calls_per_trial: usize,
        callback_ns_per_sample: CallbackDistribution,
        callback_ns_per_buffer: CallbackDistribution,
        buffer_duration_ns: f64,
        median_deadline_utilization_pct: f64,
        p95_deadline_utilization_pct: f64,
        p99_deadline_utilization_pct: f64,
        max_deadline_utilization_pct: f64,
        work_validation: WorkValidation,
    },
}

impl ConvolverCase {
    fn case_key(&self) -> &str {
        match self {
            Self::Throughput { case_key, .. } | Self::CallbackBurst { case_key, .. } => case_key,
        }
    }

    fn primary_median(&self) -> f64 {
        match self {
            Self::Throughput {
                process_into_ns_per_sample,
                ..
            } => process_into_ns_per_sample.median,
            Self::CallbackBurst {
                callback_ns_per_sample,
                ..
            } => callback_ns_per_sample.median,
        }
    }

    fn has_valid_work(&self) -> bool {
        match self {
            Self::Throughput {
                work_validation, ..
            }
            | Self::CallbackBurst {
                work_validation, ..
            } => work_validation.valid,
        }
    }

    fn has_complete_samples(&self, trials: usize) -> bool {
        match self {
            Self::Throughput {
                process_into_ns_per_sample,
                process_inplace_ns_per_sample,
                allocating_process_ns_per_sample,
                ..
            } => {
                process_into_ns_per_sample.samples.len() == trials
                    && process_inplace_ns_per_sample.samples.len() == trials
                    && allocating_process_ns_per_sample.samples.len() == trials
            }
            Self::CallbackBurst {
                calls_per_trial,
                callback_ns_per_sample,
                callback_ns_per_buffer,
                ..
            } => {
                let expected = trials * calls_per_trial;
                callback_ns_per_sample.samples.len() == expected
                    && callback_ns_per_buffer.samples.len() == expected
            }
        }
    }
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
struct ConvolverReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: ConvolverConditions,
    cases: Vec<ConvolverCase>,
    baseline: Option<BaselineReference>,
    comparisons: Vec<RegressionComparison>,
}

fn main() -> Result<(), String> {
    let pin_args = parse_pinned_probe_args(std::env::args().skip(1).collect())?;
    let args = PerfArgs::parse(pin_args.remaining)?;
    if args.help {
        print_help();
        return Ok(());
    }
    if pin_args.enabled {
        pin_current_thread(pin_args.core)?;
    }

    let (throughput_frames, throughput_base_iterations, callback_partition_cycles, trials) =
        workload(args.mode);
    let conditions = ConvolverConditions {
        sample_rate_hz: SAMPLE_RATE as u32,
        partition_threshold: PARTITIONED_CONVOLUTION_IR_THRESHOLD,
        partition_size: PARTITIONED_CONVOLUTION_PARTITION_SIZE,
        throughput_frames,
        throughput_ir_frames: THROUGHPUT_IR_FRAMES.to_vec(),
        callback_ir_frames: CALLBACK_IR_FRAMES.to_vec(),
        callback_frames: CALLBACK_FRAMES.to_vec(),
        channel_counts: CHANNEL_COUNTS.to_vec(),
        throughput_base_iterations,
        callback_partition_cycles,
        trials,
        warmup_iterations: WARMUP_ITERATIONS,
        coverage: "steady_state_throughput+long_ir_callback_burst".to_string(),
        excludes: ["callback_chain", "device_write", "ir_construction"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        pinned: pin_args.enabled,
        pin_core: pin_args.enabled.then_some(pin_args.core),
    };

    let mut cases = Vec::new();
    for ir_frames in THROUGHPUT_IR_FRAMES {
        let iterations = throughput_iterations(throughput_base_iterations, ir_frames);
        for channels in CHANNEL_COUNTS {
            cases.push(benchmark_throughput(
                channels,
                ir_frames,
                throughput_frames,
                iterations,
                trials,
            )?);
        }
    }
    for ir_frames in CALLBACK_IR_FRAMES {
        for channels in CHANNEL_COUNTS {
            for frames in CALLBACK_FRAMES {
                cases.push(benchmark_callback_burst(
                    channels,
                    ir_frames,
                    frames,
                    callback_partition_cycles,
                    trials,
                )?);
            }
        }
    }

    let mut report = ConvolverReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: "audio_convolver_perf".to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment: BenchEnvironment::capture(),
        conditions,
        cases,
        baseline: None,
        comparisons: Vec::new(),
    };

    if let Some(path) = &args.baseline {
        let baseline: ConvolverReport = read_json(path, "convolver baseline report")?;
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
        write_json(path, &report, "convolver performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }
    Ok(())
}

/// Absolute worst-case gates for the flagship long-IR small-buffer case.
/// These are machine-specific by design and therefore only enforced in the
/// isolated `--pinned` probe mode (task 07-23 acceptance criteria).
const PINNED_GATE_IR_FRAMES: usize = 65_536;
const PINNED_GATE_FRAMES: usize = 64;
const PINNED_GATE_CHANNELS: usize = 6;
const PINNED_GATE_MAX_UTILIZATION_PCT: f64 = 50.0;
const PINNED_GATE_P99_UTILIZATION_PCT: f64 = 40.0;

#[cfg(windows)]
fn pin_current_thread(core: usize) -> Result<(), String> {
    const THREAD_PRIORITY_HIGHEST: i32 = 2;
    const HIGH_PRIORITY_CLASS: u32 = 0x0000_0080;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn GetCurrentThread() -> isize;
        fn SetPriorityClass(process: isize, class: u32) -> i32;
        fn SetThreadAffinityMask(thread: isize, mask: usize) -> usize;
        fn SetThreadPriority(thread: isize, priority: i32) -> i32;
    }

    if core >= usize::BITS as usize {
        return Err(format!("--pin-core {core} exceeds the affinity mask width"));
    }
    // SAFETY: these calls only adjust scheduling for the current process/thread
    // and use pseudo handles that require no cleanup. Thread priority is
    // relative to the process class, so raise both before collecting samples.
    unsafe {
        if SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS) == 0 {
            return Err("SetPriorityClass failed".to_string());
        }
        let thread = GetCurrentThread();
        if SetThreadAffinityMask(thread, 1usize << core) == 0 {
            return Err(format!("SetThreadAffinityMask failed for core {core}"));
        }
        if SetThreadPriority(thread, THREAD_PRIORITY_HIGHEST) == 0 {
            return Err("SetThreadPriority failed".to_string());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn pin_current_thread(_core: usize) -> Result<(), String> {
    Err("--pinned is only implemented on Windows in this bench".to_string())
}

fn workload(mode: BenchMode) -> (usize, usize, usize, usize) {
    match mode {
        BenchMode::Quick => (2_048, 512, 2, 7),
        BenchMode::Full => (8_192, 256, 4, 9),
        BenchMode::Heavy => (16_384, 256, 8, 15),
    }
}

fn throughput_iterations(base: usize, ir_frames: usize) -> usize {
    match ir_frames {
        0..=256 => base,
        257..=2_048 => (base / 2).max(8),
        2_049..=8_192 => (base / 4).max(6),
        8_193..=16_384 => (base / 8).max(4),
        16_385..=32_768 => (base / 12).max(3),
        _ => (base / 16).max(2),
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_convolver_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>] [--pinned] [--pin-core <n>]\n\
         \n\
         Reports versioned long-IR throughput and per-callback burst distributions.\n\
         Timing is report-only unless a compatible same-machine baseline is supplied.\n\
         --pinned runs the isolated probe (thread pinned to --pin-core, default {}, raised\n\
         priority); with --enforce it additionally gates worst-case burst utilization\n\
         for the 65536-tap 6-channel 64-frame case.",
        DEFAULT_PINNED_PROBE_CORE
    );
}

fn benchmark_throughput(
    channels: usize,
    ir_frames: usize,
    frames: usize,
    iterations: usize,
    trials: usize,
) -> Result<ConvolverCase, String> {
    let ir = synthetic_ir(ir_frames, channels);
    let input = synthetic_input(frames, channels);
    let validation = validate_work(&ir, &input, channels, ir_frames)?;
    let metadata = convolver_metadata(&ir, channels)?;
    let mut process_into = Vec::with_capacity(trials);
    let mut process_inplace = Vec::with_capacity(trials);
    let mut allocating_process = Vec::with_capacity(trials);

    for _ in 0..trials {
        let mut into_conv = FFTConvolver::new(&ir, channels).map_err(|error| error.to_string())?;
        let mut inplace_conv =
            FFTConvolver::new(&ir, channels).map_err(|error| error.to_string())?;
        let mut allocating_conv =
            FFTConvolver::new(&ir, channels).map_err(|error| error.to_string())?;
        let mut output = vec![0.0; input.len()];
        let mut inplace_buffer = input.clone();

        warm_into(&mut into_conv, &input, &mut output)?;
        warm_inplace(&mut inplace_conv, &input, &mut inplace_buffer)?;
        warm_allocating(&mut allocating_conv, &input)?;

        let start = Instant::now();
        for _ in 0..iterations {
            into_conv
                .process_into(black_box(&input), black_box(&mut output))
                .map_err(|error| error.to_string())?;
            black_box(output[0]);
        }
        process_into.push(ns_per_sample(
            start.elapsed().as_nanos(),
            frames,
            channels,
            iterations,
        ));

        let start = Instant::now();
        for _ in 0..iterations {
            inplace_buffer.copy_from_slice(&input);
            inplace_conv
                .process_inplace(black_box(&mut inplace_buffer))
                .map_err(|error| error.to_string())?;
            black_box(inplace_buffer[0]);
        }
        process_inplace.push(ns_per_sample(
            start.elapsed().as_nanos(),
            frames,
            channels,
            iterations,
        ));

        let start = Instant::now();
        for _ in 0..iterations {
            let output = allocating_conv
                .process(black_box(&input))
                .map_err(|error| error.to_string())?;
            black_box(output[0]);
        }
        allocating_process.push(ns_per_sample(
            start.elapsed().as_nanos(),
            frames,
            channels,
            iterations,
        ));
    }

    Ok(ConvolverCase::Throughput {
        case_key: format!(
            "kind=throughput;ir_frames={ir_frames};frames={frames};channels={channels};strategy={}",
            strategy_name(metadata.strategy)
        ),
        channels,
        ir_frames,
        frames,
        samples: frames * channels,
        strategy: strategy_name(metadata.strategy).to_string(),
        fft_size: metadata.fft_size,
        partition_size: metadata.partition_size,
        iterations_per_trial: iterations,
        process_into_ns_per_sample: summarize_trials(process_into)?,
        process_inplace_ns_per_sample: summarize_trials(process_inplace)?,
        allocating_process_ns_per_sample: summarize_trials(allocating_process)?,
        work_validation: validation,
    })
}

fn benchmark_callback_burst(
    channels: usize,
    ir_frames: usize,
    frames: usize,
    partition_cycles: usize,
    trials: usize,
) -> Result<ConvolverCase, String> {
    let ir = synthetic_ir(ir_frames, channels);
    let input = synthetic_input(frames, channels);
    let validation = validate_work(&ir, &input, channels, ir_frames)?;
    let metadata = convolver_metadata(&ir, channels)?;
    let partition_size = metadata
        .partition_size
        .ok_or_else(|| format!("long IR {ir_frames} did not select partitioned convolution"))?;
    let calls_per_cycle = partition_size.div_ceil(frames);
    let calls_per_trial = calls_per_cycle * partition_cycles;
    let mut ns_per_buffer = Vec::with_capacity(trials * calls_per_trial);
    let mut ns_per_sample_values = Vec::with_capacity(trials * calls_per_trial);

    for _ in 0..trials {
        let mut convolver = FFTConvolver::new(&ir, channels).map_err(|error| error.to_string())?;
        let mut output = vec![0.0; input.len()];
        warm_into(&mut convolver, &input, &mut output)?;
        convolver.reset();

        for _ in 0..calls_per_trial {
            let start = Instant::now();
            convolver
                .process_into(black_box(&input), black_box(&mut output))
                .map_err(|error| error.to_string())?;
            let elapsed = start.elapsed().as_nanos() as f64;
            ns_per_buffer.push(elapsed);
            ns_per_sample_values.push(elapsed / (frames * channels) as f64);
            black_box(output[0]);
        }
    }

    let callback_ns_per_buffer = summarize_callbacks(ns_per_buffer)?;
    let callback_ns_per_sample = summarize_callbacks(ns_per_sample_values)?;
    let buffer_duration_ns = frames as f64 / SAMPLE_RATE * 1.0e9;
    Ok(ConvolverCase::CallbackBurst {
        case_key: format!(
            "kind=callback_burst;ir_frames={ir_frames};frames={frames};channels={channels};strategy={}",
            strategy_name(metadata.strategy)
        ),
        channels,
        ir_frames,
        frames,
        samples: frames * channels,
        strategy: strategy_name(metadata.strategy).to_string(),
        fft_size: metadata.fft_size,
        partition_size,
        calls_per_trial,
        median_deadline_utilization_pct: callback_ns_per_buffer.median / buffer_duration_ns * 100.0,
        p95_deadline_utilization_pct: callback_ns_per_buffer.p95 / buffer_duration_ns * 100.0,
        p99_deadline_utilization_pct: callback_ns_per_buffer.p99 / buffer_duration_ns * 100.0,
        max_deadline_utilization_pct: callback_ns_per_buffer.max / buffer_duration_ns * 100.0,
        callback_ns_per_sample,
        callback_ns_per_buffer,
        buffer_duration_ns,
        work_validation: validation,
    })
}

struct ConvolverMetadata {
    strategy: ConvolutionStrategy,
    fft_size: usize,
    partition_size: Option<usize>,
}

fn convolver_metadata(ir: &[f64], channels: usize) -> Result<ConvolverMetadata, String> {
    let convolver = FFTConvolver::new(ir, channels).map_err(|error| error.to_string())?;
    Ok(ConvolverMetadata {
        strategy: convolver.strategy(),
        fft_size: convolver.fft_size(),
        partition_size: convolver.partition_size(),
    })
}

fn validate_work(
    ir: &[f64],
    input: &[f64],
    channels: usize,
    ir_frames: usize,
) -> Result<WorkValidation, String> {
    let mut convolver = FFTConvolver::new(ir, channels).map_err(|error| error.to_string())?;
    let actual_strategy = convolver.strategy();
    let actual_partition_size = convolver.partition_size();
    let expected_strategy = if ir_frames > PARTITIONED_CONVOLUTION_IR_THRESHOLD {
        ConvolutionStrategy::Partitioned
    } else {
        ConvolutionStrategy::OverlapSave
    };
    let expected_partition_size = (expected_strategy == ConvolutionStrategy::Partitioned)
        .then_some(PARTITIONED_CONVOLUTION_PARTITION_SIZE);
    let mut output = vec![0.0; input.len()];
    convolver
        .process_into(input, &mut output)
        .map_err(|error| error.to_string())?;
    let output_changed = output.iter().any(|sample| *sample != 0.0);
    let all_output_samples_finite = output.iter().all(|sample| sample.is_finite());
    Ok(WorkValidation {
        valid: actual_strategy == expected_strategy
            && actual_partition_size == expected_partition_size
            && output_changed
            && all_output_samples_finite,
        expected_strategy: strategy_name(expected_strategy).to_string(),
        actual_strategy: strategy_name(actual_strategy).to_string(),
        expected_partition_size,
        actual_partition_size,
        output_changed,
        all_output_samples_finite,
    })
}

fn warm_into(
    convolver: &mut FFTConvolver,
    input: &[f64],
    output: &mut [f64],
) -> Result<(), String> {
    for _ in 0..WARMUP_ITERATIONS {
        convolver
            .process_into(input, output)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn warm_inplace(
    convolver: &mut FFTConvolver,
    input: &[f64],
    buffer: &mut [f64],
) -> Result<(), String> {
    for _ in 0..WARMUP_ITERATIONS {
        buffer.copy_from_slice(input);
        convolver
            .process_inplace(buffer)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn warm_allocating(convolver: &mut FFTConvolver, input: &[f64]) -> Result<(), String> {
    for _ in 0..WARMUP_ITERATIONS {
        black_box(
            convolver
                .process(input)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(())
}

fn summarize_callbacks(samples: Vec<f64>) -> Result<CallbackDistribution, String> {
    if samples.is_empty() {
        return Err("callback distribution requires at least one sample".to_string());
    }
    if let Some((index, value)) = samples
        .iter()
        .copied()
        .enumerate()
        .find(|(_, value)| !value.is_finite() || *value <= 0.0)
    {
        return Err(format!(
            "callback sample {index} must be finite and positive, got {value}"
        ));
    }
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    let median = if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) * 0.5
    } else {
        sorted[middle]
    };
    Ok(CallbackDistribution {
        min: sorted[0],
        median,
        p95: nearest_rank(&sorted, 0.95),
        p99: nearest_rank(&sorted, 0.99),
        max: sorted[sorted.len() - 1],
        samples,
    })
}

fn nearest_rank(sorted: &[f64], percentile: f64) -> f64 {
    let rank = ((sorted.len() as f64 * percentile).ceil() as usize).max(1);
    sorted[rank - 1]
}

fn ns_per_sample(elapsed_ns: u128, frames: usize, channels: usize, iterations: usize) -> f64 {
    elapsed_ns as f64 / (frames * channels * iterations) as f64
}

fn compare_with_baseline(
    candidate: &ConvolverReport,
    baseline: &ConvolverReport,
    threshold_pct: f64,
) -> Result<Vec<RegressionComparison>, String> {
    validate_performance_baseline(
        "convolver",
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
            .filter(|case| matches!(case, ConvolverCase::Throughput { .. }))
            .map(|case| (case.case_key().to_string(), case.primary_median())),
        baseline
            .cases
            .iter()
            .filter(|case| matches!(case, ConvolverCase::Throughput { .. }))
            .map(|case| (case.case_key().to_string(), case.primary_median())),
        threshold_pct,
    )
}

fn enforce_report(report: &ConvolverReport) -> Result<(), String> {
    let invalid = report
        .cases
        .iter()
        .filter(|case| {
            !case.has_valid_work() || !case.has_complete_samples(report.conditions.trials)
        })
        .map(ConvolverCase::case_key)
        .collect::<Vec<_>>();
    if !invalid.is_empty() {
        return Err(format!(
            "convolver report validity gate failed for cases: {}",
            invalid.join(", ")
        ));
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "convolver median regression gate failed",
        "ns/sample",
    ) {
        return Err(error);
    }
    if report.conditions.pinned {
        enforce_pinned_burst_gate(report)?;
    }
    Ok(())
}

fn enforce_pinned_burst_gate(report: &ConvolverReport) -> Result<(), String> {
    let case = report
        .cases
        .iter()
        .find_map(|case| match case {
            ConvolverCase::CallbackBurst {
                case_key,
                channels,
                ir_frames,
                frames,
                p99_deadline_utilization_pct,
                max_deadline_utilization_pct,
                ..
            } if *ir_frames == PINNED_GATE_IR_FRAMES
                && *frames == PINNED_GATE_FRAMES
                && *channels == PINNED_GATE_CHANNELS =>
            {
                Some((
                    case_key.as_str(),
                    *p99_deadline_utilization_pct,
                    *max_deadline_utilization_pct,
                ))
            }
            _ => None,
        })
        .ok_or_else(|| {
            format!(
                "pinned burst gate case missing: ir_frames={PINNED_GATE_IR_FRAMES} \
                 frames={PINNED_GATE_FRAMES} channels={PINNED_GATE_CHANNELS}"
            )
        })?;

    let (case_key, p99, max) = case;
    enforce_pinned_burst_limits(
        case_key,
        p99,
        max,
        PINNED_GATE_P99_UTILIZATION_PCT,
        PINNED_GATE_MAX_UTILIZATION_PCT,
    )
}

fn print_report(report: &ConvolverReport) -> Result<(), String> {
    println!(
        "audio_convolver_perf mode={} threshold={} partition_size={} throughput_frames={} trials={} callback_cycles={}",
        report.mode.as_str(),
        report.conditions.partition_threshold,
        report.conditions.partition_size,
        report.conditions.throughput_frames,
        report.conditions.trials,
        report.conditions.callback_partition_cycles,
    );
    println!(
        "audio_convolver_environment {}",
        environment_json(&report.environment)?
    );
    for case in &report.cases {
        match case {
            ConvolverCase::Throughput {
                case_key,
                channels,
                ir_frames,
                frames,
                strategy,
                fft_size,
                partition_size,
                iterations_per_trial,
                process_into_ns_per_sample,
                process_inplace_ns_per_sample,
                allocating_process_ns_per_sample,
                ..
            } => println!(
                "convolver_throughput case={} channels={} ir_frames={} frames={} strategy={} fft_size={} partition_size={} iterations={} into_median={:.3} into_p95={:.3} inplace_median={:.3} inplace_p95={:.3} allocating_median={:.3}",
                case_key,
                channels,
                ir_frames,
                frames,
                strategy,
                fft_size,
                partition_size.unwrap_or(0),
                iterations_per_trial,
                process_into_ns_per_sample.median,
                process_into_ns_per_sample.p95,
                process_inplace_ns_per_sample.median,
                process_inplace_ns_per_sample.p95,
                allocating_process_ns_per_sample.median,
            ),
            ConvolverCase::CallbackBurst {
                case_key,
                channels,
                ir_frames,
                frames,
                partition_size,
                calls_per_trial,
                callback_ns_per_sample,
                callback_ns_per_buffer,
                p99_deadline_utilization_pct,
                max_deadline_utilization_pct,
                ..
            } => println!(
                "convolver_callback case={} channels={} ir_frames={} frames={} partition_size={} calls_per_trial={} ns_per_sample_median={:.3} ns_per_buffer_p95={:.3} ns_per_buffer_p99={:.3} ns_per_buffer_max={:.3} p99_deadline_utilization_pct={:.4} max_deadline_utilization_pct={:.4}",
                case_key,
                channels,
                ir_frames,
                frames,
                partition_size,
                calls_per_trial,
                callback_ns_per_sample.median,
                callback_ns_per_buffer.p95,
                callback_ns_per_buffer.p99,
                callback_ns_per_buffer.max,
                p99_deadline_utilization_pct,
                max_deadline_utilization_pct,
            ),
        }
    }
    for comparison in &report.comparisons {
        println!(
            "convolver_baseline case={} baseline_median={:.3} candidate_median={:.3} regression_pct={:.3} threshold_pct={:.3} passed={}",
            comparison.case_key,
            comparison.baseline_median,
            comparison.candidate_median,
            comparison.regression_pct,
            comparison.threshold_pct,
            comparison.passed,
        );
    }
    Ok(())
}

fn strategy_name(strategy: ConvolutionStrategy) -> &'static str {
    match strategy {
        ConvolutionStrategy::OverlapSave => "overlap_save",
        ConvolutionStrategy::Partitioned => "partitioned",
    }
}

fn synthetic_ir(frames: usize, channels: usize) -> Vec<f64> {
    let mut ir = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        let decay = (-(frame as f64) / (frames as f64 * 0.18).max(64.0)).exp();
        for channel in 0..channels {
            let impulse = if frame == 0 { 0.72 } else { 0.0 };
            let early = if frame == 17 + channel * 3 { 0.12 } else { 0.0 };
            let tail = ((frame + channel * 11) as f64 * 0.37).sin() * 0.025 * decay;
            ir.push(impulse + early + tail);
        }
    }
    ir
}

fn synthetic_input(frames: usize, channels: usize) -> Vec<f64> {
    let mut seed = 0x0BAD_5EED_u64;
    let mut out = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        let time = frame as f64 / SAMPLE_RATE;
        let sine = (std::f64::consts::TAU * 997.0 * time).sin() * 0.25;
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let noise = (((seed >> 33) as f64 / u32::MAX as f64) * 2.0 - 1.0) * 0.03;
        for channel in 0..channels {
            out.push((sine + noise) * (1.0 - channel as f64 * 0.015));
        }
    }
    out
}
