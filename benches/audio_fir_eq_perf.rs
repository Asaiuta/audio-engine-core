use std::collections::BTreeSet;
use std::hint::black_box;
use std::time::Instant;

use audio_engine_core::processor::{ConvolutionStrategy, FFTConvolver, FirEq, FirPhaseMode};
use serde::{Deserialize, Serialize};

pub mod support;

use support::{
    compare_case_medians, environment_json, generated_unix_ms, read_json, regression_gate_error,
    summarize_trials, validate_performance_baseline, write_json, BenchEnvironment, BenchMode,
    PerfArgs, PerformanceReportIdentity, RegressionComparison, TrialDistribution,
    REPORT_SCHEMA_VERSION,
};

const SAMPLE_RATE: f64 = 48_000.0;
const CHANNELS: usize = 2;
const PROCESS_FRAMES: usize = 512;
const TAP_COUNTS: [usize; 3] = [511, 1023, 2047];
const REGEN_WARMUP_CURVES: usize = 2;
const APPLY_WARMUP_BUFFERS: usize = 64;

#[derive(Clone, Copy)]
enum Phase {
    Linear,
    Minimum,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Minimum => "minimum",
        }
    }

    fn mode(self) -> FirPhaseMode {
        match self {
            Self::Linear => FirPhaseMode::Linear,
            Self::Minimum => FirPhaseMode::Minimum,
        }
    }

    fn all() -> &'static [Self] {
        &[Self::Linear, Self::Minimum]
    }
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct FirEqConditions {
    sample_rate_hz: u32,
    channels: usize,
    process_frames: usize,
    tap_counts: Vec<usize>,
    phases: Vec<String>,
    regeneration_iterations_per_trial: usize,
    apply_iterations_per_trial: usize,
    trials: usize,
    regeneration_warmup_curves: usize,
    apply_warmup_buffers: usize,
    coverage: String,
    excludes: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RegenerationValidation {
    valid: bool,
    expected_ir_length: usize,
    actual_ir_length: usize,
    all_ir_samples_finite: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ApplyValidation {
    valid: bool,
    expected_ir_length: usize,
    actual_ir_length: usize,
    all_ir_samples_finite: bool,
    all_output_samples_finite: bool,
    output_changed: bool,
    expected_strategy: String,
    actual_strategy: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FirEqCase {
    Regeneration {
        case_key: String,
        phase: String,
        taps: usize,
        ir_length: usize,
        primary_unit: String,
        ns_per_regeneration: TrialDistribution,
        regenerations_per_ms_median: f64,
        work_validation: RegenerationValidation,
    },
    Apply {
        case_key: String,
        phase: String,
        taps: usize,
        ir_length: usize,
        strategy: String,
        fft_size: usize,
        partition_size: Option<usize>,
        frames: usize,
        samples: usize,
        processed_samples_per_trial: usize,
        primary_unit: String,
        ns_per_sample: TrialDistribution,
        ns_per_buffer: TrialDistribution,
        work_validation: ApplyValidation,
    },
}

impl FirEqCase {
    fn case_key(&self) -> &str {
        match self {
            Self::Regeneration { case_key, .. } | Self::Apply { case_key, .. } => case_key,
        }
    }

    fn primary_median(&self) -> f64 {
        match self {
            Self::Regeneration {
                ns_per_regeneration,
                ..
            } => ns_per_regeneration.median,
            Self::Apply { ns_per_sample, .. } => ns_per_sample.median,
        }
    }

    fn has_valid_work(&self) -> bool {
        match self {
            Self::Regeneration {
                work_validation, ..
            } => work_validation.valid,
            Self::Apply {
                work_validation, ..
            } => work_validation.valid,
        }
    }

    fn has_complete_trials(&self, expected_trials: usize) -> bool {
        match self {
            Self::Regeneration {
                ns_per_regeneration,
                ..
            } => ns_per_regeneration.samples.len() == expected_trials,
            Self::Apply {
                ns_per_sample,
                ns_per_buffer,
                ..
            } => {
                ns_per_sample.samples.len() == expected_trials
                    && ns_per_buffer.samples.len() == expected_trials
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
struct FirEqReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: FirEqConditions,
    cases: Vec<FirEqCase>,
    baseline: Option<BaselineReference>,
    comparisons: Vec<RegressionComparison>,
}

struct ApplyMeasurement {
    ns_per_sample: f64,
    ns_per_buffer: f64,
}

fn main() -> Result<(), String> {
    let args = PerfArgs::parse(std::env::args().skip(1).collect())?;
    if args.help {
        print_help();
        return Ok(());
    }

    let (regeneration_iterations, apply_iterations, trials) = workload(args.mode);
    let conditions = FirEqConditions {
        sample_rate_hz: SAMPLE_RATE as u32,
        channels: CHANNELS,
        process_frames: PROCESS_FRAMES,
        tap_counts: TAP_COUNTS.to_vec(),
        phases: Phase::all()
            .iter()
            .map(|phase| phase.name().to_string())
            .collect(),
        regeneration_iterations_per_trial: regeneration_iterations,
        apply_iterations_per_trial: apply_iterations,
        trials,
        regeneration_warmup_curves: REGEN_WARMUP_CURVES,
        apply_warmup_buffers: APPLY_WARMUP_BUFFERS,
        coverage: "fir_ir_generation+convolver_apply".to_string(),
        excludes: ["cpal_device_write", "decoder", "parameter_transport"]
            .into_iter()
            .map(str::to_string)
            .collect(),
    };

    let mut cases = Vec::new();
    for &phase in Phase::all() {
        for taps in TAP_COUNTS {
            cases.push(benchmark_regeneration(
                phase,
                taps,
                regeneration_iterations,
                trials,
            )?);
        }
    }
    for taps in TAP_COUNTS {
        cases.push(benchmark_apply(taps, apply_iterations, trials)?);
    }

    let mut report = FirEqReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: "audio_fir_eq_perf".to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment: BenchEnvironment::capture(),
        conditions,
        cases,
        baseline: None,
        comparisons: Vec::new(),
    };

    if let Some(path) = &args.baseline {
        let baseline: FirEqReport = read_json(path, "FIR EQ baseline report")?;
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
        write_json(path, &report, "FIR EQ performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }
    Ok(())
}

fn workload(mode: BenchMode) -> (usize, usize, usize) {
    match mode {
        // Each quick trial intentionally lasts tens to hundreds of
        // milliseconds. The previous 200/400-operation windows were short
        // enough for Windows scheduling and CPU-frequency changes to move
        // successive same-binary medians by 10-45%, defeating a 10% gate.
        BenchMode::Quick => (1_000, 2_000, 7),
        BenchMode::Full => (4_000, 8_000, 9),
        BenchMode::Heavy => (12_000, 24_000, 15),
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_fir_eq_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>]\n\
         \n\
         Reports raw trials plus min/median/p95/max for FIR regeneration and apply.\n\
         Timing is report-only unless a compatible same-machine baseline is supplied."
    );
}

fn benchmark_regeneration(
    phase: Phase,
    taps: usize,
    iterations: usize,
    trials: usize,
) -> Result<FirEqCase, String> {
    let validation = validate_regeneration(phase, taps);
    let mut samples = Vec::with_capacity(trials);
    for _ in 0..trials {
        let mut fir = FirEq::new(SAMPLE_RATE, taps);
        fir.set_phase_mode(phase.mode());
        warm_regeneration(&mut fir);

        let start = Instant::now();
        for iteration in 0..iterations {
            let curve = if iteration.is_multiple_of(2) {
                &STANDARD_TEST_CURVE
            } else {
                &ALT_TEST_CURVE
            };
            fir.set_bands(black_box(curve));
            black_box(fir.ir_length());
        }
        samples.push(start.elapsed().as_nanos() as f64 / iterations as f64);
    }

    let ns_per_regeneration = summarize_trials(samples)?;
    Ok(FirEqCase::Regeneration {
        case_key: format!("kind=regeneration;phase={};taps={taps}", phase.name()),
        phase: phase.name().to_string(),
        taps,
        ir_length: validation.actual_ir_length,
        primary_unit: "ns/regeneration".to_string(),
        regenerations_per_ms_median: 1_000_000.0 / ns_per_regeneration.median,
        ns_per_regeneration,
        work_validation: validation,
    })
}

fn validate_regeneration(phase: Phase, taps: usize) -> RegenerationValidation {
    let mut fir = FirEq::new(SAMPLE_RATE, taps);
    fir.set_phase_mode(phase.mode());
    fir.set_bands(&STANDARD_TEST_CURVE);
    let ir = fir.get_ir(1);
    let actual_ir_length = fir.ir_length();
    let all_ir_samples_finite = ir.iter().all(|sample| sample.is_finite());
    RegenerationValidation {
        valid: actual_ir_length == taps && all_ir_samples_finite,
        expected_ir_length: taps,
        actual_ir_length,
        all_ir_samples_finite,
    }
}

fn warm_regeneration(fir: &mut FirEq) {
    for curve in [&WARM_CURVE, &STANDARD_TEST_CURVE]
        .into_iter()
        .take(REGEN_WARMUP_CURVES)
    {
        fir.set_bands(curve);
    }
}

fn benchmark_apply(taps: usize, iterations: usize, trials: usize) -> Result<FirEqCase, String> {
    let (validation, fft_size, strategy, partition_size) = validate_apply(taps);
    let input = synthetic_input(PROCESS_FRAMES, CHANNELS);
    let mut ns_per_sample = Vec::with_capacity(trials);
    let mut ns_per_buffer = Vec::with_capacity(trials);

    for _ in 0..trials {
        let mut convolver = build_apply_convolver(taps);
        let mut output = vec![0.0; input.len()];
        warm_apply(&mut convolver, &input, &mut output);
        let measurement = measure_apply(
            &mut convolver,
            &input,
            &mut output,
            iterations,
            PROCESS_FRAMES,
        );
        ns_per_sample.push(measurement.ns_per_sample);
        ns_per_buffer.push(measurement.ns_per_buffer);
    }

    Ok(FirEqCase::Apply {
        case_key: format!(
            "kind=apply;phase=linear;taps={taps};frames={PROCESS_FRAMES};channels={CHANNELS};strategy={}",
            strategy_name(strategy)
        ),
        phase: "linear".to_string(),
        taps,
        ir_length: validation.actual_ir_length,
        strategy: strategy_name(strategy).to_string(),
        fft_size,
        partition_size,
        frames: PROCESS_FRAMES,
        samples: PROCESS_FRAMES * CHANNELS,
        processed_samples_per_trial: PROCESS_FRAMES * CHANNELS * iterations,
        primary_unit: "ns/sample".to_string(),
        ns_per_sample: summarize_trials(ns_per_sample)?,
        ns_per_buffer: summarize_trials(ns_per_buffer)?,
        work_validation: validation,
    })
}

fn validate_apply(taps: usize) -> (ApplyValidation, usize, ConvolutionStrategy, Option<usize>) {
    let mut fir = FirEq::new(SAMPLE_RATE, taps);
    fir.set_phase_mode(FirPhaseMode::Linear);
    fir.set_bands(&STANDARD_TEST_CURVE);
    let ir = fir.get_ir(CHANNELS);
    let actual_ir_length = fir.ir_length();
    let all_ir_samples_finite = ir.iter().all(|sample| sample.is_finite());
    let mut convolver = FFTConvolver::new(&ir, CHANNELS);
    let fft_size = convolver.fft_size();
    let strategy = convolver.strategy();
    let partition_size = convolver.partition_size();
    let input = synthetic_input(PROCESS_FRAMES, CHANNELS);
    let mut output = vec![0.0; input.len()];
    warm_apply(&mut convolver, &input, &mut output);
    convolver.process_into(&input, &mut output);
    let all_output_samples_finite = output.iter().all(|sample| sample.is_finite());
    let output_changed = output
        .iter()
        .zip(&input)
        .any(|(actual, source)| actual.to_bits() != source.to_bits());
    let expected_strategy = ConvolutionStrategy::OverlapSave;
    let validation = ApplyValidation {
        valid: actual_ir_length == taps
            && all_ir_samples_finite
            && all_output_samples_finite
            && output_changed
            && strategy == expected_strategy,
        expected_ir_length: taps,
        actual_ir_length,
        all_ir_samples_finite,
        all_output_samples_finite,
        output_changed,
        expected_strategy: strategy_name(expected_strategy).to_string(),
        actual_strategy: strategy_name(strategy).to_string(),
    };
    (validation, fft_size, strategy, partition_size)
}

fn build_apply_convolver(taps: usize) -> FFTConvolver {
    let mut fir = FirEq::new(SAMPLE_RATE, taps);
    fir.set_phase_mode(FirPhaseMode::Linear);
    fir.set_bands(&STANDARD_TEST_CURVE);
    FFTConvolver::new(&fir.get_ir(CHANNELS), CHANNELS)
}

fn warm_apply(convolver: &mut FFTConvolver, input: &[f64], output: &mut [f64]) {
    for _ in 0..APPLY_WARMUP_BUFFERS {
        convolver.process_into(input, output);
    }
}

fn measure_apply(
    convolver: &mut FFTConvolver,
    input: &[f64],
    output: &mut [f64],
    iterations: usize,
    frames: usize,
) -> ApplyMeasurement {
    let start = Instant::now();
    for _ in 0..iterations {
        convolver.process_into(black_box(input), black_box(output));
        black_box(output[0]);
    }
    let ns_per_buffer = start.elapsed().as_nanos() as f64 / iterations as f64;
    ApplyMeasurement {
        ns_per_sample: ns_per_buffer / (frames * CHANNELS) as f64,
        ns_per_buffer,
    }
}

fn compare_with_baseline(
    candidate: &FirEqReport,
    baseline: &FirEqReport,
    threshold_pct: f64,
) -> Result<Vec<RegressionComparison>, String> {
    validate_performance_baseline(
        "FIR EQ",
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
            .map(|case| (case.case_key().to_string(), case.primary_median())),
        baseline
            .cases
            .iter()
            .map(|case| (case.case_key().to_string(), case.primary_median())),
        threshold_pct,
    )
}

fn enforce_report(report: &FirEqReport) -> Result<(), String> {
    let mut case_keys = BTreeSet::new();
    let invalid_cases = report
        .cases
        .iter()
        .filter(|case| {
            !case_keys.insert(case.case_key())
                || !case.has_valid_work()
                || !case.has_complete_trials(report.conditions.trials)
        })
        .map(FirEqCase::case_key)
        .collect::<Vec<_>>();
    let expected_case_count =
        report.conditions.tap_counts.len() * (report.conditions.phases.len() + 1);
    if !invalid_cases.is_empty() || report.cases.len() != expected_case_count {
        return Err(format!(
            "FIR EQ report validity gate failed: invalid cases [{}], expected {expected_case_count} cases, got {}",
            invalid_cases.join(", "),
            report.cases.len()
        ));
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "FIR EQ median regression gate failed",
        "ns/case-unit",
    ) {
        return Err(error);
    }
    Ok(())
}

fn print_report(report: &FirEqReport) -> Result<(), String> {
    println!(
        "audio_fir_eq_perf mode={} sample_rate={} channels={} process_frames={} coverage={} regeneration_iterations={} apply_iterations={} trials={}",
        report.mode.as_str(),
        report.conditions.sample_rate_hz,
        report.conditions.channels,
        report.conditions.process_frames,
        report.conditions.coverage,
        report.conditions.regeneration_iterations_per_trial,
        report.conditions.apply_iterations_per_trial,
        report.conditions.trials
    );
    println!(
        "audio_fir_eq_environment {}",
        environment_json(&report.environment)?
    );
    println!(
        "audio_fir_eq_note regeneration_includes=fft_design,phase_shaping apply_path=FirEq_ir->FFTConvolver excludes={}",
        report.conditions.excludes.join(",")
    );

    for case in &report.cases {
        match case {
            FirEqCase::Regeneration {
                case_key,
                phase,
                taps,
                ir_length,
                ns_per_regeneration,
                regenerations_per_ms_median,
                ..
            } => println!(
                "fir_eq_regeneration case={case_key} phase={phase} taps={taps} ir_length={ir_length} ns_per_regeneration_min={:.3} ns_per_regeneration_median={:.3} ns_per_regeneration_p95={:.3} ns_per_regeneration_max={:.3} regenerations_per_ms_median={regenerations_per_ms_median:.3}",
                ns_per_regeneration.min,
                ns_per_regeneration.median,
                ns_per_regeneration.p95,
                ns_per_regeneration.max
            ),
            FirEqCase::Apply {
                case_key,
                taps,
                strategy,
                fft_size,
                partition_size,
                frames,
                samples,
                ns_per_sample,
                ns_per_buffer,
                ..
            } => println!(
                "fir_eq_apply case={case_key} taps={taps} strategy={strategy} fft_size={fft_size} partition_size={} frames={frames} samples={samples} ns_per_sample_min={:.3} ns_per_sample_median={:.3} ns_per_sample_p95={:.3} ns_per_sample_max={:.3} ns_per_buffer_median={:.3} ns_per_buffer_p95={:.3}",
                partition_size.unwrap_or(0),
                ns_per_sample.min,
                ns_per_sample.median,
                ns_per_sample.p95,
                ns_per_sample.max,
                ns_per_buffer.median,
                ns_per_buffer.p95
            ),
        }
    }
    for comparison in &report.comparisons {
        println!(
            "fir_eq_baseline case={} baseline_median={:.3} candidate_median={:.3} regression_pct={:.3} threshold_pct={:.3} passed={}",
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

fn strategy_name(strategy: ConvolutionStrategy) -> &'static str {
    match strategy {
        ConvolutionStrategy::OverlapSave => "overlap_save",
        ConvolutionStrategy::Partitioned => "partitioned",
    }
}

const STANDARD_TEST_CURVE: [f64; 10] = [6.0, 4.0, 2.0, 0.0, -2.0, -3.0, -1.5, 1.0, 3.5, 5.0];
const ALT_TEST_CURVE: [f64; 10] = [-5.0, -3.0, 0.0, 2.5, 4.0, 3.0, 1.0, -1.0, -2.5, -4.0];
const WARM_CURVE: [f64; 10] = [1.0; 10];

fn synthetic_input(frames: usize, channels: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    let mut left_phase = 0.0_f64;
    let mut right_phase = 0.0_f64;

    for frame in 0..frames {
        let time = frame as f64 / SAMPLE_RATE;
        left_phase += std::f64::consts::TAU * (220.0 + 11.0 * (time * 3.0).sin()) / SAMPLE_RATE;
        right_phase += std::f64::consts::TAU * (330.0 + 7.0 * (time * 5.0).cos()) / SAMPLE_RATE;
        let envelope = 0.65 + 0.20 * (std::f64::consts::TAU * 1.7 * time).sin();
        let left = (left_phase.sin() * 0.55 + (left_phase * 3.0).sin() * 0.08) * envelope;
        let right = (right_phase.sin() * 0.50 - (right_phase * 2.0).cos() * 0.07) * envelope;

        out.push(left.clamp(-0.95, 0.95));
        if channels > 1 {
            out.push(right.clamp(-0.95, 0.95));
        }
        for channel in 2..channels {
            out.push((left * (1.0 - channel as f64 * 0.05)).clamp(-0.95, 0.95));
        }
    }

    out
}
