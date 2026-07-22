use std::hint::black_box;
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
    process_checked, AudioBlockMut, AudioBlockRef, ProcessBuffers, StreamingProcessor,
    StreamingResampler, RESAMPLER_BACKEND_NAME,
};

const CHANNELS: usize = 2;
const BUFFER_FRAMES: [usize; 3] = [128, 256, 512];
const WARMUP_BUFFERS: usize = 64;
const VALIDATION_BUFFERS: usize = 8;

#[cfg(feature = "soxr")]
const RESAMPLER_ALGORITHM_LABEL: &str = "streaming default quality/phase";
#[cfg(all(feature = "rubato", not(feature = "soxr")))]
const RESAMPLER_ALGORITHM_LABEL: &str =
    "native-interleaved streaming FFT/sinc routing (High FFT, UltraHigh sinc)";

#[cfg(feature = "soxr")]
const RESAMPLER_ALGORITHM_ID: &str = "streaming_default";
#[cfg(all(feature = "rubato", not(feature = "soxr")))]
const RESAMPLER_ALGORITHM_ID: &str = "streaming_native_interleaved_fft_sinc";

fn resampler_algorithm_label() -> String {
    format!("{RESAMPLER_BACKEND_NAME} {RESAMPLER_ALGORITHM_LABEL}")
}

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    from_rate: u32,
    to_rate: u32,
}

const SCENARIOS: [Scenario; 3] = [
    Scenario {
        name: "equal_rate_48k",
        from_rate: 48_000,
        to_rate: 48_000,
    },
    Scenario {
        name: "music_44k1_to_48k",
        from_rate: 44_100,
        to_rate: 48_000,
    },
    Scenario {
        name: "upsample_48k_to_96k",
        from_rate: 48_000,
        to_rate: 96_000,
    },
];

#[derive(Clone, Copy)]
enum ApiPath {
    Checked,
    Direct,
}

impl ApiPath {
    fn name(self) -> &'static str {
        match self {
            Self::Checked => "process_checked",
            Self::Direct => "trait_process_direct",
        }
    }

    fn all() -> &'static [Self] {
        &[Self::Checked, Self::Direct]
    }
}

