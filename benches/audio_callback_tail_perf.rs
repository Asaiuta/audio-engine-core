use std::hint::black_box;
use std::time::Instant;

use audio_engine_core::processor::callback_stage_order_csv;
use serde::{Deserialize, Serialize};

pub mod support;

use support::callback_fixture::{
    callback_case_key, synthetic_callback_buffer, validate_callback_work, CallbackChainFixture,
    CallbackScenario, CallbackWorkValidation, CALLBACK_BUFFER_FRAMES, CALLBACK_CHANNELS,
    CALLBACK_SAMPLE_RATE_HZ, CALLBACK_WARMUP_BUFFERS,
};
use support::{
    environment_json, generated_unix_ms, index_cases_by_key, parse_callback_tail_args,
    parse_pinned_probe_args, pin_current_thread, read_json, summarize_callback_samples,
    validate_performance_baseline, write_json, BenchEnvironment, BenchMode,
    CallbackTailDistribution, PerfArgs, PerformanceReportIdentity, PinnedSchedulingState,
    DEFAULT_PINNED_PROBE_CORE, REPORT_SCHEMA_VERSION,
};

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct CallbackTailConditions {
    sample_rate_hz: u32,
    channels: usize,
    nodes: String,
    scenarios: Vec<String>,
    callback_frames: Vec<usize>,
    callbacks_per_case: usize,
    warmup_buffers: usize,
    copy_input: bool,
    timer: String,
    timer_scope: String,
    raw_sample_unit: String,
    percentile_method: String,
    outlier_policy: String,
    timing_gate_scenarios: Vec<String>,
    report_only_scenarios: Vec<String>,
    coverage: String,
    excludes: Vec<String>,
    pinned: bool,
    requested_pin_core: Option<usize>,
    effective_scheduling: Option<PinnedSchedulingState>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct DistributionSummary {
    min: f64,
    median: f64,
    p95: f64,
    p99: f64,
    p99_9: f64,
    max: f64,
}

impl DistributionSummary {
    fn scaled(distribution: &CallbackTailDistribution, factor: f64) -> Self {
        Self {
            min: distribution.min * factor,
            median: distribution.median * factor,
            p95: distribution.p95 * factor,
            p99: distribution.p99 * factor,
            p99_9: distribution.p99_9 * factor,
            max: distribution.max * factor,
        }
    }

    fn has_positive_finite_values(&self) -> bool {
        [
            self.min,
            self.median,
            self.p95,
            self.p99,
            self.p99_9,
            self.max,
        ]
        .into_iter()
        .all(|value| value.is_finite() && value > 0.0)
    }
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct CallbackTailCase {
    case_key: String,
    scenario: String,
    scenario_config: String,
    frames: usize,
    samples: usize,
    callbacks: usize,
    callback_ns_per_buffer: CallbackTailDistribution,
    callback_ns_per_sample: DistributionSummary,
    buffer_duration_ns: f64,
    deadline_utilization_pct: DistributionSummary,
    missed_deadline_count: usize,
    missed_deadline_rate_pct: f64,
    work_validation: CallbackWorkValidation,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct BaselineReference {
    path: String,
    revision: String,
    dirty: Option<bool>,
    generated_unix_ms: u128,
    max_median_regression_pct: f64,
    max_p99_regression_pct: f64,
    max_p999_regression_pct: f64,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct TailRegressionComparison {
    case_key: String,
    metric: String,
    baseline_value: f64,
    candidate_value: f64,
    regression_pct: f64,
    threshold_pct: f64,
    passed: bool,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct CallbackTailReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: CallbackTailConditions,
    cases: Vec<CallbackTailCase>,
    baseline: Option<BaselineReference>,
    comparisons: Vec<TailRegressionComparison>,
}

fn main() -> Result<(), String> {
    let pin_args = parse_pinned_probe_args(std::env::args().skip(1).collect())?;
    let tail_args = parse_callback_tail_args(pin_args.remaining)?;
    let args = PerfArgs::parse(tail_args.remaining)?;
    if args.help {
        print_help();
        return Ok(());
    }
    if args.baseline.is_some() && !pin_args.enabled {
        return Err(
            "--baseline requires --pinned for callback-tail timing comparisons".to_string(),
        );
    }

    let effective_scheduling = if pin_args.enabled {
        Some(pin_current_thread(pin_args.core)?)
    } else {
        None
    };
    let callbacks_per_case = workload(args.mode);
    let conditions = CallbackTailConditions {
        sample_rate_hz: CALLBACK_SAMPLE_RATE_HZ,
        channels: CALLBACK_CHANNELS,
        nodes: callback_stage_order_csv(),
        scenarios: CallbackScenario::ALL
            .iter()
            .map(|scenario| format!("{}:{}", scenario.name(), scenario.config_key()))
            .collect(),
        callback_frames: CALLBACK_BUFFER_FRAMES.to_vec(),
        callbacks_per_case,
        warmup_buffers: CALLBACK_WARMUP_BUFFERS,
        copy_input: true,
        timer: "std::time::Instant".to_string(),
        timer_scope: "input copy + DspChain::process + output black_box; one interval per callback"
            .to_string(),
        raw_sample_unit: "nanoseconds_per_callback_buffer".to_string(),
        percentile_method: "even median midpoint; nearest-rank p95/p99/p99.9".to_string(),
        outlier_policy: "retain every callback sample; no trimming or timer subtraction"
            .to_string(),
        timing_gate_scenarios: [
            CallbackScenario::ActiveDspNoConvolver,
            CallbackScenario::ActiveDspWithConvolver,
        ]
        .into_iter()
        .map(|scenario| scenario.name().to_string())
        .collect(),
        report_only_scenarios: [CallbackScenario::BypassDefault]
            .into_iter()
            .map(|scenario| scenario.name().to_string())
            .collect(),
        coverage: "full_callback_chain_per_callback_tail".to_string(),
        excludes: [
            "cpal_device_callback",
            "wasapi_device_write",
            "decoder",
            "resampler",
            "spectrum",
            "loudness_normalization_pre_gain",
            "gapless_state_machine",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        pinned: pin_args.enabled,
        requested_pin_core: pin_args.enabled.then_some(pin_args.core),
        effective_scheduling,
    };

    let mut cases = Vec::with_capacity(CallbackScenario::ALL.len() * CALLBACK_BUFFER_FRAMES.len());
    for scenario in CallbackScenario::ALL {
        for frames in CALLBACK_BUFFER_FRAMES {
            cases.push(benchmark_case(scenario, frames, callbacks_per_case)?);
        }
    }

    let mut report = CallbackTailReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: "audio_callback_tail_perf".to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment: BenchEnvironment::capture(),
        conditions,
        cases,
        baseline: None,
        comparisons: Vec::new(),
    };

    if let Some(path) = &args.baseline {
        let baseline: CallbackTailReport = read_json(path, "callback-tail baseline report")?;
        report.comparisons = compare_with_baseline(
            &report,
            &baseline,
            args.max_median_regression_pct,
            tail_args.max_p99_regression_pct,
            tail_args.max_p999_regression_pct,
        )?;
        report.baseline = Some(BaselineReference {
            path: path.display().to_string(),
            revision: baseline.environment.revision,
            dirty: baseline.environment.dirty,
            generated_unix_ms: baseline.generated_unix_ms,
            max_median_regression_pct: args.max_median_regression_pct,
            max_p99_regression_pct: tail_args.max_p99_regression_pct,
            max_p999_regression_pct: tail_args.max_p999_regression_pct,
        });
    }

    print_report(&report)?;
    if let Some(path) = &args.out {
        write_json(path, &report, "callback-tail performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }
    Ok(())
}

fn workload(mode: BenchMode) -> usize {
    match mode {
        BenchMode::Quick => 4_000,
        BenchMode::Full => 20_000,
        BenchMode::Heavy => 100_000,
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_callback_tail_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>] [--max-p99-regression-pct <pct>] [--max-p999-regression-pct <pct>] [--pinned] [--pin-core <n>]\n\
         \n\
         Retains one raw duration per full-chain callback and reports median/p95/p99/p99.9/max, deadline utilization, and missed deadlines.\n\
         --enforce without a baseline validates work and report integrity only.\n\
         Bypass tails remain report-only because sub-microsecond samples are clock-quantized.\n\
         Strict timing comparison requires a compatible Windows --pinned baseline/candidate pair.\n\
         Default pinned core: {DEFAULT_PINNED_PROBE_CORE}."
    );
}

fn benchmark_case(
    scenario: CallbackScenario,
    frames: usize,
    callbacks: usize,
) -> Result<CallbackTailCase, String> {
    let corpus = synthetic_callback_buffer(frames);
    let work_validation = validate_callback_work(scenario, frames, &corpus)?;
    let mut fixture = CallbackChainFixture::build(scenario)?;
    fixture.warm(&corpus)?;
    let mut scratch = vec![0.0; corpus.len()];
    let mut raw_samples = Vec::with_capacity(callbacks);

    for callback_index in 0..callbacks {
        let started = Instant::now();
        scratch.copy_from_slice(black_box(&corpus));
        let progress = fixture
            .chain_mut()
            .process(black_box(&mut scratch), CALLBACK_CHANNELS)
            .map_err(|error| {
                format!(
                    "callback-tail processing failed for scenario '{}' frame size {frames} at callback {callback_index}: {error}",
                    scenario.name()
                )
            })?;
        black_box(&scratch);
        // Clamp to one nanosecond: on Windows the timer tick is ~100 ns, so a
        // bypass callback can legitimately measure zero elapsed time. Zero is
        // not representable in the positive-sample distribution contract; one
        // nanosecond is the honest minimal representable duration and remains
        // report-only for bypass scenarios.
        let elapsed_ns = started.elapsed().as_nanos().max(1) as f64;
        if progress.consumed_frames() != frames || progress.produced_frames() != frames {
            return Err(format!(
                "callback-tail progress mismatch for scenario '{}' frame size {frames} at callback {callback_index}: consumed {}, produced {}",
                scenario.name(),
                progress.consumed_frames(),
                progress.produced_frames()
            ));
        }
        raw_samples.push(elapsed_ns);
    }

    let callback_ns_per_buffer = summarize_callback_samples(raw_samples)?;
    let samples = frames * CALLBACK_CHANNELS;
    let callback_ns_per_sample =
        DistributionSummary::scaled(&callback_ns_per_buffer, 1.0 / samples as f64);
    let buffer_duration_ns = frames as f64 / CALLBACK_SAMPLE_RATE_HZ as f64 * 1.0e9;
    let deadline_utilization_pct =
        DistributionSummary::scaled(&callback_ns_per_buffer, 100.0 / buffer_duration_ns);
    let missed_deadline_count = callback_ns_per_buffer
        .samples
        .iter()
        .filter(|sample| **sample > buffer_duration_ns)
        .count();
    let missed_deadline_rate_pct = missed_deadline_count as f64 / callbacks as f64 * 100.0;

    Ok(CallbackTailCase {
        case_key: callback_case_key(scenario, frames),
        scenario: scenario.name().to_string(),
        scenario_config: scenario.config_description().to_string(),
        frames,
        samples,
        callbacks,
        callback_ns_per_buffer,
        callback_ns_per_sample,
        buffer_duration_ns,
        deadline_utilization_pct,
        missed_deadline_count,
        missed_deadline_rate_pct,
        work_validation,
    })
}

fn compare_with_baseline(
    candidate: &CallbackTailReport,
    baseline: &CallbackTailReport,
    median_threshold_pct: f64,
    p99_threshold_pct: f64,
    p999_threshold_pct: f64,
) -> Result<Vec<TailRegressionComparison>, String> {
    validate_performance_baseline(
        "callback-tail",
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
    if !candidate.conditions.pinned || candidate.conditions.effective_scheduling.is_none() {
        return Err(
            "callback-tail timing comparisons require an effective pinned scheduling state"
                .to_string(),
        );
    }

    let candidate_cases = index_cases_by_key(
        &candidate.cases,
        |case| case.case_key.as_str(),
        "candidate callback-tail",
    )?;
    let baseline_cases = index_cases_by_key(
        &baseline.cases,
        |case| case.case_key.as_str(),
        "baseline callback-tail",
    )?;
    let candidate_keys = candidate_cases.keys().copied().collect::<Vec<_>>();
    let baseline_keys = baseline_cases.keys().copied().collect::<Vec<_>>();
    if candidate_keys != baseline_keys {
        let missing_in_candidate = baseline_keys
            .iter()
            .filter(|key| !candidate_cases.contains_key(*key))
            .copied()
            .collect::<Vec<_>>();
        let missing_in_baseline = candidate_keys
            .iter()
            .filter(|key| !baseline_cases.contains_key(*key))
            .copied()
            .collect::<Vec<_>>();
        return Err(format!(
            "callback-tail case sets differ: missing in candidate {missing_in_candidate:?}; missing in baseline {missing_in_baseline:?}"
        ));
    }

    let mut comparisons = Vec::with_capacity(candidate_cases.len() * 3);
    for (case_key, candidate_case) in candidate_cases {
        if !candidate
            .conditions
            .timing_gate_scenarios
            .contains(&candidate_case.scenario)
        {
            continue;
        }
        let baseline_case = baseline_cases[case_key];
        comparisons.push(compare_metric(
            case_key,
            "median_ns_per_buffer",
            candidate_case.callback_ns_per_buffer.median,
            baseline_case.callback_ns_per_buffer.median,
            median_threshold_pct,
        )?);
        comparisons.push(compare_metric(
            case_key,
            "p99_ns_per_buffer",
            candidate_case.callback_ns_per_buffer.p99,
            baseline_case.callback_ns_per_buffer.p99,
            p99_threshold_pct,
        )?);
        comparisons.push(compare_metric(
            case_key,
            "p99_9_ns_per_buffer",
            candidate_case.callback_ns_per_buffer.p99_9,
            baseline_case.callback_ns_per_buffer.p99_9,
            p999_threshold_pct,
        )?);
    }
    Ok(comparisons)
}

fn compare_metric(
    case_key: &str,
    metric: &str,
    candidate_value: f64,
    baseline_value: f64,
    threshold_pct: f64,
) -> Result<TailRegressionComparison, String> {
    if !threshold_pct.is_finite() || threshold_pct < 0.0 {
        return Err(format!(
            "callback-tail {metric} regression threshold must be finite and non-negative, got {threshold_pct}"
        ));
    }
    if !candidate_value.is_finite()
        || candidate_value <= 0.0
        || !baseline_value.is_finite()
        || baseline_value <= 0.0
    {
        return Err(format!(
            "callback-tail comparison '{case_key}' metric '{metric}' requires positive finite values; baseline={baseline_value}, candidate={candidate_value}"
        ));
    }
    let regression_pct = (candidate_value / baseline_value - 1.0) * 100.0;
    Ok(TailRegressionComparison {
        case_key: case_key.to_string(),
        metric: metric.to_string(),
        baseline_value,
        candidate_value,
        regression_pct,
        threshold_pct,
        passed: regression_pct <= threshold_pct + 1.0e-12,
    })
}

fn enforce_report(report: &CallbackTailReport) -> Result<(), String> {
    validate_scenario_classification(report)?;
    let indexed = index_cases_by_key(
        &report.cases,
        |case| case.case_key.as_str(),
        "candidate callback-tail",
    )?;
    let invalid = indexed
        .values()
        .filter_map(|case| {
            validate_case(report, case)
                .err()
                .map(|error| (case.case_key.as_str(), error))
        })
        .collect::<Vec<_>>();
    if !invalid.is_empty() {
        return Err(format!(
            "callback-tail report validity gate failed: {}",
            invalid
                .iter()
                .map(|(case_key, error)| format!("{case_key}: {error}"))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }

    if report.baseline.is_some() {
        let expected = report
            .cases
            .iter()
            .filter(|case| {
                report
                    .conditions
                    .timing_gate_scenarios
                    .contains(&case.scenario)
            })
            .count()
            * 3;
        if report.comparisons.len() != expected {
            return Err(format!(
                "callback-tail baseline report requires {expected} comparisons, found {}",
                report.comparisons.len()
            ));
        }
    } else if !report.comparisons.is_empty() {
        return Err("callback-tail report has comparisons without a baseline".to_string());
    }

    let failures = report
        .comparisons
        .iter()
        .filter(|comparison| !comparison.passed)
        .map(|comparison| {
            format!(
                "{} {}: baseline {:.3} ns, candidate {:.3} ns, regression {:.3}% > threshold {:.3}%",
                comparison.case_key,
                comparison.metric,
                comparison.baseline_value,
                comparison.candidate_value,
                comparison.regression_pct,
                comparison.threshold_pct
            )
        })
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        return Err(format!(
            "callback-tail timing regression gate failed: {}",
            failures.join("; ")
        ));
    }

    let json = serde_json::to_vec(report)
        .map_err(|error| format!("failed to serialize callback-tail report: {error}"))?;
    let decoded: CallbackTailReport = serde_json::from_slice(&json)
        .map_err(|error| format!("failed to deserialize callback-tail report: {error}"))?;
    if decoded.schema_version != report.schema_version
        || decoded.probe != report.probe
        || decoded.mode != report.mode
        || decoded.environment != report.environment
        || decoded.conditions != report.conditions
        || decoded.cases.len() != report.cases.len()
        || decoded.baseline != report.baseline
        || decoded.comparisons.len() != report.comparisons.len()
    {
        return Err("callback-tail report identity changed during JSON round trip".to_string());
    }
    let decoded_cases = index_cases_by_key(
        &decoded.cases,
        |case| case.case_key.as_str(),
        "round-trip callback-tail",
    )?;
    for case in decoded_cases.values() {
        validate_case(&decoded, case)?;
    }
    Ok(())
}

fn validate_scenario_classification(report: &CallbackTailReport) -> Result<(), String> {
    for case in &report.cases {
        let timing_gate = report
            .conditions
            .timing_gate_scenarios
            .contains(&case.scenario);
        let report_only = report
            .conditions
            .report_only_scenarios
            .contains(&case.scenario);
        if timing_gate == report_only {
            return Err(format!(
                "callback-tail scenario '{}' must be classified exactly once as timing-gated or report-only",
                case.scenario
            ));
        }
    }
    Ok(())
}

fn validate_case(report: &CallbackTailReport, case: &CallbackTailCase) -> Result<(), String> {
    if !case.work_validation.valid {
        return Err("DSP work validation failed".to_string());
    }
    if case.callbacks != report.conditions.callbacks_per_case
        || case.callback_ns_per_buffer.samples.len() != report.conditions.callbacks_per_case
    {
        return Err(format!(
            "raw sample count differs: declared {}, case {}, retained {}",
            report.conditions.callbacks_per_case,
            case.callbacks,
            case.callback_ns_per_buffer.samples.len()
        ));
    }
    let recomputed = summarize_callback_samples(case.callback_ns_per_buffer.samples.clone())?;
    if recomputed != case.callback_ns_per_buffer {
        return Err("callback distribution does not match retained raw samples".to_string());
    }
    if !case.callback_ns_per_sample.has_positive_finite_values()
        || !case.deadline_utilization_pct.has_positive_finite_values()
        || !case.buffer_duration_ns.is_finite()
        || case.buffer_duration_ns <= 0.0
    {
        return Err("derived timing values must be finite and positive".to_string());
    }
    let missed = case
        .callback_ns_per_buffer
        .samples
        .iter()
        .filter(|sample| **sample > case.buffer_duration_ns)
        .count();
    let missed_rate = missed as f64 / case.callbacks as f64 * 100.0;
    if missed != case.missed_deadline_count
        || (missed_rate - case.missed_deadline_rate_pct).abs() > 1.0e-12
    {
        return Err(format!(
            "missed-deadline summary differs: expected {missed}/{missed_rate:.12}%, report {}/{:.12}%",
            case.missed_deadline_count, case.missed_deadline_rate_pct
        ));
    }
    Ok(())
}

fn print_report(report: &CallbackTailReport) -> Result<(), String> {
    println!(
        "audio_callback_tail_perf mode={} callbacks_per_case={} cases={} pinned={} pin_core={:?}",
        report.mode.as_str(),
        report.conditions.callbacks_per_case,
        report.cases.len(),
        report.conditions.pinned,
        report.conditions.requested_pin_core
    );
    println!(
        "audio_callback_tail_environment {}",
        environment_json(&report.environment)?
    );
    if let Some(scheduling) = &report.conditions.effective_scheduling {
        println!(
            "audio_callback_tail_scheduling {}",
            serde_json::to_string(scheduling)
                .map_err(|error| format!("failed to serialize scheduling state: {error}"))?
        );
    }
    for case in &report.cases {
        println!(
            "callback_tail case={} callbacks={} median_ns={:.3} p95_ns={:.3} p99_ns={:.3} p99_9_ns={:.3} max_ns={:.3} median_util_pct={:.6} p99_util_pct={:.6} p99_9_util_pct={:.6} max_util_pct={:.6} missed={}/{} ({:.6}%) valid={}",
            case.case_key,
            case.callbacks,
            case.callback_ns_per_buffer.median,
            case.callback_ns_per_buffer.p95,
            case.callback_ns_per_buffer.p99,
            case.callback_ns_per_buffer.p99_9,
            case.callback_ns_per_buffer.max,
            case.deadline_utilization_pct.median,
            case.deadline_utilization_pct.p99,
            case.deadline_utilization_pct.p99_9,
            case.deadline_utilization_pct.max,
            case.missed_deadline_count,
            case.callbacks,
            case.missed_deadline_rate_pct,
            case.work_validation.valid
        );
    }
    for comparison in &report.comparisons {
        println!(
            "callback_tail_baseline case={} metric={} baseline={:.3} candidate={:.3} regression_pct={:.3} threshold_pct={:.3} passed={}",
            comparison.case_key,
            comparison.metric,
            comparison.baseline_value,
            comparison.candidate_value,
            comparison.regression_pct,
            comparison.threshold_pct,
            comparison.passed
        );
    }
    Ok(())
}
