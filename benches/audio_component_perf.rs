use std::hint::black_box;
use std::time::Instant;

use audio_engine_core::{
    analyze_automix, AutomixAnalysisMode, AutomixAnalysisOptions, ChannelLayout,
    DownmixCoefficients, Downmixer, LoudnessMeter, MediaLocation, RingBuffer, SpectrumAnalyzer,
    TruePeakDetector,
};
#[cfg(feature = "loudness-db")]
use audio_engine_core::{LoudnessDatabase, LoudnessDatabaseError, TrackLoudness};
use serde::{Deserialize, Serialize};

pub mod support;

use support::audio_fixture::{
    ensure_deterministic_pcm_fixture, fixture_path_display, DeterministicPcmFixtureMetadata,
};
use support::{
    compare_case_medians, environment_json, generated_unix_ms, read_json, regression_gate_error,
    summarize_trials, validate_case_key_set, validate_performance_baseline, write_json_round_trip,
    BenchEnvironment, BenchMode, PerfArgs, PerformanceReportIdentity, RegressionComparison,
    TrialDistribution, REPORT_SCHEMA_VERSION,
};

const PROBE: &str = "audio_public_component_perf";
const SAMPLE_RATE_HZ: u32 = 48_000;

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct ComponentConditions {
    trials: usize,
    iteration_scale: usize,
    automix_trials: usize,
    case_keys: Vec<String>,
    automix_fixture_path: String,
    automix_fixture_hash: String,
    automix_fixture: DeterministicPcmFixtureMetadata,
    automix_window_seconds: f64,
    loudness_database_scope: String,
    timer_scope: String,
    network_scope: String,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct ComponentWorkValidation {
    valid: bool,
    observed_operations: usize,
    expected_operations: usize,
    observed_work_items: u64,
    expected_work_items: u64,
    all_output_finite: bool,
    output_nontrivial: bool,
    checksum: f64,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct ComponentCase {
    case_key: String,
    component: String,
    operation: String,
    primary_unit: String,
    work_items_per_iteration: usize,
    iterations_per_trial: usize,
    expected_timing_samples: usize,
    distribution: TrialDistribution,
    work_validation: ComponentWorkValidation,
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
struct ComponentReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: ComponentConditions,
    cases: Vec<ComponentCase>,
    baseline: Option<BaselineReference>,
    comparisons: Vec<RegressionComparison>,
}

#[derive(Clone, Copy)]
struct ComponentWorkload {
    trials: usize,
    iteration_scale: usize,
    automix_trials: usize,
}

struct ComponentCaseInput {
    case_key: String,
    component: &'static str,
    operation: &'static str,
    primary_unit: &'static str,
    work_items_per_iteration: usize,
    iterations_per_trial: usize,
    samples: Vec<f64>,
    expected_operations: usize,
    expected_work_items: usize,
    all_output_finite: bool,
    output_nontrivial: bool,
    checksum: f64,
}

fn main() -> Result<(), String> {
    let args = PerfArgs::parse(std::env::args().skip(1).collect())?;
    if args.help {
        print_help();
        return Ok(());
    }

    let workload = workload(args.mode);
    let fixture = ensure_deterministic_pcm_fixture()?;
    let fixture_path = fixture.path.to_string_lossy().into_owned();
    let mut cases = vec![
        benchmark_spectrum(1_024, 64, 128 * workload.iteration_scale, workload.trials)?,
        benchmark_spectrum(4_096, 96, 32 * workload.iteration_scale, workload.trials)?,
        benchmark_downmix(
            ChannelLayout::surround_5_1(),
            DownmixCoefficients::ItuRbs775,
            "itu_r_bs775",
            512,
            256 * workload.iteration_scale,
            workload.trials,
        )?,
        benchmark_downmix(
            ChannelLayout::surround_7_1(),
            DownmixCoefficients::AtscA85,
            "atsc_a85",
            512,
            192 * workload.iteration_scale,
            workload.trials,
        )?,
        benchmark_loudness(512, 96 * workload.iteration_scale, workload.trials)?,
        benchmark_loudness(4_096, 16 * workload.iteration_scale, workload.trials)?,
        benchmark_true_peak_contiguous(4_096, 128 * workload.iteration_scale, workload.trials)?,
        benchmark_true_peak_strided(4_096, 96 * workload.iteration_scale, workload.trials)?,
        benchmark_automix(
            &fixture_path,
            AutomixAnalysisMode::Head,
            workload.automix_trials,
        )?,
        benchmark_automix(
            &fixture_path,
            AutomixAnalysisMode::Full,
            workload.automix_trials,
        )?,
        benchmark_ring_buffer(512, 1_024 * workload.iteration_scale, workload.trials)?,
    ];

    #[cfg(feature = "loudness-db")]
    {
        cases.extend(benchmark_loudness_database(workload)?);
    }

    cases.sort_by(|left, right| left.case_key.cmp(&right.case_key));
    let conditions = ComponentConditions {
        trials: workload.trials,
        iteration_scale: workload.iteration_scale,
        automix_trials: workload.automix_trials,
        case_keys: cases.iter().map(|case| case.case_key.clone()).collect(),
        automix_fixture_path: fixture_path_display(&fixture.path),
        automix_fixture_hash: fixture.metadata.content_fnv1a64.clone(),
        automix_fixture: fixture.metadata,
        automix_window_seconds: 5.0,
        loudness_database_scope: loudness_database_scope(),
        timer_scope: "construction, input generation, warmup, validation, report construction, and JSON I/O excluded unless the named operation is setup/open"
            .to_string(),
        network_scope: "all cases are deterministic and local; AutoMix uses the generated PCM WAV and performs no HTTP request"
            .to_string(),
    };

    let mut report = ComponentReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: PROBE.to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment: BenchEnvironment::capture(),
        conditions,
        cases,
        baseline: None,
        comparisons: Vec::new(),
    };

    if let Some(path) = args.baseline.as_deref() {
        let baseline: ComponentReport = read_json(path, "component baseline report")?;
        validate_performance_baseline(
            "component",
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
                .map(|case| (case.case_key.clone(), case.distribution.median)),
            baseline
                .cases
                .iter()
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
        write_json_round_trip(path, &report, "component performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }
    Ok(())
}

fn workload(mode: BenchMode) -> ComponentWorkload {
    match mode {
        BenchMode::Quick => ComponentWorkload {
            trials: 7,
            iteration_scale: 1,
            automix_trials: 3,
        },
        BenchMode::Full => ComponentWorkload {
            trials: 15,
            iteration_scale: 4,
            automix_trials: 7,
        },
        BenchMode::Heavy => ComponentWorkload {
            trials: 31,
            iteration_scale: 16,
            automix_trials: 15,
        },
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_component_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>]\n\
         Measures SpectrumAnalyzer, Downmixer, LoudnessMeter, TruePeakDetector, AutoMix, RingBuffer, and feature-gated in-memory LoudnessDatabase operations.\n\
         Shared-runner timing is report-only without a compatible same-machine baseline."
    );
}

fn benchmark_spectrum(
    fft_size: usize,
    bins: usize,
    iterations: usize,
    trials: usize,
) -> Result<ComponentCase, String> {
    let input = synthetic_samples(fft_size, 1);
    let mut samples = Vec::with_capacity(trials);
    let mut checksum = 0.0;
    for _ in 0..trials {
        let mut analyzer =
            SpectrumAnalyzer::new(fft_size, bins).map_err(|error| error.to_string())?;
        black_box(
            analyzer
                .analyze(&input, SAMPLE_RATE_HZ)
                .map_err(|error| error.to_string())?,
        );
        let start = Instant::now();
        for iteration in 0..iterations {
            let output = analyzer
                .analyze(black_box(&input), SAMPLE_RATE_HZ)
                .map_err(|error| error.to_string())?;
            checksum += f64::from(output[iteration % output.len()]);
            black_box(output);
        }
        samples.push(ns_per_work(start, iterations * fft_size));
    }
    let mut validation_analyzer =
        SpectrumAnalyzer::new(fft_size, bins).map_err(|error| error.to_string())?;
    let validation = validation_analyzer
        .analyze(&input, SAMPLE_RATE_HZ)
        .map_err(|error| error.to_string())?;
    let finite = validation.iter().all(|value| value.is_finite());
    let nontrivial = validation.iter().any(|value| *value > 0.0);
    component_case(ComponentCaseInput {
        case_key: format!("component=spectrum;operation=analyze;fft={fft_size};bins={bins}"),
        component: "SpectrumAnalyzer",
        operation: "analyze",
        primary_unit: "ns/input-sample",
        work_items_per_iteration: fft_size,
        iterations_per_trial: iterations,
        samples,
        expected_operations: trials * iterations,
        expected_work_items: trials * iterations * fft_size,
        all_output_finite: finite,
        output_nontrivial: nontrivial,
        checksum: checksum
            + validation
                .iter()
                .map(|value| f64::from(*value))
                .sum::<f64>(),
    })
}

fn benchmark_downmix(
    source_layout: ChannelLayout,
    coefficients: DownmixCoefficients,
    coefficient_name: &str,
    frames: usize,
    iterations: usize,
    trials: usize,
) -> Result<ComponentCase, String> {
    let source_channels = source_layout.channel_count();
    let input = synthetic_samples(frames, source_channels);
    let mut samples = Vec::with_capacity(trials);
    let mut checksum = 0.0;
    for _ in 0..trials {
        let downmixer =
            Downmixer::new(source_layout.clone(), ChannelLayout::stereo(), coefficients)
                .map_err(|error| format!("downmixer setup failed: {error}"))?;
        let mut output = vec![0.0; frames * 2];
        let start = Instant::now();
        for _ in 0..iterations {
            let produced = downmixer
                .process_into(black_box(&input), black_box(&mut output))
                .map_err(|error| format!("timed downmix failed: {error}"))?;
            checksum += output[(produced * 2 - 1).min(output.len() - 1)];
            black_box(&output);
        }
        samples.push(ns_per_work(start, iterations * frames));
    }
    let downmixer = Downmixer::new(source_layout, ChannelLayout::stereo(), coefficients)
        .map_err(|error| format!("downmixer validation setup failed: {error}"))?;
    let mut output = vec![0.0; frames * 2];
    let produced = downmixer
        .process_into(&input, &mut output)
        .map_err(|error| format!("downmixer validation failed: {error}"))?;
    component_case(ComponentCaseInput {
        case_key: format!(
            "component=downmixer;operation=process_into;source_channels={source_channels};target_channels=2;coefficients={coefficient_name};frames={frames}"
        ),
        component: "Downmixer",
        operation: "process_into",
        primary_unit: "ns/frame",
        work_items_per_iteration: frames,
        iterations_per_trial: iterations,
        samples,
        expected_operations: trials * iterations,
        expected_work_items: trials * iterations * frames,
        all_output_finite: output.iter().all(|value| value.is_finite()),
        output_nontrivial: produced == frames
            && output.iter().any(|value| value.abs() > 1.0e-12),
        checksum: checksum + output.iter().sum::<f64>(),
    })
}

fn benchmark_loudness(
    frames: usize,
    iterations: usize,
    trials: usize,
) -> Result<ComponentCase, String> {
    let channels = 2;
    let input = synthetic_samples(frames, channels);
    let mut samples = Vec::with_capacity(trials);
    let mut checksum = 0.0;
    for _ in 0..trials {
        let mut meter = LoudnessMeter::new(channels, SAMPLE_RATE_HZ);
        meter.process(&input);
        let before = meter.samples_processed();
        let start = Instant::now();
        for _ in 0..iterations {
            meter.process(black_box(&input));
        }
        samples.push(ns_per_work(start, iterations * input.len()));
        let expected_frames = frames as u64 * iterations as u64;
        if meter.samples_processed().saturating_sub(before) != expected_frames {
            return Err("loudness meter processed-frame count changed during timing".to_string());
        }
        checksum += meter.integrated_loudness() + meter.true_peak();
        black_box(&meter);
    }
    let mut validation_meter = LoudnessMeter::new(channels, SAMPLE_RATE_HZ);
    for _ in 0..48 {
        validation_meter.process(&input);
    }
    let validation_values = [
        validation_meter.integrated_loudness(),
        validation_meter.momentary_loudness(),
        validation_meter.short_term_loudness(),
        validation_meter.loudness_range(),
        validation_meter.true_peak(),
    ];
    component_case(ComponentCaseInput {
        case_key: format!(
            "component=loudness_meter;operation=process;channels={channels};frames={frames};true_peak=4x_fir"
        ),
        component: "LoudnessMeter",
        operation: "process",
        primary_unit: "ns/input-sample",
        work_items_per_iteration: input.len(),
        iterations_per_trial: iterations,
        samples,
        expected_operations: trials * iterations,
        expected_work_items: trials * iterations * input.len(),
        all_output_finite: validation_values.iter().all(|value| value.is_finite()),
        output_nontrivial: validation_meter.samples_processed() > 0
            && validation_meter.true_peak() > -70.0,
        checksum: checksum + validation_values.iter().sum::<f64>(),
    })
}

fn benchmark_true_peak_contiguous(
    samples_per_iteration: usize,
    iterations: usize,
    trials: usize,
) -> Result<ComponentCase, String> {
    let input = synthetic_samples(samples_per_iteration, 1);
    let mut samples = Vec::with_capacity(trials);
    let mut checksum = 0.0;
    for _ in 0..trials {
        let mut detector = TruePeakDetector::new();
        detector.process(&input);
        detector.reset();
        let start = Instant::now();
        for _ in 0..iterations {
            detector.process(black_box(&input));
        }
        samples.push(ns_per_work(start, iterations * input.len()));
        checksum += detector.max_true_peak();
    }
    component_case(ComponentCaseInput {
        case_key: format!(
            "component=true_peak;operation=process_contiguous;samples={samples_per_iteration};oversample=4x"
        ),
        component: "TruePeakDetector",
        operation: "process",
        primary_unit: "ns/input-sample",
        work_items_per_iteration: input.len(),
        iterations_per_trial: iterations,
        samples,
        expected_operations: trials * iterations,
        expected_work_items: trials * iterations * input.len(),
        all_output_finite: checksum.is_finite(),
        output_nontrivial: checksum > 0.0,
        checksum,
    })
}

fn benchmark_true_peak_strided(
    frames: usize,
    iterations: usize,
    trials: usize,
) -> Result<ComponentCase, String> {
    let input = synthetic_samples(frames, 2);
    let mut samples = Vec::with_capacity(trials);
    let mut checksum = 0.0;
    for _ in 0..trials {
        let mut detector = TruePeakDetector::new();
        detector.process_strided(&input, 0, 2);
        detector.reset();
        let start = Instant::now();
        for iteration in 0..iterations {
            detector.process_strided(black_box(&input), iteration & 1, 2);
        }
        samples.push(ns_per_work(start, iterations * frames));
        checksum += detector.max_true_peak();
    }
    component_case(ComponentCaseInput {
        case_key: format!(
            "component=true_peak;operation=process_strided;channels=2;frames={frames};oversample=4x"
        ),
        component: "TruePeakDetector",
        operation: "process_strided",
        primary_unit: "ns/channel-sample",
        work_items_per_iteration: frames,
        iterations_per_trial: iterations,
        samples,
        expected_operations: trials * iterations,
        expected_work_items: trials * iterations * frames,
        all_output_finite: checksum.is_finite(),
        output_nontrivial: checksum > 0.0,
        checksum,
    })
}

fn benchmark_automix(
    fixture_path: &str,
    mode: AutomixAnalysisMode,
    trials: usize,
) -> Result<ComponentCase, String> {
    let mode_name = match mode {
        AutomixAnalysisMode::Head => "head",
        AutomixAnalysisMode::Full => "full",
    };
    let options = AutomixAnalysisOptions {
        mode,
        max_analyze_time_sec: 5.0,
    };
    let mut samples = Vec::with_capacity(trials);
    let mut checksum = 0.0;
    let mut valid = true;
    for _ in 0..trials {
        let start = Instant::now();
        let analysis =
            analyze_automix(MediaLocation::local(fixture_path), None, options.clone())
                .map_err(|error| format!("timed AutoMix {mode_name} analysis failed: {error}"))?;
        samples.push(ns_per_work(start, 1));
        valid &= analysis.version == 3
            && analysis.mode == mode
            && analysis.duration > 0.0
            && !analysis.energy_profile.is_empty()
            && analysis.mix_center_pos.is_finite();
        checksum += analysis.duration
            + analysis.energy_profile.iter().sum::<f64>()
            + analysis.bpm.unwrap_or(0.0)
            + analysis.loudness.unwrap_or(-70.0);
        black_box(analysis);
    }
    component_case(ComponentCaseInput {
        case_key: format!(
            "component=automix;operation=analyze;mode={mode_name};window_seconds=5;fixture=pcm16_48k_stereo"
        ),
        component: "AutoMix",
        operation: "analyze_automix",
        primary_unit: "ns/analysis",
        work_items_per_iteration: 1,
        iterations_per_trial: 1,
        samples,
        expected_operations: trials,
        expected_work_items: trials,
        all_output_finite: checksum.is_finite(),
        output_nontrivial: valid,
        checksum,
    })
}

fn benchmark_ring_buffer(
    frames: usize,
    iterations: usize,
    trials: usize,
) -> Result<ComponentCase, String> {
    let channels = 2;
    let input = synthetic_samples(frames, channels);
    let mut samples = Vec::with_capacity(trials);
    let mut checksum = 0.0;
    let mut valid = true;
    for _ in 0..trials {
        let mut ring = RingBuffer::new(8_192, channels);
        let mut output = vec![0.0; input.len()];
        let mut read_pos = 0u64;
        let start = Instant::now();
        for _ in 0..iterations {
            let (written, overflow) = ring.write(black_box(&input));
            let read = ring.read(read_pos, black_box(&mut output));
            ring.advance_read_pos(read as u64);
            read_pos = read_pos.saturating_add(read as u64);
            valid &= written == frames && read == frames && overflow.is_none();
            checksum += output[output.len() / 2];
            black_box(&output);
        }
        samples.push(ns_per_work(start, iterations * frames));
        valid &= ring.overflow_count() == 0
            && ring.total_written() == (iterations * frames) as u64
            && output == input;
    }
    component_case(ComponentCaseInput {
        case_key: format!(
            "component=ring_buffer;operation=write_read_advance;channels={channels};frames={frames};capacity_frames=8192"
        ),
        component: "RingBuffer",
        operation: "write_read_advance",
        primary_unit: "ns/frame-roundtrip",
        work_items_per_iteration: frames,
        iterations_per_trial: iterations,
        samples,
        expected_operations: trials * iterations,
        expected_work_items: trials * iterations * frames,
        all_output_finite: input.iter().all(|value| value.is_finite()),
        output_nontrivial: valid,
        checksum,
    })
}

#[cfg(feature = "loudness-db")]
fn report_database<T>(result: Result<T, LoudnessDatabaseError>) -> Result<T, String> {
    result.map_err(|error| error.to_string())
}

#[cfg(feature = "loudness-db")]
fn benchmark_loudness_database(workload: ComponentWorkload) -> Result<Vec<ComponentCase>, String> {
    let rows = 512usize;
    let records = database_records(rows);
    let operation_iterations = 128 * workload.iteration_scale;
    let mut cases = Vec::new();

    let mut open_samples = Vec::with_capacity(workload.trials);
    for _ in 0..workload.trials {
        let start = Instant::now();
        let database = report_database(LoudnessDatabase::in_memory())?;
        open_samples.push(ns_per_work(start, 1));
        black_box(database);
    }
    cases.push(component_case(ComponentCaseInput {
        case_key: "component=loudness_database;operation=open;storage=in_memory".to_string(),
        component: "LoudnessDatabase",
        operation: "in_memory",
        primary_unit: "ns/open",
        work_items_per_iteration: 1,
        iterations_per_trial: 1,
        samples: open_samples,
        expected_operations: workload.trials,
        expected_work_items: workload.trials,
        all_output_finite: true,
        output_nontrivial: true,
        checksum: workload.trials as f64,
    })?);

    let mut upsert_samples = Vec::with_capacity(workload.trials);
    let mut upsert_checksum = 0.0;
    for _ in 0..workload.trials {
        let database = report_database(LoudnessDatabase::in_memory())?;
        let start = Instant::now();
        for iteration in 0..operation_iterations {
            report_database(database.upsert(&records[iteration % records.len()]))?;
        }
        upsert_samples.push(ns_per_work(start, operation_iterations));
        upsert_checksum += report_database(database.stats())?.total_tracks as f64;
    }
    cases.push(component_case(ComponentCaseInput {
        case_key: format!(
            "component=loudness_database;operation=single_upsert;storage=in_memory;working_set_rows={rows}"
        ),
        component: "LoudnessDatabase",
        operation: "upsert",
        primary_unit: "ns/row",
        work_items_per_iteration: 1,
        iterations_per_trial: operation_iterations,
        samples: upsert_samples,
        expected_operations: workload.trials * operation_iterations,
        expected_work_items: workload.trials * operation_iterations,
        all_output_finite: upsert_checksum.is_finite(),
        output_nontrivial: upsert_checksum > 0.0,
        checksum: upsert_checksum,
    })?);

    let mut get_samples = Vec::with_capacity(workload.trials);
    let mut get_checksum = 0.0;
    for _ in 0..workload.trials {
        let database = report_database(LoudnessDatabase::in_memory())?;
        report_database(database.batch_upsert(&records))?;
        let start = Instant::now();
        for iteration in 0..operation_iterations {
            let record = report_database(database.get(&records[iteration % records.len()].source))?
                .ok_or_else(|| "seeded LoudnessDatabase row was not found".to_string())?;
            get_checksum += record.integrated_lufs;
            black_box(record);
        }
        get_samples.push(ns_per_work(start, operation_iterations));
    }
    cases.push(component_case(ComponentCaseInput {
        case_key: format!(
            "component=loudness_database;operation=indexed_get;storage=in_memory;rows={rows}"
        ),
        component: "LoudnessDatabase",
        operation: "get",
        primary_unit: "ns/get",
        work_items_per_iteration: 1,
        iterations_per_trial: operation_iterations,
        samples: get_samples,
        expected_operations: workload.trials * operation_iterations,
        expected_work_items: workload.trials * operation_iterations,
        all_output_finite: get_checksum.is_finite(),
        output_nontrivial: get_checksum != 0.0,
        checksum: get_checksum,
    })?);

    let batch_rows = 128usize;
    let mut batch_samples = Vec::with_capacity(workload.trials);
    let mut batch_checksum = 0.0;
    for _ in 0..workload.trials {
        let database = report_database(LoudnessDatabase::in_memory())?;
        let start = Instant::now();
        let inserted = report_database(database.batch_upsert(&records[..batch_rows]))?;
        batch_samples.push(ns_per_work(start, batch_rows));
        batch_checksum += inserted as f64;
    }
    cases.push(component_case(ComponentCaseInput {
        case_key: format!(
            "component=loudness_database;operation=batch_upsert;storage=in_memory;rows={batch_rows}"
        ),
        component: "LoudnessDatabase",
        operation: "batch_upsert",
        primary_unit: "ns/row",
        work_items_per_iteration: batch_rows,
        iterations_per_trial: 1,
        samples: batch_samples,
        expected_operations: workload.trials,
        expected_work_items: workload.trials * batch_rows,
        all_output_finite: batch_checksum.is_finite(),
        output_nontrivial: batch_checksum == (workload.trials * batch_rows) as f64,
        checksum: batch_checksum,
    })?);

    let mut stats_samples = Vec::with_capacity(workload.trials);
    let mut stats_checksum = 0.0;
    for _ in 0..workload.trials {
        let database = report_database(LoudnessDatabase::in_memory())?;
        report_database(database.batch_upsert(&records))?;
        let start = Instant::now();
        for _ in 0..operation_iterations {
            let stats = report_database(database.stats())?;
            stats_checksum += stats.total_tracks as f64;
            black_box(stats);
        }
        stats_samples.push(ns_per_work(start, operation_iterations));
    }
    cases.push(component_case(ComponentCaseInput {
        case_key: format!(
            "component=loudness_database;operation=stats;storage=in_memory;rows={rows}"
        ),
        component: "LoudnessDatabase",
        operation: "stats",
        primary_unit: "ns/stats",
        work_items_per_iteration: 1,
        iterations_per_trial: operation_iterations,
        samples: stats_samples,
        expected_operations: workload.trials * operation_iterations,
        expected_work_items: workload.trials * operation_iterations,
        all_output_finite: stats_checksum.is_finite(),
        output_nontrivial: stats_checksum > 0.0,
        checksum: stats_checksum,
    })?);

    Ok(cases)
}

#[cfg(feature = "loudness-db")]
fn database_records(rows: usize) -> Vec<TrackLoudness> {
    (0..rows)
        .map(|index| {
            let location =
                MediaLocation::http(format!("https://benchmark.invalid/audio/{index:05}.flac"))
                    .expect("benchmark URL must be valid");
            TrackLoudness::new(
                &location,
                -24.0 + (index % 11) as f64 * 0.5,
                -3.0 + (index % 5) as f64 * 0.1,
                Some(4.0 + (index % 7) as f64 * 0.2),
                -14.0,
            )
        })
        .collect()
}

fn component_case(input: ComponentCaseInput) -> Result<ComponentCase, String> {
    if input.iterations_per_trial == 0
        || !input
            .expected_operations
            .is_multiple_of(input.iterations_per_trial)
    {
        return Err(format!(
            "component case '{}' has inconsistent expected operation geometry",
            input.case_key
        ));
    }
    let expected_timing_samples = input.expected_operations / input.iterations_per_trial;
    let observed_trials = input.samples.len();
    let observed_operations = observed_trials.saturating_mul(input.iterations_per_trial);
    let observed_work_items = observed_operations.saturating_mul(input.work_items_per_iteration);
    Ok(ComponentCase {
        case_key: input.case_key,
        component: input.component.to_string(),
        operation: input.operation.to_string(),
        primary_unit: input.primary_unit.to_string(),
        work_items_per_iteration: input.work_items_per_iteration,
        iterations_per_trial: input.iterations_per_trial,
        expected_timing_samples,
        distribution: summarize_trials(input.samples)?,
        work_validation: ComponentWorkValidation {
            valid: observed_operations == input.expected_operations
                && observed_work_items == input.expected_work_items
                && input.all_output_finite
                && input.output_nontrivial
                && input.checksum.is_finite(),
            observed_operations,
            expected_operations: input.expected_operations,
            observed_work_items: observed_work_items as u64,
            expected_work_items: input.expected_work_items as u64,
            all_output_finite: input.all_output_finite,
            output_nontrivial: input.output_nontrivial,
            checksum: input.checksum,
        },
    })
}

fn ns_per_work(start: Instant, work_items: usize) -> f64 {
    start.elapsed().as_nanos().max(1) as f64 / work_items.max(1) as f64
}

fn synthetic_samples(frames: usize, channels: usize) -> Vec<f64> {
    let mut output = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        let time = frame as f64 / SAMPLE_RATE_HZ as f64;
        for channel in 0..channels {
            let frequency = 173.0 + channel as f64 * 97.0;
            let carrier = (std::f64::consts::TAU * frequency * time).sin();
            let modulation = (std::f64::consts::TAU * (2.0 + channel as f64) * time).cos();
            output.push((carrier * 0.42 + modulation * 0.08).clamp(-0.9, 0.9));
        }
    }
    output
}

#[cfg(feature = "loudness-db")]
fn loudness_database_scope() -> String {
    "measured: SQLite in-memory open, single upsert, indexed get, batch upsert, and stats; no filesystem persistence"
        .to_string()
}

fn expected_component_case_keys() -> Vec<String> {
    let keys = vec![
        "component=spectrum;operation=analyze;fft=1024;bins=64".to_string(),
        "component=spectrum;operation=analyze;fft=4096;bins=96".to_string(),
        "component=downmixer;operation=process_into;source_channels=6;target_channels=2;coefficients=itu_r_bs775;frames=512".to_string(),
        "component=downmixer;operation=process_into;source_channels=8;target_channels=2;coefficients=atsc_a85;frames=512".to_string(),
        "component=loudness_meter;operation=process;channels=2;frames=512;true_peak=4x_fir".to_string(),
        "component=loudness_meter;operation=process;channels=2;frames=4096;true_peak=4x_fir".to_string(),
        "component=true_peak;operation=process_contiguous;samples=4096;oversample=4x".to_string(),
        "component=true_peak;operation=process_strided;channels=2;frames=4096;oversample=4x".to_string(),
        "component=automix;operation=analyze;mode=head;window_seconds=5;fixture=pcm16_48k_stereo".to_string(),
        "component=automix;operation=analyze;mode=full;window_seconds=5;fixture=pcm16_48k_stereo".to_string(),
        "component=ring_buffer;operation=write_read_advance;channels=2;frames=512;capacity_frames=8192".to_string(),
    ];
    #[cfg(feature = "loudness-db")]
    let keys = {
        let mut keys = keys;
        keys.extend([
            "component=loudness_database;operation=open;storage=in_memory".to_string(),
            "component=loudness_database;operation=single_upsert;storage=in_memory;working_set_rows=512".to_string(),
            "component=loudness_database;operation=indexed_get;storage=in_memory;rows=512"
                .to_string(),
            "component=loudness_database;operation=batch_upsert;storage=in_memory;rows=128"
                .to_string(),
            "component=loudness_database;operation=stats;storage=in_memory;rows=512".to_string(),
        ]);
        keys
    };
    keys
}

#[cfg(not(feature = "loudness-db"))]
fn loudness_database_scope() -> String {
    "excluded: crate compiled without the optional loudness-db feature".to_string()
}

fn print_report(report: &ComponentReport) -> Result<(), String> {
    println!(
        "audio_component_perf mode={} cases={} database_scope={} environment={}",
        report.mode.as_str(),
        report.cases.len(),
        report.conditions.loudness_database_scope,
        environment_json(&report.environment)?
    );
    for case in &report.cases {
        println!(
            "component case={} unit={} median={:.3} p95={:.3} max={:.3} trials={} valid={}",
            case.case_key,
            case.primary_unit,
            case.distribution.median,
            case.distribution.p95,
            case.distribution.max,
            case.distribution.samples.len(),
            case.work_validation.valid
        );
    }
    Ok(())
}

fn enforce_report(report: &ComponentReport) -> Result<(), String> {
    validate_case_key_set(
        report.cases.iter().map(|case| case.case_key.clone()),
        expected_component_case_keys(),
        "component",
    )?;
    let invalid = report
        .cases
        .iter()
        .filter(|case| {
            !case.work_validation.valid
                || case.distribution.samples.len() != case.expected_timing_samples
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
            "component work/report integrity gate failed for cases: {}",
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
        return Err("component condition case keys differ from measured cases".to_string());
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "component median regression gate failed",
        "primary-unit",
    ) {
        return Err(error);
    }
    Ok(())
}