struct TrialMeasurement {
    ns_per_input_sample: f64,
    ns_per_input_buffer: f64,
    output_frames_total: usize,
    consumed_frames_total: usize,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct ResamplerConditions {
    channels: usize,
    algorithm: String,
    coverage: String,
    excludes: Vec<String>,
    warmup_buffers: usize,
    iterations_per_trial: usize,
    trials: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResamplerWorkValidation {
    valid: bool,
    validation_buffers: usize,
    consumed_frames: usize,
    expected_consumed_frames: usize,
    produced_frames: usize,
    all_output_samples_finite: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResamplerCase {
    case_key: String,
    scenario: String,
    api: String,
    algorithm: String,
    frames: usize,
    input_samples: usize,
    from_rate_hz: u32,
    to_rate_hz: u32,
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
    work_validation: ResamplerWorkValidation,
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
struct ResamplerReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: ResamplerConditions,
    cases: Vec<ResamplerCase>,
    baseline: Option<BaselineReference>,
    comparisons: Vec<RegressionComparison>,
}

#[derive(Clone, Copy)]
struct ApiRun {
    consumed_frames: usize,
    produced_frames: usize,
    all_output_samples_finite: bool,
}

fn main() -> Result<(), String> {
    let args = PerfArgs::parse(std::env::args().skip(1).collect())?;
    if args.help {
        print_help();
        return Ok(());
    }

    let (iterations, trials) = workload(args.mode);
    let environment = BenchEnvironment::capture();
    let conditions = ResamplerConditions {
        channels: CHANNELS,
        algorithm: resampler_algorithm_label(),
        coverage: "streaming_resampler_only".to_string(),
        excludes: [
            "decoder",
            "callback_dsp_chain",
            "cpal_device_write",
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
    for scenario in SCENARIOS {
        for frames in BUFFER_FRAMES {
            let input = synthetic_buffer(frames, CHANNELS, scenario.from_rate);
            for &api in ApiPath::all() {
                cases.push(benchmark_api(
                    scenario, api, frames, &input, iterations, trials,
                )?);
            }
        }
    }

    let mut report = ResamplerReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: "audio_resampler_streaming_perf".to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment,
        conditions,
        cases,
        baseline: None,
        comparisons: Vec::new(),
    };
    if let Some(path) = &args.baseline {
        let baseline: ResamplerReport = read_json(path, "resampler baseline report")?;
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
        write_json(path, &report, "resampler performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }

    Ok(())
}

fn workload(mode: BenchMode) -> (usize, usize) {
    match mode {
        BenchMode::Quick => (75, 7),
        BenchMode::Full => (400, 9),
        BenchMode::Heavy => (1_350, 15),
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_resampler_streaming_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>]\n\
         \n\
         Reports trial min/median/p95/max and source-buffer realtime-reference utilization.\n\
         Timing is report-only unless a compatible same-machine baseline is supplied."
    );
}

fn print_report(report: &ResamplerReport) -> Result<(), String> {
    println!(
        "audio_resampler_streaming_perf mode={} channels={} coverage=streaming_resampler_only iterations={} trials={}",
        report.mode.as_str(),
        report.conditions.channels,
        report.conditions.iterations_per_trial,
        report.conditions.trials
    );
    println!(
        "audio_resampler_streaming_environment {}",
        environment_json(&report.environment)?
    );
    println!(
        "audio_resampler_streaming_note excludes={} utilization_reference=source_buffer_duration",
        report.conditions.excludes.join(",")
    );
    for case in &report.cases {
        println!(
            "resampler_streaming case={} scenario={} api={} frames={} input_samples={} from_rate={} to_rate={} output_frames_total_median={} ns_per_input_sample_min={:.3} ns_per_input_sample_median={:.3} ns_per_input_sample_p95={:.3} ns_per_input_sample_max={:.3} ns_per_input_buffer_median={:.3} ns_per_input_buffer_p95={:.3} median_source_realtime_utilization_pct={:.4} p95_source_realtime_utilization_pct={:.4}",
            case.case_key,
            case.scenario,
            case.api,
            case.frames,
            case.input_samples,
            case.from_rate_hz,
            case.to_rate_hz,
            median_usize(&case.output_frames_total_per_trial),
            case.ns_per_input_sample.min,
            case.ns_per_input_sample.median,
            case.ns_per_input_sample.p95,
            case.ns_per_input_sample.max,
            case.ns_per_input_buffer.median,
            case.ns_per_input_buffer.p95,
            case.median_source_realtime_utilization_pct,
            case.p95_source_realtime_utilization_pct
        );
    }
    for comparison in &report.comparisons {
        println!(
            "resampler_streaming_baseline case={} baseline_median={:.3} candidate_median={:.3} regression_pct={:.3} threshold_pct={:.3} passed={}",
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

fn median_usize(values: &[usize]) -> usize {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn compare_with_baseline(
    candidate: &ResamplerReport,
    baseline: &ResamplerReport,
    threshold_pct: f64,
) -> Result<Vec<RegressionComparison>, String> {
    validate_performance_baseline(
        "resampler",
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

fn enforce_report(report: &ResamplerReport) -> Result<(), String> {
    let invalid_cases = report
        .cases
        .iter()
        .filter(|case| {
            !case.work_validation.valid
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
            "resampler report validity gate failed for cases: {}",
            invalid_cases.join(", ")
        ));
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "resampler median regression gate failed",
        "ns/input-sample",
    ) {
        return Err(error);
    }
    Ok(())
}

fn benchmark_api(
    scenario: Scenario,
    api: ApiPath,
    frames: usize,
    input: &[f64],
    iterations: usize,
    trials: usize,
) -> Result<ResamplerCase, String> {
    let work_validation = validate_work(scenario, api, input);
    let mut ns_per_input_sample = Vec::with_capacity(trials);
    let mut ns_per_input_buffer = Vec::with_capacity(trials);
    let mut output_frames_total_per_trial = Vec::with_capacity(trials);
    let mut consumed_frames_total_per_trial = Vec::with_capacity(trials);

    for _ in 0..trials {
        let mut resampler = StreamingResampler::new(CHANNELS, scenario.from_rate, scenario.to_rate)
            .expect("valid resampler rates");
        warm_resampler(&mut resampler, api, input);
        let measurement = measure_resampler(&mut resampler, api, frames, input, iterations)?;
        ns_per_input_sample.push(measurement.ns_per_input_sample);
        ns_per_input_buffer.push(measurement.ns_per_input_buffer);
        output_frames_total_per_trial.push(measurement.output_frames_total);
        consumed_frames_total_per_trial.push(measurement.consumed_frames_total);
    }

    let ns_per_input_sample = summarize_trials(ns_per_input_sample)?;
    let ns_per_input_buffer = summarize_trials(ns_per_input_buffer)?;
    let expected_output_frames_total = (frames as f64 * iterations as f64 * scenario.to_rate as f64
        / scenario.from_rate as f64)
        .round() as usize;
    let minimum_output_frames_total = expected_output_frames_total * 95 / 100;
    let maximum_output_frames_total = expected_output_frames_total * 105 / 100;
    let source_buffer_duration_ns = frames as f64 / scenario.from_rate as f64 * 1.0e9;

    Ok(ResamplerCase {
        case_key: format!(
            "scenario={};api={};frames={};from={};to={};algorithm={RESAMPLER_BACKEND_NAME}_{RESAMPLER_ALGORITHM_ID}",
            scenario.name,
            api.name(),
            frames,
            scenario.from_rate,
            scenario.to_rate
        ),
        scenario: scenario.name.to_string(),
        api: api.name().to_string(),
        algorithm: resampler_algorithm_label(),
        frames,
        input_samples: frames * CHANNELS,
        from_rate_hz: scenario.from_rate,
        to_rate_hz: scenario.to_rate,
        median_source_realtime_utilization_pct: ns_per_input_buffer.median
            / source_buffer_duration_ns
            * 100.0,
        p95_source_realtime_utilization_pct: ns_per_input_buffer.p95 / source_buffer_duration_ns
            * 100.0,
        ns_per_input_sample,
        ns_per_input_buffer,
        source_buffer_duration_ns,
        expected_output_frames_total,
        minimum_output_frames_total,
        maximum_output_frames_total,
        output_frames_total_per_trial,
        consumed_frames_total_per_trial,
        work_validation,
    })
}

fn validate_work(scenario: Scenario, api: ApiPath, input: &[f64]) -> ResamplerWorkValidation {
    let mut resampler = StreamingResampler::new(CHANNELS, scenario.from_rate, scenario.to_rate)
        .expect("valid resampler rates");
    warm_resampler(&mut resampler, api, input);
    let mut output = vec![0.0; streaming_output_capacity(&resampler, input.len())];
    let mut consumed_frames = 0usize;
    let mut produced_frames = 0usize;
    let mut all_output_samples_finite = true;
    for _ in 0..VALIDATION_BUFFERS {
        let run = run_api(&mut resampler, api, input, &mut output, true);
        consumed_frames += run.consumed_frames;
        produced_frames += run.produced_frames;
        all_output_samples_finite &= run.all_output_samples_finite;
    }
    let expected_consumed_frames = input.len() / CHANNELS * VALIDATION_BUFFERS;
    ResamplerWorkValidation {
        valid: consumed_frames == expected_consumed_frames
            && produced_frames > 0
            && all_output_samples_finite,
        validation_buffers: VALIDATION_BUFFERS,
        consumed_frames,
        expected_consumed_frames,
        produced_frames,
        all_output_samples_finite,
    }
}

fn warm_resampler(resampler: &mut StreamingResampler, api: ApiPath, input: &[f64]) {
    let mut output = vec![0.0; streaming_output_capacity(resampler, input.len())];

    for _ in 0..WARMUP_BUFFERS {
        let _ = run_api(resampler, api, input, &mut output, false);
    }
}

fn measure_resampler(
    resampler: &mut StreamingResampler,
    api: ApiPath,
    frames: usize,
    input: &[f64],
    iterations: usize,
) -> Result<TrialMeasurement, String> {
    let mut output = vec![0.0; streaming_output_capacity(resampler, input.len())];
    let mut output_frames_total = 0usize;
    let mut consumed_frames_total = 0usize;
    let start = Instant::now();

    for _ in 0..iterations {
        let run = run_api(resampler, api, black_box(input), &mut output, false);
        consumed_frames_total += run.consumed_frames;
        output_frames_total += run.produced_frames;
    }

    let elapsed = start.elapsed();
    let expected_output_frames = (frames as f64 * iterations as f64 * resampler.to_rate() as f64
        / resampler.from_rate() as f64)
        .round() as usize;
    let minimum_output_frames = expected_output_frames * 95 / 100;
    let maximum_output_frames = expected_output_frames * 105 / 100;
    if output_frames_total < minimum_output_frames || output_frames_total > maximum_output_frames {
        return Err(format!(
            "{}->{} {} produced {} frames outside [{}, {}] for {} expected streaming frames",
            resampler.from_rate(),
            resampler.to_rate(),
            api.name(),
            output_frames_total,
            minimum_output_frames,
            maximum_output_frames,
            expected_output_frames
        ));
    }
    let expected_consumed_frames = frames * iterations;
    if consumed_frames_total != expected_consumed_frames {
        return Err(format!(
            "{}->{} {} consumed {} frames for {} expected input frames",
            resampler.from_rate(),
            resampler.to_rate(),
            api.name(),
            consumed_frames_total,
            expected_consumed_frames
        ));
    }
    let ns_per_input_buffer = elapsed.as_nanos() as f64 / iterations as f64;
    let ns_per_input_sample = ns_per_input_buffer / (frames * CHANNELS) as f64;

    Ok(TrialMeasurement {
        ns_per_input_sample,
        ns_per_input_buffer,
        output_frames_total,
        consumed_frames_total,
    })
}

fn streaming_output_capacity(resampler: &StreamingResampler, input_samples: usize) -> usize {
    // Streaming resampler backends can emit delayed output in bursts after
    // internal buffering. Use a deliberately conservative caller scratch so
    // this bench measures the API path rather than capacity edge behavior.
    resampler
        .max_output_len_for_input(input_samples)
        .saturating_mul(8)
        .saturating_add(8192)
}

fn run_api(
    resampler: &mut StreamingResampler,
    api: ApiPath,
    input: &[f64],
    output: &mut [f64],
    validate_output: bool,
) -> ApiRun {
    let input_frames = input.len() / CHANNELS;
    let mut consumed_frames = 0;
    let mut output_frames = 0;
    let mut all_output_samples_finite = true;
    while consumed_frames < input_frames {
        let input_block =
            AudioBlockRef::new(&input[consumed_frames * CHANNELS..], CHANNELS).unwrap();
        let output_block = AudioBlockMut::new(output, CHANNELS).unwrap();
        let buffers = ProcessBuffers::out_of_place(input_block, output_block).unwrap();
        let progress = match api {
            ApiPath::Checked => process_checked(resampler, buffers).unwrap(),
            ApiPath::Direct => resampler.process(buffers).unwrap(),
        };
        consumed_frames += progress.consumed_frames();
        output_frames += progress.produced_frames();
        let produced = &output[..progress.produced_frames() * CHANNELS];
        if validate_output {
            all_output_samples_finite &= produced.iter().all(|sample| sample.is_finite());
        }
        black_box(produced);
    }
    ApiRun {
        consumed_frames,
        produced_frames: output_frames,
        all_output_samples_finite,
    }
}

fn synthetic_buffer(frames: usize, channels: usize, sample_rate: u32) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    let sample_rate = sample_rate as f64;
    let mut left_phase = 0.0_f64;
    let mut right_phase = 0.0_f64;

    for frame in 0..frames {
        let t = frame as f64 / sample_rate;
        left_phase += std::f64::consts::TAU * (330.0 + 17.0 * (t * 2.5).sin()) / sample_rate;
        right_phase += std::f64::consts::TAU * (550.0 + 23.0 * (t * 1.7).cos()) / sample_rate;
        let envelope = 0.7 + 0.15 * (std::f64::consts::TAU * 1.1 * t).sin();
        let left = (left_phase.sin() * 0.6 + (left_phase * 2.0).sin() * 0.05) * envelope;
        let right = (right_phase.sin() * 0.55 - (right_phase * 3.0).cos() * 0.04) * envelope;

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
