use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub mod support;

use support::{
    compare_case_medians, environment_json, generated_unix_ms, read_json, regression_gate_error,
    summarize_trials, validate_performance_baseline, write_json, BenchEnvironment, BenchMode,
    PerfArgs, PerformanceReportIdentity, RegressionComparison, TrialDistribution,
    REPORT_SCHEMA_VERSION,
};

use audio_engine_core::processor::{
    callback_stage_order_csv, AtomicCrossfeedParams, AtomicDynamicLoudnessParams,
    AtomicDynamicLoudnessTelemetry, AtomicEqParams, AtomicNoiseShaperParams,
    AtomicPeakLimiterParams, AtomicSaturationParams, AtomicVolumeParams, ConvolverControl,
    DspChain, FFTConvolver, NoiseShaperCurve, OutputChainBuilder, OutputChainParams, Saturation,
    SaturationQuality, SaturationQualityValue, SaturationType, SaturationTypeValue, EQ_BANDS,
};

const CHANNELS: usize = 2;
const SAMPLE_RATE: f64 = 48_000.0;
const BUFFER_FRAMES: [usize; 4] = [64, 128, 256, 512];
const WARMUP_BUFFERS: usize = 256;
const PRIMARY_ACTIVE_FRAMES: usize = 512;
const PRIMARY_MEDIAN_MAX_REGRESSION_PCT: f64 = 3.0;
const PRIMARY_P95_DEADLINE_MAX_REGRESSION_PCT: f64 = 5.0;

#[derive(Clone, Copy)]
enum Scenario {
    BypassDefault,
    ActiveDspNoConvolver,
    ActiveDspWithConvolver,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Self::BypassDefault => "bypass_default",
            Self::ActiveDspNoConvolver => "active_dsp_no_convolver",
            Self::ActiveDspWithConvolver => "active_dsp_with_convolver",
        }
    }

    fn all() -> &'static [Self] {
        &[
            Self::BypassDefault,
            Self::ActiveDspNoConvolver,
            Self::ActiveDspWithConvolver,
        ]
    }

    fn config_key(self) -> &'static str {
        match self {
            Self::BypassDefault => "bypass_defaults",
            Self::ActiveDspNoConvolver => "active_oversampled4x_no_convolver",
            Self::ActiveDspWithConvolver => "active_oversampled4x_ir256",
        }
    }

    fn config_description(self) -> &'static str {
        match self {
            Self::BypassDefault => {
                "optional stages disabled; volume unity; convolver slot empty"
            }
            Self::ActiveDspNoConvolver => {
                "EQ + Oversampled4x Tube saturation + Bauer low-pass crossfeed + dynamic loudness + true-peak limiter + 24-bit TPDF noise shaper; convolver slot empty"
            }
            Self::ActiveDspWithConvolver => {
                "active DSP configuration plus a stereo 256-tap synthetic convolver"
            }
        }
    }
}

struct ChainBundle {
    chain: DspChain,
}

