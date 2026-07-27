use std::hint::black_box;
use std::time::Instant;

use audio_engine_core::processor::ConvolverProcessor;
use audio_engine_core::{
    finish_checked, process_checked, AudioBlockMut, AudioBlockRef, ConvolverControl, FFTConvolver,
    ProcessBuffers, ProcessState, StreamingProcessor, StreamingResampler, RESAMPLER_BACKEND_NAME,
};
use serde::{Deserialize, Serialize};

pub mod support;

use support::allocation::{AllocationScope, AllocationSnapshot};
use support::{
    compare_case_medians, environment_json, generated_unix_ms, read_json, regression_gate_error,
    summarize_trials, validate_case_key_set, validate_performance_baseline, write_json_round_trip,
    BenchEnvironment, BenchMode, PerfArgs, PerformanceReportIdentity, RegressionComparison,
    TrialDistribution, REPORT_SCHEMA_VERSION,
};

const PROBE: &str = "audio_lifecycle_memory_perf";
const CHANNELS: usize = 2;
const FROM_RATE_HZ: u32 = 44_100;
const TO_RATE_HZ: u32 = 48_000;
const PROCESS_FRAMES: usize = 8_192;
const SHORT_IR_FRAMES: usize = 256;
const LONG_IR_FRAMES: usize = 8_192;
const FINISH_BLOCK_FRAMES: usize = 128;
const EXPECTED_ALLOCATION_OPERATIONS: [&str; 9] = [
    "resampler_equal_rate_setup",
    "resampler_active_reset",
    "resampler_active_finish",
    "resampler_equal_rate_finish",
    "convolver_reset",
    "convolver_finish",
    "dynamic_convolver_publish",
    "dynamic_convolver_adopt",
    "dynamic_convolver_reclaim",
];

