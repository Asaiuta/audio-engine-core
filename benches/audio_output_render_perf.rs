use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub mod support;

use support::allocation::AllocationScope;
use support::{
    compare_case_medians, environment_json, generated_unix_ms, read_json, regression_gate_error,
    summarize_trials, validate_performance_baseline, write_json, BenchEnvironment, BenchMode,
    PerfArgs, PerformanceReportIdentity, RegressionComparison, TrialDistribution,
    REPORT_SCHEMA_VERSION,
};

use audio_engine_core::processor::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry,
    AtomicEqParams, AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams,
    AtomicVolumeParams, ConvolverControl, FFTConvolver, NoiseShaperCurve, OfflineRenderPolicy,
    OutputChainBuilder, OutputChainParams, OutputRenderChain, SaturationQualityValue,
    SaturationTypeValue, StreamingResampler, EQ_BANDS, RESAMPLER_BACKEND_NAME,
};

const CHANNELS: usize = 2;
const EQUAL_RATE_HZ: u32 = 48_000;
const RESAMPLED_SOURCE_RATE_HZ: u32 = 44_100;
const OUTPUT_RATE_HZ: u32 = 48_000;
const RENDER_BLOCK_FRAMES: usize = 4_096;
const SHORT_RENDER_BLOCK_FRAMES: usize = 64;
const WARMUP_FRAMES: usize = 4_096;
const ACTIVE_EQ_GAINS_DB: [f64; EQ_BANDS] = [2.5, -1.5, 1.0, -0.5, 1.5, -2.0, 1.0, 0.5, -1.0, 1.5];

#[derive(Clone, Copy)]
enum Scenario {
    TransparentEqualRate,
    ActiveIirEqualRate,
    ActiveSaturation4xEqualRate,
    ConvolverTailEqualRate,
    ActiveEqualRate,
    ActiveResampled,
}