struct TrialMeasurement {
    ns_per_sample: f64,
    ns_per_buffer: f64,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct CallbackConditions {
    sample_rate_hz: u32,
    channels: usize,
    nodes: String,
    copy_input: bool,
    coverage: String,
    excludes: Vec<String>,
    warmup_buffers: usize,
    iterations_per_trial: usize,
    trials: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkValidation {
    valid: bool,
    all_samples_finite: bool,
    output_changed: bool,
    expected_output_changed: bool,
    bypassed: bool,
    consumed_frames: usize,
    produced_frames: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct CallbackCase {
    case_key: String,
    scenario: String,
    scenario_config: String,
    frames: usize,
    samples: usize,
    ns_per_sample: TrialDistribution,
    ns_per_buffer: TrialDistribution,
    buffer_duration_ns: f64,
    median_deadline_utilization_pct: f64,
    p95_deadline_utilization_pct: f64,
    work_validation: WorkValidation,
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
struct AcceptanceComparison {
    case_key: String,
    metric: String,
    baseline_value: f64,
    candidate_value: f64,
    regression_pct: f64,
    threshold_pct: f64,
    strict_improvement: bool,
    passed: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct CallbackReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: CallbackConditions,
    cases: Vec<CallbackCase>,
    baseline: Option<BaselineReference>,
    comparisons: Vec<RegressionComparison>,
    #[serde(default)]
    acceptance_comparisons: Vec<AcceptanceComparison>,
}

fn main() -> Result<(), String> {
    let args = PerfArgs::parse(std::env::args().skip(1).collect())?;
    if args.help {
        print_help();
        return Ok(());
    }

    let (iterations, trials) = workload(args.mode);
    let node_order = callback_stage_order_csv();
    let environment = BenchEnvironment::capture();
    let conditions = CallbackConditions {
        sample_rate_hz: SAMPLE_RATE as u32,
        channels: CHANNELS,
        nodes: node_order,
        copy_input: true,
        coverage: "dsp_chain_plus_isolated_saturation".to_string(),
        excludes: [
            "cpal_device_write",
            "decoder",
            "resampler",
            "spectrum",
            "loudness_normalization_pre_gain",
            "gapless_state_machine",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        warmup_buffers: WARMUP_BUFFERS,
        iterations_per_trial: iterations,
        trials,
    };
    let mut cases = Vec::new();
    for &scenario in Scenario::all() {
        for &frames in &BUFFER_FRAMES {
            cases.push(benchmark_scenario(scenario, frames, iterations, trials)?);
        }
    }
    for &frames in &BUFFER_FRAMES {
        cases.push(benchmark_isolated_saturation(frames, iterations, trials)?);
    }

    let mut report = CallbackReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: "audio_callback_chain_perf".to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment,
        conditions,
        cases,
        baseline: None,
        comparisons: Vec::new(),
        acceptance_comparisons: Vec::new(),
    };

    if let Some(path) = &args.baseline {
        let baseline: CallbackReport = read_json(path, "callback baseline report")?;
        report.comparisons =
            compare_with_baseline(&report, &baseline, args.max_median_regression_pct)?;
        report.acceptance_comparisons = compare_acceptance_cases(&report, &baseline)?;
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
        write_json(path, &report, "callback performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }

    Ok(())
}

fn workload(mode: BenchMode) -> (usize, usize) {
    match mode {
        BenchMode::Quick => (1_000, 7),
        BenchMode::Full => (7_500, 9),
        BenchMode::Heavy => (30_000, 15),
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_callback_chain_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>]\n\
         \n\
         Reports trial min/median/p95/max and callback deadline utilization.\n\
         With a compatible baseline, 512-frame active median/p95 gates are fixed at 3%/5%.\n\
         Every isolated Saturation 4x median must strictly improve."
    );
}

fn print_report(report: &CallbackReport) -> Result<(), String> {
    println!(
        "audio_callback_chain_perf mode={} sample_rate={} channels={} nodes={} copy_input=true coverage={} iterations={} trials={}",
        report.mode.as_str(),
        report.conditions.sample_rate_hz,
        report.conditions.channels,
        report.conditions.nodes,
        report.conditions.coverage,
        report.conditions.iterations_per_trial,
        report.conditions.trials
    );
    println!(
        "audio_callback_chain_environment {}",
        environment_json(&report.environment)?
    );
    println!(
        "audio_callback_chain_note excludes={}",
        report.conditions.excludes.join(",")
    );

    for case in &report.cases {
        println!(
            "callback_chain case={} scenario={} frames={} samples={} ns_per_sample_min={:.3} ns_per_sample_median={:.3} ns_per_sample_p95={:.3} ns_per_sample_max={:.3} ns_per_buffer_median={:.3} ns_per_buffer_p95={:.3} median_deadline_utilization_pct={:.4} p95_deadline_utilization_pct={:.4}",
            case.case_key,
            case.scenario,
            case.frames,
            case.samples,
            case.ns_per_sample.min,
            case.ns_per_sample.median,
            case.ns_per_sample.p95,
            case.ns_per_sample.max,
            case.ns_per_buffer.median,
            case.ns_per_buffer.p95,
            case.median_deadline_utilization_pct,
            case.p95_deadline_utilization_pct
        );
    }
    for comparison in &report.comparisons {
        println!(
            "callback_chain_baseline case={} baseline_median={:.3} candidate_median={:.3} regression_pct={:.3} threshold_pct={:.3} passed={}",
            comparison.case_key,
            comparison.baseline_median,
            comparison.candidate_median,
            comparison.regression_pct,
            comparison.threshold_pct,
            comparison.passed
        );
    }
    for comparison in &report.acceptance_comparisons {
        println!(
            "callback_chain_acceptance case={} metric={} baseline={:.6} candidate={:.6} regression_pct={:.3} threshold_pct={:.3} strict_improvement={} passed={}",
            comparison.case_key,
            comparison.metric,
            comparison.baseline_value,
            comparison.candidate_value,
            comparison.regression_pct,
            comparison.threshold_pct,
            comparison.strict_improvement,
            comparison.passed,
        );
    }
    Ok(())
}

fn compare_with_baseline(
    candidate: &CallbackReport,
    baseline: &CallbackReport,
    threshold_pct: f64,
) -> Result<Vec<RegressionComparison>, String> {
    validate_performance_baseline(
        "callback",
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
            .map(|case| (case.case_key.clone(), case.ns_per_sample.median)),
        baseline
            .cases
            .iter()
            .map(|case| (case.case_key.clone(), case.ns_per_sample.median)),
        threshold_pct,
    )
}

fn compare_acceptance_cases(
    candidate: &CallbackReport,
    baseline: &CallbackReport,
) -> Result<Vec<AcceptanceComparison>, String> {
    fn compare(
        case_key: &str,
        metric: &str,
        baseline_value: f64,
        candidate_value: f64,
        threshold_pct: f64,
        strict_improvement: bool,
    ) -> Result<AcceptanceComparison, String> {
        if !baseline_value.is_finite()
            || baseline_value <= 0.0
            || !candidate_value.is_finite()
            || candidate_value <= 0.0
        {
            return Err(format!(
                "callback acceptance comparison '{case_key}' metric '{metric}' requires positive finite values; baseline={baseline_value}, candidate={candidate_value}"
            ));
        }
        let regression_pct = (candidate_value / baseline_value - 1.0) * 100.0;
        let passed = if strict_improvement {
            candidate_value < baseline_value
        } else {
            regression_pct <= threshold_pct
        };
        Ok(AcceptanceComparison {
            case_key: case_key.to_string(),
            metric: metric.to_string(),
            baseline_value,
            candidate_value,
            regression_pct,
            threshold_pct,
            strict_improvement,
            passed,
        })
    }

    let mut comparisons = Vec::new();
    let mut primary_active_cases = 0usize;
    let mut isolated_cases = 0usize;
    for candidate_case in &candidate.cases {
        let baseline_case = baseline
            .cases
            .iter()
            .find(|case| case.case_key == candidate_case.case_key)
            .ok_or_else(|| {
                format!(
                    "missing callback acceptance baseline case '{}'",
                    candidate_case.case_key
                )
            })?;

        if candidate_case.frames == PRIMARY_ACTIVE_FRAMES
            && matches!(
                candidate_case.scenario.as_str(),
                "active_dsp_no_convolver" | "active_dsp_with_convolver"
            )
        {
            primary_active_cases += 1;
            comparisons.push(compare(
                &candidate_case.case_key,
                "median_ns_per_sample",
                baseline_case.ns_per_sample.median,
                candidate_case.ns_per_sample.median,
                PRIMARY_MEDIAN_MAX_REGRESSION_PCT,
                false,
            )?);
            comparisons.push(compare(
                &candidate_case.case_key,
                "p95_deadline_utilization_pct",
                baseline_case.p95_deadline_utilization_pct,
                candidate_case.p95_deadline_utilization_pct,
                PRIMARY_P95_DEADLINE_MAX_REGRESSION_PCT,
                false,
            )?);
        }

        if candidate_case.scenario == "isolated_saturation_4x" {
            isolated_cases += 1;
            comparisons.push(compare(
                &candidate_case.case_key,
                "median_ns_per_sample",
                baseline_case.ns_per_sample.median,
                candidate_case.ns_per_sample.median,
                0.0,
                true,
            )?);
        }
    }

    if primary_active_cases != 2 || isolated_cases != BUFFER_FRAMES.len() {
        return Err(format!(
            "callback acceptance case matrix incomplete: primary_active_512={primary_active_cases}, isolated_saturation_4x={isolated_cases}"
        ));
    }
    Ok(comparisons)
}

fn enforce_report(report: &CallbackReport) -> Result<(), String> {
    let invalid_cases = report
        .cases
        .iter()
        .filter(|case| {
            !case.work_validation.valid
                || case.ns_per_sample.samples.len() != report.conditions.trials
                || case.ns_per_buffer.samples.len() != report.conditions.trials
        })
        .map(|case| case.case_key.as_str())
        .collect::<Vec<_>>();
    if !invalid_cases.is_empty() {
        return Err(format!(
            "callback report validity gate failed for cases: {}",
            invalid_cases.join(", ")
        ));
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "callback median regression gate failed",
        "ns/sample",
    ) {
        return Err(error);
    }
    let acceptance_failures = report
        .acceptance_comparisons
        .iter()
        .filter(|comparison| !comparison.passed)
        .map(|comparison| {
            format!(
                "{} {}: baseline {:.6}, candidate {:.6}, regression {:.3}%, threshold {:.3}%, strict_improvement={}",
                comparison.case_key,
                comparison.metric,
                comparison.baseline_value,
                comparison.candidate_value,
                comparison.regression_pct,
                comparison.threshold_pct,
                comparison.strict_improvement,
            )
        })
        .collect::<Vec<_>>();
    if !acceptance_failures.is_empty() {
        return Err(format!(
            "callback task acceptance gate failed: {}",
            acceptance_failures.join("; ")
        ));
    }
    Ok(())
}

fn benchmark_scenario(
    scenario: Scenario,
    frames: usize,
    iterations: usize,
    trials: usize,
) -> Result<CallbackCase, String> {
    let corpus = synthetic_buffer(frames, CHANNELS);
    let work_validation = validate_work(scenario, frames, &corpus);
    let mut ns_per_sample = Vec::with_capacity(trials);
    let mut ns_per_buffer = Vec::with_capacity(trials);

    for _ in 0..trials {
        let mut bundle = build_chain_bundle(scenario);
        warm_chain(&mut bundle, scenario, &corpus);
        let measurement = measure_chain(&mut bundle.chain, &corpus, frames, iterations);
        ns_per_sample.push(measurement.ns_per_sample);
        ns_per_buffer.push(measurement.ns_per_buffer);
    }

    let ns_per_sample = summarize_trials(ns_per_sample)?;
    let ns_per_buffer = summarize_trials(ns_per_buffer)?;
    let buffer_duration_ns = frames as f64 / SAMPLE_RATE * 1.0e9;
    Ok(CallbackCase {
        case_key: format!(
            "scenario={};frames={};config={}",
            scenario.name(),
            frames,
            scenario.config_key()
        ),
        scenario: scenario.name().to_string(),
        scenario_config: scenario.config_description().to_string(),
        frames,
        samples: frames * CHANNELS,
        median_deadline_utilization_pct: ns_per_buffer.median / buffer_duration_ns * 100.0,
        p95_deadline_utilization_pct: ns_per_buffer.p95 / buffer_duration_ns * 100.0,
        ns_per_sample,
        ns_per_buffer,
        buffer_duration_ns,
        work_validation,
    })
}

fn benchmark_isolated_saturation(
    frames: usize,
    iterations: usize,
    trials: usize,
) -> Result<CallbackCase, String> {
    let corpus = synthetic_buffer(frames, CHANNELS);
    let work_validation = validate_isolated_saturation_work(frames, &corpus);
    let mut ns_per_sample = Vec::with_capacity(trials);
    let mut ns_per_buffer = Vec::with_capacity(trials);

    for _ in 0..trials {
        let mut saturation = build_isolated_saturation();
        warm_isolated_saturation(&mut saturation, &corpus);
        let measurement = measure_isolated_saturation(&mut saturation, &corpus, frames, iterations);
        ns_per_sample.push(measurement.ns_per_sample);
        ns_per_buffer.push(measurement.ns_per_buffer);
    }

    let ns_per_sample = summarize_trials(ns_per_sample)?;
    let ns_per_buffer = summarize_trials(ns_per_buffer)?;
    let buffer_duration_ns = frames as f64 / SAMPLE_RATE * 1.0e9;
    Ok(CallbackCase {
        case_key: format!(
            "scenario=isolated_saturation_4x;frames={frames};config=isolated_tube_oversampled4x_fullband"
        ),
        scenario: "isolated_saturation_4x".to_string(),
        scenario_config:
            "direct Saturation; Tube; Oversampled4x; full-band nonlinear residual; driven input"
                .to_string(),
        frames,
        samples: frames * CHANNELS,
        median_deadline_utilization_pct: ns_per_buffer.median / buffer_duration_ns * 100.0,
        p95_deadline_utilization_pct: ns_per_buffer.p95 / buffer_duration_ns * 100.0,
        ns_per_sample,
        ns_per_buffer,
        buffer_duration_ns,
        work_validation,
    })
}

fn validate_work(scenario: Scenario, frames: usize, corpus: &[f64]) -> WorkValidation {
    let mut bundle = build_chain_bundle(scenario);
    warm_chain(&mut bundle, scenario, corpus);
    let mut scratch = corpus.to_vec();
    let progress = bundle
        .chain
        .process(&mut scratch, CHANNELS)
        .expect("benchmark callback validation must succeed");
    let all_samples_finite = scratch.iter().all(|sample| sample.is_finite());
    let output_changed = scratch
        .iter()
        .zip(corpus)
        .any(|(output, input)| output.to_bits() != input.to_bits());
    let expected_output_changed = !matches!(scenario, Scenario::BypassDefault);
    let bypassed = progress.is_bypassed();
    let consumed_frames = progress.consumed_frames();
    let produced_frames = progress.produced_frames();
    WorkValidation {
        valid: all_samples_finite
            && output_changed == expected_output_changed
            && consumed_frames == frames
            && produced_frames == frames,
        all_samples_finite,
        output_changed,
        expected_output_changed,
        bypassed,
        consumed_frames,
        produced_frames,
    }
}

fn validate_isolated_saturation_work(frames: usize, corpus: &[f64]) -> WorkValidation {
    let mut saturation = build_isolated_saturation();
    warm_isolated_saturation(&mut saturation, corpus);
    let mut scratch = corpus.to_vec();
    saturation.process_with_channels(&mut scratch, CHANNELS);
    let all_samples_finite = scratch.iter().all(|sample| sample.is_finite());
    let output_changed = scratch
        .iter()
        .zip(corpus)
        .any(|(output, input)| output.to_bits() != input.to_bits());
    WorkValidation {
        valid: all_samples_finite && output_changed,
        all_samples_finite,
        output_changed,
        expected_output_changed: true,
        bypassed: false,
        consumed_frames: frames,
        produced_frames: frames,
    }
}

// Rebuilds the callback-safe output chain from the crate's canonical builder.
// The timings here track realtime DSP cost and exclude decoder, upstream
// playback resampling, spectrum packing, and the OS device write.
fn build_chain_bundle(scenario: Scenario) -> ChainBundle {
    let eq_params = Arc::new(AtomicEqParams::new());
    let saturation_params = Arc::new(AtomicSaturationParams::new());
    let crossfeed_params = Arc::new(AtomicCrossfeedParams::new());
    let limiter_params = Arc::new(AtomicPeakLimiterParams::new());
    let volume_params = Arc::new(AtomicVolumeParams::new());
    let noise_shaper_params = Arc::new(AtomicNoiseShaperParams::new());
    let dynamic_loudness_params = Arc::new(AtomicDynamicLoudnessParams::new());
    let dynamic_loudness_telemetry = Arc::new(AtomicDynamicLoudnessTelemetry::new());

    configure_params(
        scenario,
        &eq_params,
        &saturation_params,
        &crossfeed_params,
        &limiter_params,
        &volume_params,
        &noise_shaper_params,
        &dynamic_loudness_params,
    );

    let convolver_control = ConvolverControl::default();

    if matches!(scenario, Scenario::ActiveDspWithConvolver) {
        convolver_control.set_enabled(true);
        convolver_control
            .publish_at_rate(
                FFTConvolver::new(&synthetic_ir(256, CHANNELS), CHANNELS)
                    .expect("benchmark IR geometry is valid"),
                SAMPLE_RATE as u32,
            )
            .expect("benchmark sample rate is valid");
    }

    let chain = OutputChainBuilder::new(OutputChainParams {
        channels: CHANNELS,
        source_sample_rate: SAMPLE_RATE as u32,
        output_sample_rate: SAMPLE_RATE as u32,
        eq_params,
        saturation_params,
        crossfeed_params,
        convolver_control,
        volume_params,
        dynamic_loudness_params,
        dynamic_loudness_telemetry,
        limiter_params,
        noise_shaper_params,
    })
    .build_callback_chain()
    .expect("benchmark output-chain configuration must be valid");

    ChainBundle { chain }
}

#[allow(clippy::too_many_arguments)]
fn configure_params(
    scenario: Scenario,
    eq_params: &AtomicEqParams,
    saturation_params: &AtomicSaturationParams,
    crossfeed_params: &AtomicCrossfeedParams,
    limiter_params: &AtomicPeakLimiterParams,
    volume_params: &AtomicVolumeParams,
    noise_shaper_params: &AtomicNoiseShaperParams,
    dynamic_loudness_params: &AtomicDynamicLoudnessParams,
) {
    match scenario {
        Scenario::BypassDefault => {
            eq_params.write(&[0.0; EQ_BANDS], false);
            saturation_params.set_enabled(false);
            saturation_params.set_armed(false);
            crossfeed_params.set_enabled(false);
            limiter_params.set_enabled(false);
            volume_params.set_volume(1.0);
            volume_params.set_muted(false);
            noise_shaper_params.set_enabled(false);
            dynamic_loudness_params.set_enabled(false);
        }
        Scenario::ActiveDspNoConvolver => {
            eq_params.write(
                &[1.5, -0.75, 0.5, 0.0, -1.0, 0.8, 0.0, 1.0, -0.4, 0.2],
                true,
            );
            saturation_params.set_enabled(true);
            saturation_params.set_armed(true);
            saturation_params.set_drive(0.85);
            saturation_params.set_threshold(0.82);
            saturation_params.set_mix(0.35);
            saturation_params.set_sat_type(SaturationTypeValue::Tube);
            saturation_params.set_quality(SaturationQualityValue::Oversampled4x);
            saturation_params.set_highpass_mode(true);
            saturation_params.set_highpass_cutoff(4_000.0);
            crossfeed_params.set_enabled(true);
            crossfeed_params.set_mix(0.30);
            crossfeed_params.set_cutoff(700.0);
            limiter_params.set_enabled(true);
            limiter_params.set_threshold(-1.0);
            limiter_params.set_release(120.0);
            volume_params.set_volume(0.72);
            volume_params.set_muted(false);
            noise_shaper_params.set_enabled(true);
            noise_shaper_params.set_bits(24);
            noise_shaper_params.set_curve(NoiseShaperCurve::TpdfOnly);
            dynamic_loudness_params.set_enabled(true);
            dynamic_loudness_params.set_volume(0.72);
            dynamic_loudness_params.set_strength(0.65);
        }
        Scenario::ActiveDspWithConvolver => {
            configure_params(
                Scenario::ActiveDspNoConvolver,
                eq_params,
                saturation_params,
                crossfeed_params,
                limiter_params,
                volume_params,
                noise_shaper_params,
                dynamic_loudness_params,
            );
        }
    }
}

fn warm_chain(bundle: &mut ChainBundle, _scenario: Scenario, corpus: &[f64]) {
    let mut scratch = corpus.to_vec();

    for _ in 0..WARMUP_BUFFERS {
        scratch.copy_from_slice(corpus);
        let _ = bundle
            .chain
            .process(black_box(&mut scratch), CHANNELS)
            .expect("benchmark callback processing must succeed");
    }
}

fn build_isolated_saturation() -> Saturation {
    let mut saturation = Saturation::new();
    saturation.set_sample_rate(SAMPLE_RATE);
    saturation.set_channel_count(CHANNELS);
    saturation.set_type(SaturationType::Tube);
    saturation.set_quality(SaturationQuality::Oversampled4x);
    saturation.set_drive(0.92);
    saturation.set_threshold(0.30);
    saturation.set_mix(0.75);
    saturation.set_input_gain(0.0);
    saturation.set_output_gain(0.0);
    saturation.set_highpass_mode(false);
    saturation.set_enabled(true);
    saturation
}

fn warm_isolated_saturation(saturation: &mut Saturation, corpus: &[f64]) {
    let mut scratch = corpus.to_vec();

    for _ in 0..WARMUP_BUFFERS {
        scratch.copy_from_slice(corpus);
        saturation.process_with_channels(black_box(&mut scratch), CHANNELS);
    }
}

fn measure_chain(
    chain: &mut DspChain,
    corpus: &[f64],
    frames: usize,
    iterations: usize,
) -> TrialMeasurement {
    let mut scratch = vec![0.0; corpus.len()];
    let start = Instant::now();

    for _ in 0..iterations {
        scratch.copy_from_slice(black_box(corpus));
        let _ = chain
            .process(black_box(&mut scratch), CHANNELS)
            .expect("benchmark callback processing must succeed");
        black_box(&scratch);
    }

    let elapsed = start.elapsed();
    let ns_per_buffer = elapsed.as_nanos() as f64 / iterations as f64;
    let ns_per_sample = ns_per_buffer / (frames * CHANNELS) as f64;

    TrialMeasurement {
        ns_per_sample,
        ns_per_buffer,
    }
}

fn measure_isolated_saturation(
    saturation: &mut Saturation,
    corpus: &[f64],
    frames: usize,
    iterations: usize,
) -> TrialMeasurement {
    let mut scratch = vec![0.0; corpus.len()];
    let start = Instant::now();

    for _ in 0..iterations {
        scratch.copy_from_slice(black_box(corpus));
        saturation.process_with_channels(black_box(&mut scratch), CHANNELS);
        black_box(&scratch);
    }

    let elapsed = start.elapsed();
    let ns_per_buffer = elapsed.as_nanos() as f64 / iterations as f64;
    let ns_per_sample = ns_per_buffer / (frames * CHANNELS) as f64;

    TrialMeasurement {
        ns_per_sample,
        ns_per_buffer,
    }
}

fn synthetic_buffer(frames: usize, channels: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    let mut left_phase = 0.0_f64;
    let mut right_phase = 0.0_f64;

    for frame in 0..frames {
        let t = frame as f64 / SAMPLE_RATE;
        left_phase += std::f64::consts::TAU * (220.0 + 11.0 * (t * 3.0).sin()) / SAMPLE_RATE;
        right_phase += std::f64::consts::TAU * (330.0 + 7.0 * (t * 5.0).cos()) / SAMPLE_RATE;
        let envelope = 0.65 + 0.20 * (std::f64::consts::TAU * 1.7 * t).sin();
        let transient = if frame % 127 == 0 { 0.28 } else { 0.0 };
        let left =
            (left_phase.sin() * 0.55 + (left_phase * 3.0).sin() * 0.08 + transient) * envelope;
        let right =
            (right_phase.sin() * 0.50 - (right_phase * 2.0).cos() * 0.07 - transient) * envelope;

        out.push(left.clamp(-0.95, 0.95));
        if channels > 1 {
            out.push(right.clamp(-0.95, 0.95));
        }
        for ch in 2..channels {
            out.push((left * (1.0 - ch as f64 * 0.05)).clamp(-0.95, 0.95));
        }
    }

    out
}

fn synthetic_ir(taps_per_channel: usize, channels: usize) -> Vec<f64> {
    let mut ir = Vec::with_capacity(taps_per_channel * channels);

    for tap in 0..taps_per_channel {
        let decay = (-(tap as f64) / 48.0).exp();
        for ch in 0..channels {
            let impulse = if tap == 0 { 0.72 } else { 0.0 };
            let early = if tap == 17 + ch * 3 { 0.12 } else { 0.0 };
            let tail = ((tap + ch * 11) as f64 * 0.37).sin() * 0.025 * decay;
            ir.push(impulse + early + tail);
        }
    }

    ir
}