#[derive(Clone, Copy)]
struct LifecycleWorkload {
    trials: usize,
    soak_trials: usize,
    soak_iterations: usize,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct LifecycleConditions {
    backend: String,
    channels: usize,
    from_rate_hz: u32,
    to_rate_hz: u32,
    process_frames: usize,
    short_ir_frames: usize,
    long_ir_frames: usize,
    finish_block_frames: usize,
    trials: usize,
    soak_trials: usize,
    soak_iterations: usize,
    case_keys: Vec<String>,
    baseline_gate_case_keys: Vec<String>,
    timer_scope: String,
    allocator_scope: String,
    native_allocation_disclosure: String,
    soak_scope: String,
    baseline_scope: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct LifecycleWorkValidation {
    valid: bool,
    observed_operations: usize,
    expected_operations: usize,
    consumed_frames: u64,
    produced_frames: u64,
    expected_produced_frames: u64,
    terminal_finished: bool,
    terminal_idempotent: bool,
    lifecycle_counters_valid: bool,
    expected_timing_samples: usize,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct LifecycleCase {
    case_key: String,
    component: String,
    operation: String,
    primary_unit: String,
    baseline_gate: bool,
    expected_timing_samples: usize,
    distribution: TrialDistribution,
    work_validation: LifecycleWorkValidation,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct PersistentMemoryEvidence {
    component: String,
    configuration: String,
    setup_allocation_calls: usize,
    setup_deallocation_calls: usize,
    setup_reallocation_calls: usize,
    setup_peak_live_bytes: usize,
    setup_retained_rust_bytes: usize,
    configured_working_buffer_bytes: Option<usize>,
    allocation_boundary: String,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct LifecycleAllocationEvidence {
    operation: String,
    allocation_calls: usize,
    deallocation_calls: usize,
    reallocation_calls: usize,
    peak_live_bytes: usize,
    retained_bytes: usize,
    rust_allocation_free_required: bool,
    valid: bool,
    allocation_boundary: String,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct SoakEvidence {
    trials: usize,
    iterations_per_trial: usize,
    retained_rust_bytes_per_trial: Vec<usize>,
    peak_rust_bytes_per_trial: Vec<usize>,
    allocation_calls_per_trial: Vec<usize>,
    deallocation_calls_per_trial: Vec<usize>,
    maximum_retained_rust_bytes: usize,
    all_cycles_valid: bool,
    bounded: bool,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct BaselineReference {
    path: String,
    revision: String,
    dirty: Option<bool>,
    generated_unix_ms: u128,
    max_median_regression_pct: f64,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct LifecycleReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: LifecycleConditions,
    cases: Vec<LifecycleCase>,
    persistent_memory: Vec<PersistentMemoryEvidence>,
    lifecycle_allocations: Vec<LifecycleAllocationEvidence>,
    soak: SoakEvidence,
    baseline: Option<BaselineReference>,
    comparisons: Vec<RegressionComparison>,
}

struct DrainResult {
    produced_frames: usize,
    all_finite: bool,
    terminal_idempotent: bool,
}

struct DynamicMeasurements {
    publish_ns: Vec<f64>,
    adopt_ns: Vec<f64>,
    reclaim_ns: Vec<f64>,
    valid: bool,
}

struct SoakTrial {
    ns_per_cycle: f64,
    snapshot: AllocationSnapshot,
    valid: bool,
}

fn main() -> Result<(), String> {
    let args = PerfArgs::parse(std::env::args().skip(1).collect())?;
    if args.help {
        print_help();
        return Ok(());
    }
    let workload = workload(args.mode);
    let input = synthetic_input(PROCESS_FRAMES, CHANNELS, FROM_RATE_HZ);
    let short_ir = synthetic_ir(SHORT_IR_FRAMES, CHANNELS);
    let long_ir = synthetic_ir(LONG_IR_FRAMES, CHANNELS);

    let mut cases = vec![
        benchmark_resampler_setup(FROM_RATE_HZ, TO_RATE_HZ, workload.trials, true)?,
        benchmark_resampler_setup(FROM_RATE_HZ, FROM_RATE_HZ, workload.trials, false)?,
        benchmark_convolver_setup(&short_ir, SHORT_IR_FRAMES, workload.trials)?,
        benchmark_convolver_setup(&long_ir, LONG_IR_FRAMES, workload.trials)?,
        benchmark_resampler_reset(&input, workload.trials)?,
        benchmark_convolver_reset(&short_ir, workload.trials)?,
        benchmark_resampler_finish(&input, FROM_RATE_HZ, TO_RATE_HZ, workload.trials, true)?,
        benchmark_resampler_finish(&input, FROM_RATE_HZ, FROM_RATE_HZ, workload.trials, false)?,
        benchmark_convolver_finish(&short_ir, workload.trials)?,
    ];

    let dynamic = benchmark_dynamic_convolver(workload.trials)?;
    let dynamic_validation = LifecycleWorkValidation {
        valid: dynamic.valid,
        observed_operations: workload.trials,
        expected_operations: workload.trials,
        consumed_frames: (workload.trials * 2 * 512) as u64,
        produced_frames: (workload.trials * 2 * 512) as u64,
        expected_produced_frames: (workload.trials * 2 * 512) as u64,
        terminal_finished: false,
        terminal_idempotent: false,
        lifecycle_counters_valid: dynamic.valid,
        expected_timing_samples: workload.trials,
    };
    cases.push(lifecycle_case(
        "component=dynamic_convolver;operation=publish;ir_frames=1;channels=1".to_string(),
        "ConvolverControl",
        "publish_at_rate",
        "ns/publication",
        dynamic.publish_ns,
        dynamic_validation.clone(),
        false,
    )?);
    cases.push(lifecycle_case(
        "component=dynamic_convolver;operation=adopt_process;ir_frames=1;channels=1;block_frames=512"
            .to_string(),
        "ConvolverProcessor",
        "adopt_and_process",
        "ns/adoption-block",
        dynamic.adopt_ns,
        dynamic_validation.clone(),
        false,
    )?);
    cases.push(lifecycle_case(
        "component=dynamic_convolver;operation=reclaim_retired;ir_frames=1;channels=1".to_string(),
        "ConvolverControl",
        "reclaim_retired",
        "ns/reclamation",
        dynamic.reclaim_ns,
        dynamic_validation,
        false,
    )?);

    let (soak_case, soak) = benchmark_soak(workload)?;
    cases.push(soak_case);
    cases.sort_by(|left, right| left.case_key.cmp(&right.case_key));

    let persistent_memory = measure_persistent_memory(&short_ir, &long_ir)?;
    let lifecycle_allocations = measure_lifecycle_allocations(&input, &short_ir)?;
    let conditions = LifecycleConditions {
        backend: RESAMPLER_BACKEND_NAME.to_string(),
        channels: CHANNELS,
        from_rate_hz: FROM_RATE_HZ,
        to_rate_hz: TO_RATE_HZ,
        process_frames: PROCESS_FRAMES,
        short_ir_frames: SHORT_IR_FRAMES,
        long_ir_frames: LONG_IR_FRAMES,
        finish_block_frames: FINISH_BLOCK_FRAMES,
        trials: workload.trials,
        soak_trials: workload.soak_trials,
        soak_iterations: workload.soak_iterations,
        case_keys: cases.iter().map(|case| case.case_key.clone()).collect(),
        baseline_gate_case_keys: cases
            .iter()
            .filter(|case| case.baseline_gate)
            .map(|case| case.case_key.clone())
            .collect(),
        timer_scope: "setup timers include construction only; reset timers exclude re-dirtying; finish timers include repeated finish_checked calls through terminal state; dynamic timers isolate control publication, one adoption block, or control reclamation"
            .to_string(),
        allocator_scope: "Rust global allocator calls only; setup snapshots are captured while the constructed object remains alive; caller input/output buffers are allocated before the scope"
            .to_string(),
        native_allocation_disclosure: native_allocation_disclosure(),
        soak_scope: "bounded repeated one-tap Convolver control/processor construction, publication, adoption, retirement, reclamation, quiescence, and drop; this is not an unbounded RSS claim"
            .to_string(),
        baseline_scope: "compatible-baseline medians gate active-resampler setup/reset/drain, Convolver setup/drain, and soak cases; timer-quantized Convolver reset plus equal-rate and isolated dynamic ownership cases remain report-only while retaining raw samples"
            .to_string(),
    };

    let mut report = LifecycleReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: PROBE.to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment: BenchEnvironment::capture(),
        conditions,
        cases,
        persistent_memory,
        lifecycle_allocations,
        soak,
        baseline: None,
        comparisons: Vec::new(),
    };

    if let Some(path) = args.baseline.as_deref() {
        let baseline: LifecycleReport = read_json(path, "lifecycle baseline report")?;
        validate_performance_baseline(
            "lifecycle",
            PerformanceReportIdentity {
                schema_version: report.schema_version,
                probe: &report.probe,
                mode: report.mode,
                environment: &report.environment,
                conditions: &report.conditions,
            },
            PerformanceReportIdentity {
                schema_version: baseline.schema_version,
                probe: &baseline.probe,
                mode: baseline.mode,
                environment: &baseline.environment,
                conditions: &baseline.conditions,
            },
        )?;
        report.comparisons = compare_case_medians(
            report
                .cases
                .iter()
                .filter(|case| case.baseline_gate)
                .map(|case| (case.case_key.clone(), case.distribution.median)),
            baseline
                .cases
                .iter()
                .filter(|case| case.baseline_gate)
                .map(|case| (case.case_key.clone(), case.distribution.median)),
            args.max_median_regression_pct,
        )?;
        report.baseline = Some(BaselineReference {
            path: path.display().to_string(),
            revision: baseline.environment.revision,
            dirty: baseline.environment.dirty,
            generated_unix_ms: baseline.generated_unix_ms,
            max_median_regression_pct: args.max_median_regression_pct,
        });
    }

    print_report(&report)?;
    if let Some(path) = args.out.as_deref() {
        write_json_round_trip(path, &report, "lifecycle memory performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }
    Ok(())
}

fn workload(mode: BenchMode) -> LifecycleWorkload {
    match mode {
        BenchMode::Quick => LifecycleWorkload {
            trials: 7,
            soak_trials: 5,
            soak_iterations: 128,
        },
        BenchMode::Full => LifecycleWorkload {
            trials: 15,
            soak_trials: 7,
            soak_iterations: 1_024,
        },
        BenchMode::Heavy => LifecycleWorkload {
            trials: 31,
            soak_trials: 11,
            soak_iterations: 4_096,
        },
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_lifecycle_memory_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>]\n\
         Measures representative setup/reset/finish/drain, persistent Rust allocation evidence, dynamic Convolver publication/reclamation, and bounded repeated lifecycle behavior.\n\
         Backend-native allocation limitations are recorded explicitly in every report."
    );
}

fn benchmark_resampler_setup(
    from_rate_hz: u32,
    to_rate_hz: u32,
    trials: usize,
    baseline_gate: bool,
) -> Result<LifecycleCase, String> {
    let mut samples = Vec::with_capacity(trials);
    let mut valid = true;
    for _ in 0..trials {
        let start = Instant::now();
        let resampler = StreamingResampler::new(CHANNELS, from_rate_hz, to_rate_hz)
            .map_err(|error| format!("timed resampler setup failed: {error}"))?;
        samples.push(elapsed_ns(start));
        valid &= resampler.from_rate() == from_rate_hz && resampler.to_rate() == to_rate_hz;
        black_box(resampler);
    }
    lifecycle_case(
        format!(
            "component=resampler;operation=setup;channels={CHANNELS};from={from_rate_hz};to={to_rate_hz};backend={RESAMPLER_BACKEND_NAME}"
        ),
        "StreamingResampler",
        "new",
        "ns/setup",
        samples,
        basic_validation(valid, trials),
        baseline_gate,
    )
}

fn benchmark_convolver_setup(
    ir: &[f64],
    ir_frames: usize,
    trials: usize,
) -> Result<LifecycleCase, String> {
    let mut samples = Vec::with_capacity(trials);
    let mut valid = true;
    for _ in 0..trials {
        let start = Instant::now();
        let convolver = FFTConvolver::new(black_box(ir), CHANNELS)
            .map_err(|error| format!("timed convolver setup failed: {error}"))?;
        samples.push(elapsed_ns(start));
        valid &= convolver.ir_length() == ir_frames;
        black_box(convolver);
    }
    lifecycle_case(
        format!(
            "component=fft_convolver;operation=setup;channels={CHANNELS};ir_frames={ir_frames}"
        ),
        "FFTConvolver",
        "new",
        "ns/setup",
        samples,
        basic_validation(valid, trials),
        true,
    )
}

fn benchmark_resampler_reset(input: &[f64], trials: usize) -> Result<LifecycleCase, String> {
    let mut samples = Vec::with_capacity(trials);
    let mut valid = true;
    let mut consumed = 0usize;
    let mut produced = 0usize;
    for _ in 0..trials {
        let mut resampler = StreamingResampler::new(CHANNELS, FROM_RATE_HZ, TO_RATE_HZ)
            .map_err(|error| format!("resampler reset setup failed: {error}"))?;
        let first = feed_resampler(&mut resampler, input)?;
        let start = Instant::now();
        resampler
            .reset()
            .map_err(|error| format!("timed resampler reset failed: {error}"))?;
        samples.push(elapsed_ns(start));
        let after_reset = feed_resampler(&mut resampler, input)?;
        valid &= first.0 == after_reset.0 && first.1 == after_reset.1;
        consumed += after_reset.0;
        produced += after_reset.1;
    }
    lifecycle_case(
        format!(
            "component=resampler;operation=reset_after_process;channels={CHANNELS};from={FROM_RATE_HZ};to={TO_RATE_HZ};backend={RESAMPLER_BACKEND_NAME}"
        ),
        "StreamingResampler",
        "reset",
        "ns/reset",
        samples,
        LifecycleWorkValidation {
            valid: valid && consumed == trials * PROCESS_FRAMES && produced > 0,
            observed_operations: trials,
            expected_operations: trials,
            consumed_frames: consumed as u64,
            produced_frames: produced as u64,
            expected_produced_frames: produced as u64,
            terminal_finished: false,
            terminal_idempotent: false,
            lifecycle_counters_valid: valid,
            expected_timing_samples: trials,
        },
        true,
    )
}

fn benchmark_convolver_reset(ir: &[f64], trials: usize) -> Result<LifecycleCase, String> {
    let mut samples = Vec::with_capacity(trials);
    let mut valid = true;
    let mut processed_frames = 0usize;
    for _ in 0..trials {
        let control = ConvolverControl::new(true);
        let mut processor = ConvolverProcessor::new(control.clone())
            .map_err(|error| format!("convolver reset processor setup failed: {error}"))?;
        processor
            .set_sample_rate(TO_RATE_HZ)
            .map_err(|error| format!("convolver reset sample-rate setup failed: {error}"))?;
        let kernel = FFTConvolver::new(ir, CHANNELS)
            .map_err(|error| format!("convolver reset kernel setup failed: {error}"))?;
        control
            .publish_at_rate(kernel, TO_RATE_HZ)
            .map_err(|error| format!("convolver reset publish failed: {error}"))?;
        let mut block = synthetic_input(512, CHANNELS, TO_RATE_HZ);
        process_convolver(&mut processor, &mut block, CHANNELS)?;
        let start = Instant::now();
        processor
            .reset()
            .map_err(|error| format!("timed convolver reset failed: {error}"))?;
        samples.push(elapsed_ns(start));
        let progress = process_convolver(&mut processor, &mut block, CHANNELS)?;
        valid &= progress == 512 && control.status().latest_adopted_generation == 1;
        processed_frames += progress;
    }
    lifecycle_case(
        format!(
            "component=dynamic_convolver;operation=reset_after_process;channels={CHANNELS};ir_frames={SHORT_IR_FRAMES}"
        ),
        "ConvolverProcessor",
        "reset",
        "ns/reset",
        samples,
        LifecycleWorkValidation {
            valid: valid && processed_frames == trials * 512,
            observed_operations: trials,
            expected_operations: trials,
            consumed_frames: processed_frames as u64,
            produced_frames: processed_frames as u64,
            expected_produced_frames: (trials * 512) as u64,
            terminal_finished: false,
            terminal_idempotent: false,
            lifecycle_counters_valid: valid,
            expected_timing_samples: trials,
        },
        false,
    )
}

fn benchmark_resampler_finish(
    input: &[f64],
    from_rate_hz: u32,
    to_rate_hz: u32,
    trials: usize,
    baseline_gate: bool,
) -> Result<LifecycleCase, String> {
    let mut samples = Vec::with_capacity(trials);
    let mut valid = true;
    let mut total_consumed = 0usize;
    let mut total_produced = 0usize;
    let expected_per_trial =
        (PROCESS_FRAMES as f64 * to_rate_hz as f64 / from_rate_hz as f64).round() as usize;
    for _ in 0..trials {
        let mut resampler = StreamingResampler::new(CHANNELS, from_rate_hz, to_rate_hz)
            .map_err(|error| format!("resampler finish setup failed: {error}"))?;
        let (consumed, process_produced) = feed_resampler(&mut resampler, input)?;
        let mut output = vec![0.0; FINISH_BLOCK_FRAMES * CHANNELS * 64];
        let start = Instant::now();
        let drain = drain_processor(&mut resampler, &mut output, CHANNELS, 128)?;
        samples.push(elapsed_ns(start));
        let produced = process_produced + drain.produced_frames;
        valid &= consumed == PROCESS_FRAMES
            && produced == expected_per_trial
            && drain.all_finite
            && drain.terminal_idempotent;
        total_consumed += consumed;
        total_produced += produced;
    }
    lifecycle_case(
        format!(
            "component=resampler;operation=finish_drain;channels={CHANNELS};input_frames={PROCESS_FRAMES};from={from_rate_hz};to={to_rate_hz};backend={RESAMPLER_BACKEND_NAME}"
        ),
        "StreamingResampler",
        "finish_checked_until_terminal",
        "ns/drain",
        samples,
        LifecycleWorkValidation {
            valid,
            observed_operations: trials,
            expected_operations: trials,
            consumed_frames: total_consumed as u64,
            produced_frames: total_produced as u64,
            expected_produced_frames: (trials * expected_per_trial) as u64,
            terminal_finished: valid,
            terminal_idempotent: valid,
            lifecycle_counters_valid: true,
            expected_timing_samples: trials,
        },
        baseline_gate,
    )
}

fn benchmark_convolver_finish(ir: &[f64], trials: usize) -> Result<LifecycleCase, String> {
    let mut samples = Vec::with_capacity(trials);
    let mut valid = true;
    let mut total_tail = 0usize;
    for _ in 0..trials {
        let control = ConvolverControl::new(true);
        let mut processor = ConvolverProcessor::new(control.clone())
            .map_err(|error| format!("convolver finish processor setup failed: {error}"))?;
        processor
            .set_sample_rate(TO_RATE_HZ)
            .map_err(|error| format!("convolver finish sample rate failed: {error}"))?;
        let kernel = FFTConvolver::new(ir, CHANNELS)
            .map_err(|error| format!("convolver finish kernel setup failed: {error}"))?;
        control
            .publish_at_rate(kernel, TO_RATE_HZ)
            .map_err(|error| format!("convolver finish publish failed: {error}"))?;
        let mut input = synthetic_input(512, CHANNELS, TO_RATE_HZ);
        process_convolver(&mut processor, &mut input, CHANNELS)?;
        let mut output = vec![0.0; FINISH_BLOCK_FRAMES * CHANNELS];
        let start = Instant::now();
        let drain = drain_processor(&mut processor, &mut output, CHANNELS, 128)?;
        samples.push(elapsed_ns(start));
        valid &= drain.produced_frames == SHORT_IR_FRAMES - 1
            && drain.all_finite
            && drain.terminal_idempotent;
        total_tail += drain.produced_frames;
    }
    lifecycle_case(
        format!(
            "component=dynamic_convolver;operation=finish_drain;channels={CHANNELS};ir_frames={SHORT_IR_FRAMES};finish_block_frames={FINISH_BLOCK_FRAMES}"
        ),
        "ConvolverProcessor",
        "finish_checked_until_terminal",
        "ns/drain",
        samples,
        LifecycleWorkValidation {
            valid,
            observed_operations: trials,
            expected_operations: trials,
            consumed_frames: (trials * 512) as u64,
            produced_frames: total_tail as u64,
            expected_produced_frames: (trials * (SHORT_IR_FRAMES - 1)) as u64,
            terminal_finished: valid,
            terminal_idempotent: valid,
            lifecycle_counters_valid: true,
            expected_timing_samples: trials,
        },
        true,
    )
}

fn benchmark_dynamic_convolver(trials: usize) -> Result<DynamicMeasurements, String> {
    let mut publish_ns = Vec::with_capacity(trials);
    let mut adopt_ns = Vec::with_capacity(trials);
    let mut reclaim_ns = Vec::with_capacity(trials);
    let mut valid = true;
    for trial in 0..trials {
        let control = ConvolverControl::new(true);
        let mut processor = ConvolverProcessor::new(control.clone())
            .map_err(|error| format!("dynamic convolver processor setup failed: {error}"))?;
        processor
            .set_sample_rate(TO_RATE_HZ)
            .map_err(|error| format!("dynamic convolver sample rate failed: {error}"))?;
        let first = FFTConvolver::new(&[0.5], 1)
            .map_err(|error| format!("dynamic first kernel setup failed: {error}"))?;
        let start = Instant::now();
        let first_generation = control
            .publish_at_rate(first, TO_RATE_HZ)
            .map_err(|error| format!("timed dynamic publish failed: {error}"))?;
        publish_ns.push(elapsed_ns(start));

        let mut block = vec![0.25 + trial as f64 * 1.0e-4; 512];
        let start = Instant::now();
        let first_frames = process_convolver(&mut processor, &mut block, 1)?;
        adopt_ns.push(elapsed_ns(start));

        let replacement = FFTConvolver::new(&[0.25], 1)
            .map_err(|error| format!("dynamic replacement kernel setup failed: {error}"))?;
        let second_generation = control
            .publish_at_rate(replacement, TO_RATE_HZ)
            .map_err(|error| format!("dynamic replacement publish failed: {error}"))?;
        let second_frames = process_convolver(&mut processor, &mut block, 1)?;
        let start = Instant::now();
        let reclaimed = control.reclaim_retired();
        reclaim_ns.push(elapsed_ns(start));
        let status = control.status();
        valid &= first_generation == 1
            && second_generation == 2
            && first_frames == 512
            && second_frames == 512
            && reclaimed
            && status.latest_adopted_generation == 2
            && status.adopted_kernels == 2
            && status.reclaimed_kernels == 1;
        valid &= retire_to_quiescence(&control, &mut processor, &mut block, 1)?;
    }
    Ok(DynamicMeasurements {
        publish_ns,
        adopt_ns,
        reclaim_ns,
        valid,
    })
}

fn benchmark_soak(workload: LifecycleWorkload) -> Result<(LifecycleCase, SoakEvidence), String> {
    let mut timing_samples = Vec::with_capacity(workload.soak_trials);
    let mut retained = Vec::with_capacity(workload.soak_trials);
    let mut peaks = Vec::with_capacity(workload.soak_trials);
    let mut allocations = Vec::with_capacity(workload.soak_trials);
    let mut deallocations = Vec::with_capacity(workload.soak_trials);
    let mut all_valid = true;
    for trial in 0..workload.soak_trials {
        let result = soak_trial(workload.soak_iterations, trial)?;
        timing_samples.push(result.ns_per_cycle);
        retained.push(result.snapshot.live_bytes);
        peaks.push(result.snapshot.peak_live_bytes);
        allocations.push(result.snapshot.allocations);
        deallocations.push(result.snapshot.deallocations);
        all_valid &= result.valid;
    }
    let maximum_retained = retained.iter().copied().max().unwrap_or(0);
    let bounded = maximum_retained == 0;
    let validation = LifecycleWorkValidation {
        valid: all_valid && bounded,
        observed_operations: workload.soak_trials * workload.soak_iterations,
        expected_operations: workload.soak_trials * workload.soak_iterations,
        consumed_frames: (workload.soak_trials * workload.soak_iterations * 512) as u64,
        produced_frames: (workload.soak_trials * workload.soak_iterations * 512) as u64,
        expected_produced_frames: (workload.soak_trials * workload.soak_iterations * 512) as u64,
        terminal_finished: false,
        terminal_idempotent: false,
        lifecycle_counters_valid: all_valid,
        expected_timing_samples: workload.soak_trials,
    };
    let case = lifecycle_case(
        format!(
            "component=dynamic_convolver;operation=bounded_lifecycle_soak;iterations={};ir_frames=1;channels=1",
            workload.soak_iterations
        ),
        "ConvolverControl+ConvolverProcessor",
        "complete_lifecycle_cycle",
        "ns/cycle",
        timing_samples,
        validation,
        true,
    )?;
    Ok((
        case,
        SoakEvidence {
            trials: workload.soak_trials,
            iterations_per_trial: workload.soak_iterations,
            retained_rust_bytes_per_trial: retained,
            peak_rust_bytes_per_trial: peaks,
            allocation_calls_per_trial: allocations,
            deallocation_calls_per_trial: deallocations,
            maximum_retained_rust_bytes: maximum_retained,
            all_cycles_valid: all_valid,
            bounded,
        },
    ))
}

fn soak_trial(iterations: usize, trial: usize) -> Result<SoakTrial, String> {
    let scope = AllocationScope::start();
    let start = Instant::now();
    let valid = {
        let control = ConvolverControl::new(true);
        let mut processor = ConvolverProcessor::new(control.clone())
            .map_err(|error| format!("soak processor setup failed: {error}"))?;
        processor
            .set_sample_rate(TO_RATE_HZ)
            .map_err(|error| format!("soak sample rate setup failed: {error}"))?;
        let mut block = vec![0.2 + trial as f64 * 1.0e-5; 512];
        let mut valid = true;
        for iteration in 0..iterations {
            let gain = 0.2 + (iteration % 17) as f64 * 0.01;
            let kernel = FFTConvolver::new(&[gain], 1)
                .map_err(|error| format!("soak kernel setup failed: {error}"))?;
            let generation = control
                .publish_at_rate(kernel, TO_RATE_HZ)
                .map_err(|error| format!("soak publish failed: {error}"))?;
            let frames = process_convolver(&mut processor, &mut block, 1)?;
            let _ = control.reclaim_retired();
            let status = control.status();
            valid &= generation == (iteration + 1) as u64
                && frames == 512
                && status.pending_kernels == 0
                && status.pending_reclamations <= 1;
        }
        valid &= retire_to_quiescence(&control, &mut processor, &mut block, 1)?;
        drop(processor);
        drop(control);
        valid
    };
    let elapsed = elapsed_ns(start);
    let snapshot = scope.finish();
    Ok(SoakTrial {
        ns_per_cycle: elapsed / iterations as f64,
        snapshot,
        valid,
    })
}

fn measure_persistent_memory(
    short_ir: &[f64],
    long_ir: &[f64],
) -> Result<Vec<PersistentMemoryEvidence>, String> {
    let configured_resampler_bytes =
        StreamingResampler::working_buffer_bytes(CHANNELS, FROM_RATE_HZ, TO_RATE_HZ)
            .map_err(|error| format!("resampler working-buffer query failed: {error}"))?;
    let resampler_scope = AllocationScope::start();
    let resampler = StreamingResampler::new(CHANNELS, FROM_RATE_HZ, TO_RATE_HZ)
        .map_err(|error| format!("resampler memory setup failed: {error}"))?;
    black_box(&resampler);
    let resampler_snapshot = resampler_scope.finish();
    drop(resampler);

    let short_scope = AllocationScope::start();
    let short = FFTConvolver::new(short_ir, CHANNELS)
        .map_err(|error| format!("short convolver memory setup failed: {error}"))?;
    black_box(&short);
    let short_snapshot = short_scope.finish();
    drop(short);

    let long_scope = AllocationScope::start();
    let long = FFTConvolver::new(long_ir, CHANNELS)
        .map_err(|error| format!("long convolver memory setup failed: {error}"))?;
    black_box(&long);
    let long_snapshot = long_scope.finish();
    drop(long);
    let resampler_boundary = resampler_allocation_boundary();

    Ok(vec![
        persistent_memory(
            "StreamingResampler",
            &format!(
                "channels={CHANNELS};from={FROM_RATE_HZ};to={TO_RATE_HZ};backend={RESAMPLER_BACKEND_NAME}"
            ),
            resampler_snapshot,
            Some(configured_resampler_bytes),
            &resampler_boundary,
        ),
        persistent_memory(
            "FFTConvolver",
            &format!("channels={CHANNELS};ir_frames={SHORT_IR_FRAMES};strategy=overlap-save"),
            short_snapshot,
            None,
            "all constructor allocations routed through the Rust global allocator while the convolver remains alive",
        ),
        persistent_memory(
            "FFTConvolver",
            &format!("channels={CHANNELS};ir_frames={LONG_IR_FRAMES};strategy=partitioned"),
            long_snapshot,
            None,
            "all constructor allocations routed through the Rust global allocator while the convolver remains alive",
        ),
    ])
}

fn native_allocation_disclosure() -> String {
    if RESAMPLER_BACKEND_NAME == "soxr" {
        "soxr: Rust global allocation hooks cannot observe libsoxr malloc/calloc; StreamingResampler::working_buffer_bytes reports exact adapter-owned PCM scratch only"
            .to_string()
    } else {
        format!(
            "{RESAMPLER_BACKEND_NAME}: backend allocations routed through Rust are counted; StreamingResampler::working_buffer_bytes reports adapter-owned PCM scratch only, and no opaque engine estimate is invented"
        )
    }
}

fn resampler_allocation_boundary() -> String {
    if RESAMPLER_BACKEND_NAME == "soxr" {
        "Rust setup allocations plus exact adapter-owned reusable PCM scratch; native libsoxr allocations are excluded"
            .to_string()
    } else {
        "Rust setup allocations routed through the global allocator plus exact adapter-owned reusable PCM scratch"
            .to_string()
    }
}

fn measure_lifecycle_allocations(
    input: &[f64],
    short_ir: &[f64],
) -> Result<Vec<LifecycleAllocationEvidence>, String> {
    let mut evidence = Vec::with_capacity(EXPECTED_ALLOCATION_OPERATIONS.len());

    let setup_scope = AllocationScope::start();
    let equal_rate = StreamingResampler::new(CHANNELS, FROM_RATE_HZ, FROM_RATE_HZ)
        .map_err(|error| format!("equal-rate allocation setup failed: {error}"))?;
    black_box(&equal_rate);
    let setup_snapshot = setup_scope.finish();
    let setup_valid =
        equal_rate.from_rate() == FROM_RATE_HZ && equal_rate.to_rate() == FROM_RATE_HZ;
    drop(equal_rate);
    evidence.push(lifecycle_allocation_evidence(
        "resampler_equal_rate_setup",
        setup_snapshot,
        false,
        setup_valid,
        "Rust setup allocations captured while the equal-rate bypass object remains alive",
    ));

    let mut resampler = StreamingResampler::new(CHANNELS, FROM_RATE_HZ, TO_RATE_HZ)
        .map_err(|error| format!("resampler reset allocation setup failed: {error}"))?;
    let before_reset = feed_resampler(&mut resampler, input)?;
    let reset_scope = AllocationScope::start();
    resampler
        .reset()
        .map_err(|error| format!("resampler allocation reset failed: {error}"))?;
    let reset_snapshot = reset_scope.finish();
    let after_reset = feed_resampler(&mut resampler, input)?;
    evidence.push(lifecycle_allocation_evidence(
        "resampler_active_reset",
        reset_snapshot,
        true,
        before_reset == after_reset,
        "active resampler object and caller input/output storage are created before the reset scope",
    ));

    evidence.push(measure_resampler_finish_allocation(
        input,
        FROM_RATE_HZ,
        TO_RATE_HZ,
        "resampler_active_finish",
    )?);
    evidence.push(measure_resampler_finish_allocation(
        input,
        FROM_RATE_HZ,
        FROM_RATE_HZ,
        "resampler_equal_rate_finish",
    )?);

    let control = ConvolverControl::new(true);
    let mut processor = ConvolverProcessor::new(control.clone())
        .map_err(|error| format!("convolver reset allocation setup failed: {error}"))?;
    processor
        .set_sample_rate(TO_RATE_HZ)
        .map_err(|error| format!("convolver reset allocation sample rate failed: {error}"))?;
    let kernel = FFTConvolver::new(short_ir, CHANNELS)
        .map_err(|error| format!("convolver reset allocation kernel failed: {error}"))?;
    control
        .publish_at_rate(kernel, TO_RATE_HZ)
        .map_err(|error| format!("convolver reset allocation publish failed: {error}"))?;
    let mut convolver_block = synthetic_input(512, CHANNELS, TO_RATE_HZ);
    process_convolver(&mut processor, &mut convolver_block, CHANNELS)?;
    let convolver_reset_scope = AllocationScope::start();
    processor
        .reset()
        .map_err(|error| format!("convolver allocation reset failed: {error}"))?;
    let convolver_reset_snapshot = convolver_reset_scope.finish();
    let reset_frames = process_convolver(&mut processor, &mut convolver_block, CHANNELS)?;
    evidence.push(lifecycle_allocation_evidence(
        "convolver_reset",
        convolver_reset_snapshot,
        true,
        reset_frames == 512 && control.status().latest_adopted_generation == 1,
        "Convolver kernel, processor, and caller block are created before the reset scope",
    ));

    let finish_control = ConvolverControl::new(true);
    let mut finish_processor = ConvolverProcessor::new(finish_control.clone())
        .map_err(|error| format!("convolver finish allocation setup failed: {error}"))?;
    finish_processor
        .set_sample_rate(TO_RATE_HZ)
        .map_err(|error| format!("convolver finish allocation sample rate failed: {error}"))?;
    let finish_kernel = FFTConvolver::new(short_ir, CHANNELS)
        .map_err(|error| format!("convolver finish allocation kernel failed: {error}"))?;
    finish_control
        .publish_at_rate(finish_kernel, TO_RATE_HZ)
        .map_err(|error| format!("convolver finish allocation publish failed: {error}"))?;
    let mut finish_input = synthetic_input(512, CHANNELS, TO_RATE_HZ);
    process_convolver(&mut finish_processor, &mut finish_input, CHANNELS)?;
    let mut finish_output = vec![0.0; FINISH_BLOCK_FRAMES * CHANNELS];
    let convolver_finish_scope = AllocationScope::start();
    let convolver_drain =
        drain_processor(&mut finish_processor, &mut finish_output, CHANNELS, 128)?;
    let convolver_finish_snapshot = convolver_finish_scope.finish();
    evidence.push(lifecycle_allocation_evidence(
        "convolver_finish",
        convolver_finish_snapshot,
        true,
        convolver_drain.produced_frames == SHORT_IR_FRAMES - 1
            && convolver_drain.all_finite
            && convolver_drain.terminal_idempotent,
        "Convolver state and caller-owned finish output are allocated before the complete drain scope",
    ));

    evidence.extend(measure_dynamic_allocation_operations()?);
    Ok(evidence)
}

fn measure_resampler_finish_allocation(
    input: &[f64],
    from_rate_hz: u32,
    to_rate_hz: u32,
    operation: &'static str,
) -> Result<LifecycleAllocationEvidence, String> {
    let mut resampler = StreamingResampler::new(CHANNELS, from_rate_hz, to_rate_hz)
        .map_err(|error| format!("{operation} setup failed: {error}"))?;
    let (consumed, process_produced) = feed_resampler(&mut resampler, input)?;
    let mut output = vec![0.0; FINISH_BLOCK_FRAMES * CHANNELS * 64];
    let scope = AllocationScope::start();
    let drain = drain_processor(&mut resampler, &mut output, CHANNELS, 128)?;
    let snapshot = scope.finish();
    let expected =
        (PROCESS_FRAMES as f64 * to_rate_hz as f64 / from_rate_hz as f64).round() as usize;
    Ok(lifecycle_allocation_evidence(
        operation,
        snapshot,
        true,
        consumed == PROCESS_FRAMES
            && process_produced + drain.produced_frames == expected
            && drain.all_finite
            && drain.terminal_idempotent,
        "resampler state and caller-owned finish output are allocated before the complete drain scope",
    ))
}

fn measure_dynamic_allocation_operations() -> Result<Vec<LifecycleAllocationEvidence>, String> {
    let control = ConvolverControl::new(true);
    let mut processor = ConvolverProcessor::new(control.clone())
        .map_err(|error| format!("dynamic allocation processor setup failed: {error}"))?;
    processor
        .set_sample_rate(TO_RATE_HZ)
        .map_err(|error| format!("dynamic allocation sample rate failed: {error}"))?;
    let first = FFTConvolver::new(&[0.5], 1)
        .map_err(|error| format!("dynamic allocation first kernel failed: {error}"))?;

    let publish_scope = AllocationScope::start();
    let first_generation = control
        .publish_at_rate(first, TO_RATE_HZ)
        .map_err(|error| format!("dynamic allocation publish failed: {error}"))?;
    let publish_snapshot = publish_scope.finish();

    let mut block = vec![0.25; 512];
    let adopt_scope = AllocationScope::start();
    let adopted_frames = process_convolver(&mut processor, &mut block, 1)?;
    let adopt_snapshot = adopt_scope.finish();

    let replacement = FFTConvolver::new(&[0.25], 1)
        .map_err(|error| format!("dynamic allocation replacement kernel failed: {error}"))?;
    let second_generation = control
        .publish_at_rate(replacement, TO_RATE_HZ)
        .map_err(|error| format!("dynamic allocation replacement publish failed: {error}"))?;
    let replacement_frames = process_convolver(&mut processor, &mut block, 1)?;
    let reclaim_scope = AllocationScope::start();
    let reclaimed = control.reclaim_retired();
    let reclaim_snapshot = reclaim_scope.finish();
    let status = control.status();

    let valid = first_generation == 1
        && second_generation == 2
        && adopted_frames == 512
        && replacement_frames == 512
        && reclaimed
        && status.latest_adopted_generation == 2
        && status.reclaimed_kernels == 1;
    let quiescent = retire_to_quiescence(&control, &mut processor, &mut block, 1)?;

    Ok(vec![
        lifecycle_allocation_evidence(
            "dynamic_convolver_publish",
            publish_snapshot,
            false,
            valid && first_generation == 1,
            "the FFTConvolver kernel is built before timing; control publication may allocate ownership storage retained after the scope",
        ),
        lifecycle_allocation_evidence(
            "dynamic_convolver_adopt",
            adopt_snapshot,
            true,
            valid && adopted_frames == 512,
            "published ownership and the caller block exist before the audio-side adoption/process scope",
        ),
        lifecycle_allocation_evidence(
            "dynamic_convolver_reclaim",
            reclaim_snapshot,
            false,
            valid && quiescent,
            "control-side reclamation may deallocate ownership created before this isolated scope",
        ),
    ])
}

fn lifecycle_allocation_evidence(
    operation: &str,
    snapshot: AllocationSnapshot,
    rust_allocation_free_required: bool,
    valid: bool,
    boundary: &str,
) -> LifecycleAllocationEvidence {
    LifecycleAllocationEvidence {
        operation: operation.to_string(),
        allocation_calls: snapshot.allocations,
        deallocation_calls: snapshot.deallocations,
        reallocation_calls: snapshot.reallocations,
        peak_live_bytes: snapshot.peak_live_bytes,
        retained_bytes: snapshot.live_bytes,
        rust_allocation_free_required,
        valid,
        allocation_boundary: boundary.to_string(),
    }
}

fn persistent_memory(
    component: &str,
    configuration: &str,
    snapshot: AllocationSnapshot,
    configured_working_buffer_bytes: Option<usize>,
    boundary: &str,
) -> PersistentMemoryEvidence {
    PersistentMemoryEvidence {
        component: component.to_string(),
        configuration: configuration.to_string(),
        setup_allocation_calls: snapshot.allocations,
        setup_deallocation_calls: snapshot.deallocations,
        setup_reallocation_calls: snapshot.reallocations,
        setup_peak_live_bytes: snapshot.peak_live_bytes,
        setup_retained_rust_bytes: snapshot.live_bytes,
        configured_working_buffer_bytes,
        allocation_boundary: boundary.to_string(),
    }
}

fn feed_resampler(
    resampler: &mut StreamingResampler,
    input: &[f64],
) -> Result<(usize, usize), String> {
    let input_frames = input.len() / CHANNELS;
    let output_samples = resampler
        .max_output_len_for_input(input.len())
        .saturating_mul(8)
        .saturating_add(8_192);
    let mut output = vec![0.0; output_samples];
    let mut consumed = 0usize;
    let mut produced = 0usize;
    let mut calls = 0usize;
    while consumed < input_frames {
        let input_block = AudioBlockRef::new(&input[consumed * CHANNELS..], CHANNELS)
            .map_err(|error| format!("resampler input block failed: {error}"))?;
        let output_block = AudioBlockMut::new(&mut output, CHANNELS)
            .map_err(|error| format!("resampler output block failed: {error}"))?;
        let buffers = ProcessBuffers::out_of_place(input_block, output_block)
            .map_err(|error| format!("resampler buffer pairing failed: {error}"))?;
        let progress = process_checked(resampler, buffers)
            .map_err(|error| format!("resampler process failed: {error}"))?;
        consumed += progress.consumed_frames();
        produced += progress.produced_frames();
        black_box(&output[..progress.produced_frames() * CHANNELS]);
        calls += 1;
        if calls > 256 {
            return Err("resampler process exceeded bounded call count".to_string());
        }
    }
    Ok((consumed, produced))
}

fn process_convolver(
    processor: &mut ConvolverProcessor,
    buffer: &mut [f64],
    channels: usize,
) -> Result<usize, String> {
    let block = AudioBlockMut::new(buffer, channels)
        .map_err(|error| format!("convolver process block failed: {error}"))?;
    let progress = process_checked(processor, ProcessBuffers::in_place(block))
        .map_err(|error| format!("convolver process failed: {error}"))?;
    black_box(buffer);
    Ok(progress.produced_frames())
}

fn drain_processor(
    processor: &mut impl StreamingProcessor,
    output: &mut [f64],
    channels: usize,
    max_calls: usize,
) -> Result<DrainResult, String> {
    let mut produced_frames = 0usize;
    let mut calls = 0usize;
    let mut all_finite = true;
    loop {
        output.fill(0.0);
        let block = AudioBlockMut::new(output, channels)
            .map_err(|error| format!("finish output block failed: {error}"))?;
        let progress = finish_checked(processor, block)
            .map_err(|error| format!("finish_checked failed: {error}"))?;
        let produced = progress.produced_frames();
        produced_frames += produced;
        all_finite &= output[..produced * channels]
            .iter()
            .all(|sample| sample.is_finite());
        black_box(&output[..produced * channels]);
        calls += 1;
        match progress.state() {
            ProcessState::Finished => break,
            ProcessState::NeedOutput => {}
            ProcessState::NeedInput => {
                return Err("finish_checked returned NeedInput".to_string());
            }
        }
        if calls >= max_calls {
            return Err(format!("finish exceeded bounded call count {max_calls}"));
        }
    }
    output.fill(0.0);
    let block = AudioBlockMut::new(output, channels)
        .map_err(|error| format!("terminal finish block failed: {error}"))?;
    let terminal = finish_checked(processor, block)
        .map_err(|error| format!("terminal finish repeat failed: {error}"))?;
    Ok(DrainResult {
        produced_frames,
        all_finite,
        terminal_idempotent: terminal.state() == ProcessState::Finished
            && terminal.produced_frames() == 0,
    })
}

fn retire_to_quiescence(
    control: &ConvolverControl,
    processor: &mut ConvolverProcessor,
    block: &mut [f64],
    channels: usize,
) -> Result<bool, String> {
    control.set_enabled(false);
    for _ in 0..8 {
        let _ = process_convolver(processor, block, channels)?;
        while control.reclaim_retired() {}
        if control.is_quiescent() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn basic_validation(valid: bool, trials: usize) -> LifecycleWorkValidation {
    LifecycleWorkValidation {
        valid,
        observed_operations: trials,
        expected_operations: trials,
        consumed_frames: 0,
        produced_frames: 0,
        expected_produced_frames: 0,
        terminal_finished: false,
        terminal_idempotent: false,
        lifecycle_counters_valid: valid,
        expected_timing_samples: trials,
    }
}

fn lifecycle_case(
    case_key: String,
    component: &str,
    operation: &str,
    primary_unit: &str,
    samples: Vec<f64>,
    work_validation: LifecycleWorkValidation,
    baseline_gate: bool,
) -> Result<LifecycleCase, String> {
    let expected_timing_samples = work_validation.expected_timing_samples;
    Ok(LifecycleCase {
        case_key,
        component: component.to_string(),
        operation: operation.to_string(),
        primary_unit: primary_unit.to_string(),
        baseline_gate,
        expected_timing_samples,
        distribution: summarize_trials(samples)?,
        work_validation,
    })
}

fn synthetic_input(frames: usize, channels: usize, sample_rate_hz: u32) -> Vec<f64> {
    let mut output = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        let time = frame as f64 / sample_rate_hz as f64;
        for channel in 0..channels {
            let frequency = 223.0 + channel as f64 * 179.0;
            output.push(
                ((std::f64::consts::TAU * frequency * time).sin() * 0.45
                    + (std::f64::consts::TAU * 3.0 * time).cos() * 0.05)
                    .clamp(-0.9, 0.9),
            );
        }
    }
    output
}

fn synthetic_ir(frames: usize, channels: usize) -> Vec<f64> {
    let mut output = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        let decay = (-6.0 * frame as f64 / frames.max(1) as f64).exp();
        for channel in 0..channels {
            let value = if frame == 0 {
                0.8
            } else {
                decay
                    * 0.03
                    * if (frame + channel) & 1 == 0 {
                        1.0
                    } else {
                        -1.0
                    }
            };
            output.push(value);
        }
    }
    output
}

fn elapsed_ns(start: Instant) -> f64 {
    start.elapsed().as_nanos().max(1) as f64
}

fn print_report(report: &LifecycleReport) -> Result<(), String> {
    println!(
        "audio_lifecycle_memory_perf mode={} backend={} cases={} soak={}x{} environment={}",
        report.mode.as_str(),
        report.conditions.backend,
        report.cases.len(),
        report.soak.trials,
        report.soak.iterations_per_trial,
        environment_json(&report.environment)?
    );
    for case in &report.cases {
        println!(
            "lifecycle case={} unit={} median={:.3} p95={:.3} max={:.3} trials={} baseline_gate={} valid={}",
            case.case_key,
            case.primary_unit,
            case.distribution.median,
            case.distribution.p95,
            case.distribution.max,
            case.distribution.samples.len(),
            case.baseline_gate,
            case.work_validation.valid
        );
    }
    for allocation in &report.lifecycle_allocations {
        println!(
            "lifecycle allocation operation={} allocs={} deallocs={} reallocs={} peak_bytes={} retained_bytes={} rust_allocation_free_required={} valid={}",
            allocation.operation,
            allocation.allocation_calls,
            allocation.deallocation_calls,
            allocation.reallocation_calls,
            allocation.peak_live_bytes,
            allocation.retained_bytes,
            allocation.rust_allocation_free_required,
            allocation.valid
        );
    }
    for memory in &report.persistent_memory {
        println!(
            "lifecycle memory component={} config={} allocs={} peak_bytes={} retained_rust_bytes={} configured_working_bytes={:?}",
            memory.component,
            memory.configuration,
            memory.setup_allocation_calls,
            memory.setup_peak_live_bytes,
            memory.setup_retained_rust_bytes,
            memory.configured_working_buffer_bytes
        );
    }
    println!(
        "lifecycle soak valid={} bounded={} max_retained_rust_bytes={}",
        report.soak.all_cycles_valid, report.soak.bounded, report.soak.maximum_retained_rust_bytes
    );
    Ok(())
}

fn expected_lifecycle_case_keys(soak_iterations: usize) -> Vec<String> {
    vec![
        format!(
            "component=resampler;operation=setup;channels={CHANNELS};from={FROM_RATE_HZ};to={TO_RATE_HZ};backend={RESAMPLER_BACKEND_NAME}"
        ),
        format!(
            "component=resampler;operation=setup;channels={CHANNELS};from={FROM_RATE_HZ};to={FROM_RATE_HZ};backend={RESAMPLER_BACKEND_NAME}"
        ),
        format!(
            "component=fft_convolver;operation=setup;channels={CHANNELS};ir_frames={SHORT_IR_FRAMES}"
        ),
        format!(
            "component=fft_convolver;operation=setup;channels={CHANNELS};ir_frames={LONG_IR_FRAMES}"
        ),
        format!(
            "component=resampler;operation=reset_after_process;channels={CHANNELS};from={FROM_RATE_HZ};to={TO_RATE_HZ};backend={RESAMPLER_BACKEND_NAME}"
        ),
        format!(
            "component=dynamic_convolver;operation=reset_after_process;channels={CHANNELS};ir_frames={SHORT_IR_FRAMES}"
        ),
        format!(
            "component=resampler;operation=finish_drain;channels={CHANNELS};input_frames={PROCESS_FRAMES};from={FROM_RATE_HZ};to={TO_RATE_HZ};backend={RESAMPLER_BACKEND_NAME}"
        ),
        format!(
            "component=resampler;operation=finish_drain;channels={CHANNELS};input_frames={PROCESS_FRAMES};from={FROM_RATE_HZ};to={FROM_RATE_HZ};backend={RESAMPLER_BACKEND_NAME}"
        ),
        format!(
            "component=dynamic_convolver;operation=finish_drain;channels={CHANNELS};ir_frames={SHORT_IR_FRAMES};finish_block_frames={FINISH_BLOCK_FRAMES}"
        ),
        "component=dynamic_convolver;operation=publish;ir_frames=1;channels=1".to_string(),
        "component=dynamic_convolver;operation=adopt_process;ir_frames=1;channels=1;block_frames=512"
            .to_string(),
        "component=dynamic_convolver;operation=reclaim_retired;ir_frames=1;channels=1"
            .to_string(),
        format!(
            "component=dynamic_convolver;operation=bounded_lifecycle_soak;iterations={soak_iterations};ir_frames=1;channels=1"
        ),
    ]
}

fn enforce_report(report: &LifecycleReport) -> Result<(), String> {
    if report.conditions.backend != RESAMPLER_BACKEND_NAME {
        return Err(format!(
            "lifecycle report backend '{}' differs from compiled backend '{RESAMPLER_BACKEND_NAME}'",
            report.conditions.backend
        ));
    }
    validate_case_key_set(
        report.cases.iter().map(|case| case.case_key.clone()),
        expected_lifecycle_case_keys(report.conditions.soak_iterations),
        "lifecycle",
    )?;
    let invalid = report
        .cases
        .iter()
        .filter(|case| {
            !case.work_validation.valid
                || case.distribution.samples.len() != case.expected_timing_samples
                || case.expected_timing_samples != case.work_validation.expected_timing_samples
                || case
                    .distribution
                    .samples
                    .iter()
                    .any(|sample| !sample.is_finite() || *sample <= 0.0)
        })
        .map(|case| case.case_key.as_str())
        .collect::<Vec<_>>();
    if !invalid.is_empty() {
        return Err(format!(
            "lifecycle work/report integrity gate failed for cases: {}",
            invalid.join(", ")
        ));
    }
    if report.conditions.case_keys
        != report
            .cases
            .iter()
            .map(|case| case.case_key.clone())
            .collect::<Vec<_>>()
    {
        return Err("lifecycle condition case keys differ from measured cases".to_string());
    }
    let baseline_gate_case_keys = report
        .cases
        .iter()
        .filter(|case| case.baseline_gate)
        .map(|case| case.case_key.clone())
        .collect::<Vec<_>>();
    if report.conditions.baseline_gate_case_keys != baseline_gate_case_keys {
        return Err(
            "lifecycle baseline-gate case keys differ from measured classifications".to_string(),
        );
    }
    if report.persistent_memory.len() != 3
        || report
            .persistent_memory
            .iter()
            .any(|memory| memory.setup_retained_rust_bytes == 0)
    {
        return Err(
            "persistent memory evidence is missing or reports zero retained Rust storage"
                .to_string(),
        );
    }
    validate_case_key_set(
        report
            .lifecycle_allocations
            .iter()
            .map(|allocation| allocation.operation.clone()),
        EXPECTED_ALLOCATION_OPERATIONS.map(str::to_string),
        "lifecycle allocation",
    )?;
    let invalid_allocations = report
        .lifecycle_allocations
        .iter()
        .filter(|allocation| {
            !allocation.valid
                || (allocation.rust_allocation_free_required
                    && (allocation.allocation_calls != 0
                        || allocation.deallocation_calls != 0
                        || allocation.reallocation_calls != 0
                        || allocation.retained_bytes != 0))
        })
        .map(|allocation| allocation.operation.as_str())
        .collect::<Vec<_>>();
    if !invalid_allocations.is_empty() {
        return Err(format!(
            "lifecycle allocation gate failed for operations: {}",
            invalid_allocations.join(", ")
        ));
    }
    if !report.soak.all_cycles_valid || !report.soak.bounded {
        return Err(format!(
            "bounded lifecycle soak failed: valid={}, maximum retained Rust bytes={}",
            report.soak.all_cycles_valid, report.soak.maximum_retained_rust_bytes
        ));
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "lifecycle median regression gate failed",
        "primary-unit",
    ) {
        return Err(error);
    }
    Ok(())
}