impl Scenario {
    fn all() -> &'static [Self] {
        &[
            Self::TransparentEqualRate,
            Self::ActiveIirEqualRate,
            Self::ActiveSaturation4xEqualRate,
            Self::ConvolverTailEqualRate,
            Self::ActiveEqualRate,
            Self::ActiveResampled,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            Self::TransparentEqualRate => "transparent_equal_rate",
            Self::ActiveIirEqualRate => "active_iir_equal_rate",
            Self::ActiveSaturation4xEqualRate => "active_saturation_4x_equal_rate",
            Self::ConvolverTailEqualRate => "convolver_tail_equal_rate",
            Self::ActiveEqualRate => "active_equal_rate",
            Self::ActiveResampled => "active_44k1_to_48k",
        }
    }

    fn source_rate_hz(self) -> u32 {
        match self {
            Self::ActiveResampled => RESAMPLED_SOURCE_RATE_HZ,
            Self::TransparentEqualRate
            | Self::ActiveIirEqualRate
            | Self::ActiveSaturation4xEqualRate
            | Self::ConvolverTailEqualRate
            | Self::ActiveEqualRate => EQUAL_RATE_HZ,
        }
    }

    fn output_rate_hz(self) -> u32 {
        OUTPUT_RATE_HZ
    }

    fn description(self) -> String {
        match self {
            Self::TransparentEqualRate => {
                "all optional stages bypassed; equal source/output rate".to_string()
            }
            Self::ActiveIirEqualRate => {
                "isolated active EQ + crossfeed + dynamic-loudness IIR path".to_string()
            }
            Self::ActiveSaturation4xEqualRate => {
                "isolated active Oversampled4x Tube saturation finite tail".to_string()
            }
            Self::ConvolverTailEqualRate => {
                "isolated active stereo 256-tap Convolver finite tail".to_string()
            }
            Self::ActiveEqualRate => {
                "EQ + Oversampled4x Tube saturation + crossfeed + dynamic loudness + true-peak limiter + 24-bit TPDF noise shaper".to_string()
            }
            Self::ActiveResampled => format!(
                "active DSP configuration with 44.1 kHz source and 48 kHz output {RESAMPLER_BACKEND_NAME} boundary"
            ),
        }
    }

    fn configuration(self) -> String {
        let eq_enabled = self.iir_enabled();
        let saturation_enabled = self.saturation_enabled();
        let crossfeed_enabled = self.iir_enabled();
        let convolver_enabled = self.convolver_enabled();
        let dynamic_enabled = self.iir_enabled();
        let output_stages_enabled = self.complete_chain_enabled();
        format!(
            "source_rate_hz={};output_rate_hz={};eq_enabled={eq_enabled};eq_gains_db={ACTIVE_EQ_GAINS_DB:?};saturation_armed={saturation_enabled};saturation_enabled={saturation_enabled};saturation_drive=0.85;saturation_threshold=0.35;saturation_mix=0.45;saturation_type=Tube;saturation_quality=Oversampled4x;saturation_highpass=false;crossfeed_enabled={crossfeed_enabled};crossfeed_mix=0.30;crossfeed_cutoff_hz=700;convolver_enabled={convolver_enabled};convolver_ir_frames=256;volume={};muted=false;dynamic_enabled={dynamic_enabled};dynamic_volume={};dynamic_strength={};limiter_enabled={output_stages_enabled};limiter_threshold_db=-1;limiter_release_ms=120;limiter_mode=TruePeak;noise_shaper_enabled={output_stages_enabled};noise_shaper_bits=24;noise_shaper_curve={:?}",
            self.source_rate_hz(),
            self.output_rate_hz(),
            if output_stages_enabled { 0.72 } else { 1.0 },
            if dynamic_enabled { 0.72 } else { 1.0 },
            if dynamic_enabled { 0.65 } else { 0.0 },
            NoiseShaperCurve::auto_select(self.output_rate_hz()),
        )
    }

    fn active(self) -> bool {
        !matches!(self, Self::TransparentEqualRate)
    }

    fn iir_enabled(self) -> bool {
        matches!(
            self,
            Self::ActiveIirEqualRate | Self::ActiveEqualRate | Self::ActiveResampled
        )
    }

    fn saturation_enabled(self) -> bool {
        matches!(
            self,
            Self::ActiveSaturation4xEqualRate | Self::ActiveEqualRate | Self::ActiveResampled
        )
    }

    fn convolver_enabled(self) -> bool {
        matches!(self, Self::ConvolverTailEqualRate)
    }

    fn complete_chain_enabled(self) -> bool {
        matches!(self, Self::ActiveEqualRate | Self::ActiveResampled)
    }

    fn expects_finite_tail(self) -> bool {
        matches!(
            self,
            Self::ActiveSaturation4xEqualRate | Self::ConvolverTailEqualRate
        )
    }

    fn has_unknown_tail(self) -> bool {
        matches!(
            self,
            Self::ActiveIirEqualRate | Self::ActiveEqualRate | Self::ActiveResampled
        )
    }
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct RenderConditions {
    channels: usize,
    output_rate_hz: u32,
    scenarios: Vec<String>,
    scenario_configurations: Vec<String>,
    durations_seconds: Vec<u32>,
    block_frames: Vec<usize>,
    trials: usize,
    allocator_scope: String,
    native_soxr_bytes: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkValidation {
    valid: bool,
    all_samples_finite: bool,
    output_nonempty: bool,
    output_frames: usize,
    expected_nominal_frames: usize,
    checksum: f64,
    #[serde(default)]
    checksum_nonzero: bool,
    tail_truncated: bool,
    #[serde(default)]
    reference_output_frames: usize,
    #[serde(default)]
    changed_samples_from_transparent: usize,
    #[serde(default)]
    max_abs_delta_from_transparent: f64,
    #[serde(default)]
    active_processing_observed: bool,
    #[serde(default)]
    algorithmic_latency_frames: usize,
    #[serde(default)]
    semantic_tail_frames: usize,
    #[serde(default)]
    finite_tail_observed: bool,
    #[serde(default)]
    frame_count_valid: bool,
    #[serde(default)]
    unknown_tail_stopped_before_cap: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct MemoryEvidence {
    chain_setup_allocations: usize,
    chain_steady_state_bytes: usize,
    chain_setup_peak_bytes: usize,
    render_allocations: usize,
    render_deallocations: usize,
    render_reallocations: usize,
    render_peak_live_bytes: usize,
    render_retained_bytes: usize,
    peak_temporary_bytes: usize,
    peak_total_bytes: usize,
    final_output_capacity_bytes: usize,
    configured_resampler_working_bytes: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct RenderCase {
    case_key: String,
    scenario: String,
    scenario_config: String,
    source_rate_hz: u32,
    output_rate_hz: u32,
    block_frames: usize,
    input_frames: usize,
    input_samples: usize,
    output_frames: usize,
    ns_per_input_sample: TrialDistribution,
    realtime_factor: TrialDistribution,
    memory: MemoryEvidence,
    work_validation: WorkValidation,
}

#[derive(Debug, Deserialize, Serialize)]
struct MemoryComparison {
    case_key: String,
    baseline_peak_temporary_bytes: usize,
    candidate_peak_temporary_bytes: usize,
    reduction_pct: f64,
    passed: bool,
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
struct RenderReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: RenderConditions,
    cases: Vec<RenderCase>,
    baseline: Option<BaselineReference>,
    comparisons: Vec<RegressionComparison>,
    memory_comparisons: Vec<MemoryComparison>,
}

fn main() -> Result<(), String> {
    let args = PerfArgs::parse(std::env::args().skip(1).collect())?;
    if args.help {
        print_help();
        return Ok(());
    }

    let (durations_seconds, trials) = workload(args.mode);
    let conditions = RenderConditions {
        channels: CHANNELS,
        output_rate_hz: OUTPUT_RATE_HZ,
        scenarios: Scenario::all()
            .iter()
            .map(|scenario| scenario.name().to_string())
            .collect(),
        scenario_configurations: Scenario::all()
            .iter()
            .map(|scenario| scenario.configuration())
            .collect(),
        durations_seconds: durations_seconds.to_vec(),
        block_frames: vec![SHORT_RENDER_BLOCK_FRAMES, RENDER_BLOCK_FRAMES],
        trials,
        allocator_scope: "Rust global allocator; CPU trials run with instrumentation disabled"
            .to_string(),
        native_soxr_bytes: "excluded; reported configured Rust-side buffer bytes only".to_string(),
    };
    let environment = BenchEnvironment::capture();
    let mut cases = Vec::new();
    for &scenario in Scenario::all() {
        for &duration_seconds in durations_seconds {
            cases.push(benchmark_case(
                scenario,
                duration_seconds,
                RENDER_BLOCK_FRAMES,
                trials,
            )?);
        }
    }
    let short_duration_seconds = durations_seconds[0];
    for &scenario in Scenario::all() {
        cases.push(benchmark_case(
            scenario,
            short_duration_seconds,
            SHORT_RENDER_BLOCK_FRAMES,
            trials,
        )?);
    }

    let mut report = RenderReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: "audio_output_render_perf".to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment,
        conditions,
        cases,
        baseline: None,
        comparisons: Vec::new(),
        memory_comparisons: Vec::new(),
    };

    if let Some(path) = &args.baseline {
        let baseline: RenderReport = read_json(path, "render baseline report")?;
        let (comparisons, memory_comparisons) =
            compare_with_baseline(&report, &baseline, args.max_median_regression_pct)?;
        report.comparisons = comparisons;
        report.memory_comparisons = memory_comparisons;
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
        write_json(path, &report, "render performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }
    Ok(())
}

fn workload(mode: BenchMode) -> (&'static [u32], usize) {
    match mode {
        BenchMode::Quick => (&[1, 5], 7),
        BenchMode::Full => (&[5, 60], 9),
        BenchMode::Heavy => (&[60, 120], 15),
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_output_render_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>]\n\
         \n\
         Reports offline render CPU, realtime factor, Rust allocation counts,\n\
         peak temporary bytes, retained chain bytes, and final output capacity."
    );
}

fn print_report(report: &RenderReport) -> Result<(), String> {
    println!(
        "audio_output_render_perf mode={} output_rate={} channels={} block_frames={:?} trials={} scenarios={} durations_seconds={:?}",
        report.mode.as_str(),
        report.conditions.output_rate_hz,
        report.conditions.channels,
        report.conditions.block_frames,
        report.conditions.trials,
        report.conditions.scenarios.join(","),
        report.conditions.durations_seconds
    );
    println!(
        "audio_output_render_environment {}",
        environment_json(&report.environment)?
    );
    println!(
        "audio_output_render_note allocator_scope={} native_soxr_bytes={}",
        report.conditions.allocator_scope, report.conditions.native_soxr_bytes
    );
    for case in &report.cases {
        println!(
            "output_render case={} block_frames={} input_frames={} output_frames={} ns_per_input_sample_median={:.3} ns_per_input_sample_p95={:.3} realtime_factor_median={:.5} realtime_factor_p95={:.5} setup_bytes={} render_peak_temp_bytes={} render_peak_total_bytes={} output_capacity_bytes={} resampler_working_bytes={} allocs={} active_work_observed={} changed_vs_transparent={} max_delta_vs_transparent={:.6} semantic_tail_frames={}",
            case.case_key,
            case.block_frames,
            case.input_frames,
            case.output_frames,
            case.ns_per_input_sample.median,
            case.ns_per_input_sample.p95,
            case.realtime_factor.median,
            case.realtime_factor.p95,
            case.memory.chain_steady_state_bytes,
            case.memory.peak_temporary_bytes,
            case.memory.peak_total_bytes,
            case.memory.final_output_capacity_bytes,
            case.memory.configured_resampler_working_bytes,
            case.memory.render_allocations,
            case.work_validation.active_processing_observed,
            case.work_validation.changed_samples_from_transparent,
            case.work_validation.max_abs_delta_from_transparent,
            case.work_validation.semantic_tail_frames,
        );
    }
    for comparison in &report.comparisons {
        println!(
            "output_render_baseline case={} baseline_median={:.3} candidate_median={:.3} regression_pct={:.3} threshold_pct={:.3} passed={}",
            comparison.case_key,
            comparison.baseline_median,
            comparison.candidate_median,
            comparison.regression_pct,
            comparison.threshold_pct,
            comparison.passed
        );
    }
    for comparison in &report.memory_comparisons {
        println!(
            "output_render_memory_baseline case={} baseline_peak_temp_bytes={} candidate_peak_temp_bytes={} reduction_pct={:.3} passed={}",
            comparison.case_key,
            comparison.baseline_peak_temporary_bytes,
            comparison.candidate_peak_temporary_bytes,
            comparison.reduction_pct,
            comparison.passed
        );
    }
    Ok(())
}

fn compare_with_baseline(
    candidate: &RenderReport,
    baseline: &RenderReport,
    threshold_pct: f64,
) -> Result<(Vec<RegressionComparison>, Vec<MemoryComparison>), String> {
    validate_performance_baseline(
        "render",
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

    let comparisons = compare_case_medians(
        candidate
            .cases
            .iter()
            .map(|case| (case.case_key.clone(), case.ns_per_input_sample.median)),
        baseline
            .cases
            .iter()
            .map(|case| (case.case_key.clone(), case.ns_per_input_sample.median)),
        threshold_pct,
    )?;

    let mut memory_comparisons = Vec::with_capacity(candidate.cases.len());
    for candidate_case in &candidate.cases {
        let baseline_case = baseline
            .cases
            .iter()
            .find(|case| case.case_key == candidate_case.case_key)
            .ok_or_else(|| format!("missing baseline memory case '{}'", candidate_case.case_key))?;
        let baseline_bytes = baseline_case.memory.peak_temporary_bytes;
        let candidate_bytes = candidate_case.memory.peak_temporary_bytes;
        let reduction_pct = if baseline_bytes == 0 {
            if candidate_bytes == 0 {
                0.0
            } else {
                -100.0
            }
        } else {
            (1.0 - candidate_bytes as f64 / baseline_bytes as f64) * 100.0
        };
        memory_comparisons.push(MemoryComparison {
            case_key: candidate_case.case_key.clone(),
            baseline_peak_temporary_bytes: baseline_bytes,
            candidate_peak_temporary_bytes: candidate_bytes,
            reduction_pct,
            passed: candidate_bytes <= baseline_bytes,
        });
    }
    Ok((comparisons, memory_comparisons))
}

fn enforce_report(report: &RenderReport) -> Result<(), String> {
    let invalid_cases = report
        .cases
        .iter()
        .filter(|case| {
            !case.work_validation.valid
                || case.ns_per_input_sample.samples.len() != report.conditions.trials
                || case.realtime_factor.samples.len() != report.conditions.trials
                || case.memory.peak_total_bytes < case.memory.chain_steady_state_bytes
        })
        .map(|case| case.case_key.as_str())
        .collect::<Vec<_>>();
    if !invalid_cases.is_empty() {
        return Err(format!(
            "render report validity gate failed for cases: {}",
            invalid_cases.join(", ")
        ));
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "render median regression gate failed",
        "ns/input-sample",
    ) {
        return Err(error);
    }
    let memory_failures = report
        .memory_comparisons
        .iter()
        .filter(|comparison| !comparison.passed)
        .map(|comparison| {
            format!(
                "{}: baseline {} bytes, candidate {} bytes",
                comparison.case_key,
                comparison.baseline_peak_temporary_bytes,
                comparison.candidate_peak_temporary_bytes
            )
        })
        .collect::<Vec<_>>();
    if !memory_failures.is_empty() {
        return Err(format!(
            "render temporary-memory gate failed: {}",
            memory_failures.join("; ")
        ));
    }
    validate_memory_scaling(report)
}

fn validate_memory_scaling(report: &RenderReport) -> Result<(), String> {
    for scenario in &report.conditions.scenarios {
        for &block_frames in &report.conditions.block_frames {
            let mut values = report
                .cases
                .iter()
                .filter(|case| case.scenario == *scenario && case.block_frames == block_frames)
                .map(|case| case.memory.peak_temporary_bytes);
            let Some(first) = values.next() else {
                return Err(format!(
                    "missing memory cases for scenario '{scenario}' block_frames={block_frames}"
                ));
            };
            let (minimum, maximum) = values.fold((first, first), |(minimum, maximum), value| {
                (minimum.min(value), maximum.max(value))
            });
            if maximum > minimum.saturating_mul(2).saturating_add(64 * 1024) {
                return Err(format!(
                    "render temporary-memory scaling gate failed for {scenario} block_frames={block_frames}: min={minimum} max={maximum}"
                ));
            }
        }
    }
    Ok(())
}

fn benchmark_case(
    scenario: Scenario,
    duration_seconds: u32,
    block_frames: usize,
    trials: usize,
) -> Result<RenderCase, String> {
    let source_rate_hz = scenario.source_rate_hz();
    let output_rate_hz = scenario.output_rate_hz();
    let input_frames = (source_rate_hz as usize)
        .checked_mul(duration_seconds as usize)
        .ok_or_else(|| "input frame count overflow".to_string())?;
    let input = synthetic_input(input_frames, source_rate_hz);
    let work_validation =
        validate_work(scenario, &input, input_frames, output_rate_hz, block_frames)?;
    let setup = measure_chain_setup(scenario)?;
    let mut ns_per_input_sample = Vec::with_capacity(trials);
    let mut realtime_factor = Vec::with_capacity(trials);

    for _ in 0..trials {
        let mut chain = build_chain(scenario)?;
        warm_chain(&mut chain, &input, block_frames)?;
        let start = Instant::now();
        let rendered = chain
            .render_with_policy_and_block_frames(
                &input,
                OfflineRenderPolicy::default(),
                block_frames,
            )
            .map_err(|error| error.to_string())?;
        let elapsed_ns = start.elapsed().as_nanos() as f64;
        let elapsed_seconds = elapsed_ns / 1.0e9;
        ns_per_input_sample.push(elapsed_ns / input.len() as f64);
        realtime_factor.push(elapsed_seconds / duration_seconds as f64);
        black_box(rendered.samples.len());
    }

    let memory = measure_render_memory(scenario, &input, block_frames, &setup)?;
    let output_frames = work_validation.output_frames;
    Ok(RenderCase {
        case_key: format!(
            "scenario={};duration_seconds={duration_seconds};source_rate={source_rate_hz};output_rate={output_rate_hz};block_frames={block_frames}",
            scenario.name()
        ),
        scenario: scenario.name().to_string(),
        scenario_config: scenario.description(),
        source_rate_hz,
        output_rate_hz,
        block_frames,
        input_frames,
        input_samples: input.len(),
        output_frames,
        ns_per_input_sample: summarize_trials(ns_per_input_sample)?,
        realtime_factor: summarize_trials(realtime_factor)?,
        memory,
        work_validation,
    })
}

fn measure_chain_setup(scenario: Scenario) -> Result<MemoryEvidence, String> {
    let scope = AllocationScope::start();
    let params = build_params(scenario);
    let chain = OutputChainBuilder::new(params)
        .build_render_chain_with_policy(OfflineRenderPolicy::default())
        .map_err(|error| error.to_string())?;
    let snapshot = scope.finish();
    black_box(&chain);
    Ok(MemoryEvidence {
        chain_setup_allocations: snapshot.allocations,
        chain_steady_state_bytes: snapshot.live_bytes,
        chain_setup_peak_bytes: snapshot.peak_live_bytes,
        render_allocations: 0,
        render_deallocations: 0,
        render_reallocations: 0,
        render_peak_live_bytes: 0,
        render_retained_bytes: 0,
        peak_temporary_bytes: 0,
        peak_total_bytes: snapshot.live_bytes,
        final_output_capacity_bytes: 0,
        configured_resampler_working_bytes: configured_resampler_bytes(scenario),
    })
}

fn measure_render_memory(
    scenario: Scenario,
    input: &[f64],
    block_frames: usize,
    setup: &MemoryEvidence,
) -> Result<MemoryEvidence, String> {
    let mut chain = build_chain(scenario)?;
    warm_chain(&mut chain, input, block_frames)?;
    let scope = AllocationScope::start();
    let rendered = chain
        .render_with_policy_and_block_frames(input, OfflineRenderPolicy::default(), block_frames)
        .map_err(|error| error.to_string())?;
    let output_capacity_bytes = rendered
        .samples
        .capacity()
        .saturating_mul(std::mem::size_of::<f64>());
    let snapshot = scope.finish();
    let retained_bytes = snapshot.live_bytes;
    let peak_temporary_bytes = snapshot
        .peak_live_bytes
        .saturating_sub(retained_bytes.max(output_capacity_bytes));
    Ok(MemoryEvidence {
        chain_setup_allocations: setup.chain_setup_allocations,
        chain_steady_state_bytes: setup.chain_steady_state_bytes,
        chain_setup_peak_bytes: setup.chain_setup_peak_bytes,
        render_allocations: snapshot.allocations,
        render_deallocations: snapshot.deallocations,
        render_reallocations: snapshot.reallocations,
        render_peak_live_bytes: snapshot.peak_live_bytes,
        render_retained_bytes: retained_bytes,
        peak_temporary_bytes,
        peak_total_bytes: setup
            .chain_steady_state_bytes
            .saturating_add(snapshot.peak_live_bytes),
        final_output_capacity_bytes: output_capacity_bytes,
        configured_resampler_working_bytes: setup.configured_resampler_working_bytes,
    })
}

fn validate_work(
    scenario: Scenario,
    input: &[f64],
    input_frames: usize,
    output_rate_hz: u32,
    block_frames: usize,
) -> Result<WorkValidation, String> {
    let mut chain = build_chain(scenario)?;
    let rendered = chain
        .render_with_policy_and_block_frames(input, OfflineRenderPolicy::default(), block_frames)
        .map_err(|error| error.to_string())?;
    let mut transparent_chain = build_chain_with_activity(scenario, false)?;
    let transparent = transparent_chain
        .render_with_policy_and_block_frames(input, OfflineRenderPolicy::default(), block_frames)
        .map_err(|error| error.to_string())?;
    let output_frames = rendered.samples.len() / CHANNELS;
    let expected_nominal_frames =
        ((input_frames as u64 * output_rate_hz as u64) / scenario.source_rate_hz() as u64) as usize;
    let all_samples_finite = rendered.samples.iter().all(|sample| sample.is_finite());
    let output_nonempty = !rendered.samples.is_empty();
    let checksum = rendered
        .samples
        .iter()
        .copied()
        .fold(0.0, |sum, sample| sum + sample.abs());
    let checksum_nonzero = checksum > 0.0;
    let comparison_samples = rendered.samples.len().min(transparent.samples.len());
    let mut changed_samples_from_transparent = 0usize;
    let mut max_abs_delta_from_transparent = 0.0_f64;
    for (&candidate, &reference) in rendered
        .samples
        .iter()
        .zip(&transparent.samples)
        .take(comparison_samples)
    {
        let delta = (candidate - reference).abs();
        max_abs_delta_from_transparent = max_abs_delta_from_transparent.max(delta);
        changed_samples_from_transparent += usize::from(candidate.to_bits() != reference.to_bits());
    }
    let minimum_changed_samples = (comparison_samples / 100).max(1);
    let active_processing_observed = !scenario.active()
        || (changed_samples_from_transparent >= minimum_changed_samples
            && max_abs_delta_from_transparent > 1.0e-6);
    let finite_tail_observed = !scenario.expects_finite_tail() || rendered.semantic_tail_frames > 0;
    let expected_finite_frames =
        expected_nominal_frames.saturating_add(rendered.semantic_tail_frames);
    let unknown_tail_limit_frames = (OfflineRenderPolicy::default().unknown_tail.max_tail_ms
        as usize)
        .saturating_mul(output_rate_hz as usize)
        / 1_000;
    let generated_after_nominal = output_frames.saturating_sub(expected_nominal_frames);
    let unknown_tail_stopped_before_cap = !scenario.has_unknown_tail()
        || (!rendered.tail_truncated && generated_after_nominal < unknown_tail_limit_frames);
    let frame_count_valid = if scenario.has_unknown_tail() {
        output_frames >= expected_nominal_frames && unknown_tail_stopped_before_cap
    } else {
        output_frames == expected_finite_frames
    };
    Ok(WorkValidation {
        valid: all_samples_finite
            && output_nonempty
            && frame_count_valid
            && !rendered.tail_truncated
            && checksum.is_finite()
            && checksum_nonzero
            && active_processing_observed
            && finite_tail_observed,
        all_samples_finite,
        output_nonempty,
        output_frames,
        expected_nominal_frames,
        checksum,
        checksum_nonzero,
        tail_truncated: rendered.tail_truncated,
        reference_output_frames: transparent.samples.len() / CHANNELS,
        changed_samples_from_transparent,
        max_abs_delta_from_transparent,
        active_processing_observed,
        algorithmic_latency_frames: rendered.algorithmic_latency_frames,
        semantic_tail_frames: rendered.semantic_tail_frames,
        finite_tail_observed,
        frame_count_valid,
        unknown_tail_stopped_before_cap,
    })
}

fn build_chain(scenario: Scenario) -> Result<OutputRenderChain, String> {
    build_chain_with_activity(scenario, scenario.active())
}

fn build_chain_with_activity(
    scenario: Scenario,
    active: bool,
) -> Result<OutputRenderChain, String> {
    OutputChainBuilder::new(build_params_with_activity(scenario, active))
        .build_render_chain_with_policy(OfflineRenderPolicy::default())
        .map_err(|error| error.to_string())
}

fn build_params(scenario: Scenario) -> OutputChainParams {
    build_params_with_activity(scenario, scenario.active())
}

fn build_params_with_activity(scenario: Scenario, active: bool) -> OutputChainParams {
    let source_rate_hz = scenario.source_rate_hz();
    let output_rate_hz = scenario.output_rate_hz();
    let eq_params = Arc::new(AtomicEqParams::new());
    let saturation_params = Arc::new(AtomicSaturationParams::new());
    let crossfeed_params = Arc::new(AtomicCrossfeedParams::new());
    let limiter_params = Arc::new(AtomicPeakLimiterParams::new());
    let volume_params = Arc::new(AtomicVolumeParams::new());
    let noise_shaper_params = Arc::new(AtomicNoiseShaperParams::new());
    let dynamic_loudness_params = Arc::new(AtomicDynamicLoudnessParams::new());
    let iir_enabled = active && scenario.iir_enabled();
    let saturation_enabled = active && scenario.saturation_enabled();
    let convolver_enabled = active && scenario.convolver_enabled();
    let output_stages_enabled = active && scenario.complete_chain_enabled();

    if iir_enabled {
        eq_params.write(&ACTIVE_EQ_GAINS_DB, true);
    } else {
        eq_params.write(&[0.0; EQ_BANDS], false);
    }
    saturation_params.set_enabled(saturation_enabled);
    saturation_params.set_armed(saturation_enabled);
    saturation_params.set_drive(0.85);
    saturation_params.set_threshold(0.35);
    saturation_params.set_mix(0.45);
    saturation_params.set_sat_type(SaturationTypeValue::Tube);
    saturation_params.set_quality(SaturationQualityValue::Oversampled4x);
    saturation_params.set_highpass_mode(false);
    saturation_params.set_highpass_cutoff(4_000.0);
    crossfeed_params.set_enabled(iir_enabled);
    crossfeed_params.set_mix(0.30);
    crossfeed_params.set_cutoff(700.0);
    limiter_params.set_enabled(output_stages_enabled);
    limiter_params.set_threshold(-1.0);
    limiter_params.set_release(120.0);
    volume_params.set_volume(if output_stages_enabled { 0.72 } else { 1.0 });
    volume_params.set_muted(false);
    noise_shaper_params.set_enabled(output_stages_enabled);
    noise_shaper_params.set_bits(24);
    noise_shaper_params.set_curve(NoiseShaperCurve::auto_select(output_rate_hz));
    dynamic_loudness_params.set_enabled(iir_enabled);
    dynamic_loudness_params.set_volume(if iir_enabled { 0.72 } else { 1.0 });
    dynamic_loudness_params.set_strength(if iir_enabled { 0.65 } else { 0.0 });
    let convolver_control = ConvolverControl::new(convolver_enabled);
    if convolver_enabled {
        convolver_control
            .publish_at_rate(
                FFTConvolver::new(&synthetic_ir(256, CHANNELS), CHANNELS)
                    .expect("benchmark IR geometry is valid"),
                source_rate_hz,
            )
            .expect("benchmark source sample rate is non-zero");
    }

    OutputChainParams {
        channels: CHANNELS,
        source_sample_rate: source_rate_hz,
        output_sample_rate: output_rate_hz,
        eq_params,
        saturation_params,
        crossfeed_params,
        convolver_control,
        volume_params,
        dynamic_loudness_params,
        dynamic_loudness_telemetry: Arc::new(AtomicDynamicLoudnessTelemetry::new()),
        limiter_params,
        noise_shaper_params,
    }
}

fn warm_chain(
    chain: &mut OutputRenderChain,
    input: &[f64],
    block_frames: usize,
) -> Result<(), String> {
    let warm_samples = input.len().min(WARMUP_FRAMES * CHANNELS);
    if warm_samples == 0 {
        return Ok(());
    }
    chain
        .render_with_policy_and_block_frames(
            &input[..warm_samples],
            OfflineRenderPolicy::default(),
            block_frames,
        )
        .map(|rendered| {
            black_box(rendered.samples.len());
        })
        .map_err(|error| error.to_string())
}

fn configured_resampler_bytes(scenario: Scenario) -> usize {
    if scenario.source_rate_hz() == scenario.output_rate_hz() {
        0
    } else {
        StreamingResampler::working_buffer_bytes(
            CHANNELS,
            scenario.source_rate_hz(),
            scenario.output_rate_hz(),
        )
        .unwrap_or(0)
    }
}

fn synthetic_input(frames: usize, sample_rate_hz: u32) -> Vec<f64> {
    let mut output = Vec::with_capacity(frames * CHANNELS);
    let sample_rate = sample_rate_hz as f64;
    let mut left_phase = 0.0;
    let mut right_phase = 0.0;
    for frame in 0..frames {
        let t = frame as f64 / sample_rate;
        left_phase += std::f64::consts::TAU * (220.0 + 11.0 * (t * 3.0).sin()) / sample_rate;
        right_phase += std::f64::consts::TAU * (330.0 + 7.0 * (t * 5.0).cos()) / sample_rate;
        let envelope = 0.65 + 0.20 * (std::f64::consts::TAU * 1.7 * t).sin();
        let transient = if frame % 127 == 0 { 0.28 } else { 0.0 };
        output.push(
            (left_phase.sin() * 0.55 + (left_phase * 3.0).sin() * 0.08 + transient) * envelope,
        );
        output.push(
            (right_phase.sin() * 0.50 - (right_phase * 2.0).cos() * 0.07 - transient) * envelope,
        );
    }
    output
}

fn synthetic_ir(taps_per_channel: usize, channels: usize) -> Vec<f64> {
    let mut ir = Vec::with_capacity(taps_per_channel * channels);
    for tap in 0..taps_per_channel {
        let decay = (-(tap as f64) / 48.0).exp();
        for channel in 0..channels {
            let direct = if tap == 0 { 0.72 } else { 0.0 };
            let early = if tap == 17 + channel * 3 { 0.12 } else { 0.0 };
            let tail = ((tap + channel * 11) as f64 * 0.37).sin() * 0.025 * decay;
            ir.push(direct + early + tail);
        }
    }
    ir
}
