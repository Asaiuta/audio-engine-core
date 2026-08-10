use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

use audio_engine_core::decoder::OpenedMediaSource;
use audio_engine_core::{MediaLocation, StreamingDecoder};
use serde::{Deserialize, Serialize};

pub mod support;

use support::allocation::{AllocationScope, AllocationSnapshot};
use support::audio_fixture::{
    ensure_deterministic_pcm_fixture, fixture_path_display, DeterministicPcmFixture,
    DeterministicPcmFixtureMetadata,
};
use support::{
    compare_case_medians, environment_json, generated_unix_ms, read_json, regression_gate_error,
    summarize_callback_samples, summarize_trials, validate_case_key_set,
    validate_performance_baseline, write_json_round_trip, BenchEnvironment, BenchMode,
    CallbackTailDistribution, PerfArgs, PerformanceReportIdentity, RegressionComparison,
    TrialDistribution, REPORT_SCHEMA_VERSION,
};

const PROBE: &str = "audio_decoder_startup_streaming_perf";
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const EXPECTED_CASE_KEYS: [&str; 7] = [
    "phase=source_open;container=wav;source=local",
    "phase=probe;container=wav;codec=pcm_s16le",
    "phase=decoder_build;container=wav;codec=pcm_s16le",
    "phase=first_borrowed_pcm;container=wav;codec=pcm_s16le",
    "phase=steady_borrowed_decode;container=wav;codec=pcm_s16le",
    "phase=seek_command;mode=coarse;container=wav;codec=pcm_s16le",
    "phase=seek_to_first_pcm;mode=coarse;container=wav;codec=pcm_s16le",
];

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct DecoderConditions {
    fixture_path: String,
    fixture: DeterministicPcmFixtureMetadata,
    ordinary_samples_per_case: usize,
    ordinary_iterations_per_sample: usize,
    seek_samples_per_case: usize,
    warmups: usize,
    seek_targets_seconds: Vec<f64>,
    source_cache_state: String,
    timer_scope: String,
    allocator_scope: String,
    crate_staging_scope: String,
    opaque_allocation_disclosure: String,
    network_scope: String,
    validated_sample_rate_hz: u32,
    validated_channels: usize,
    validated_decoded_frames: u64,
    validated_decoded_pcm_fnv1a64: String,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct DecoderWorkValidation {
    valid: bool,
    observed_operations: usize,
    expected_operations: usize,
    decoded_frames: u64,
    expected_decoded_frames: u64,
    first_packet_samples: usize,
    packet_count: usize,
    all_samples_finite: bool,
    output_fnv1a64: String,
    maximum_seek_error_frames: u64,
    sample_rate_hz: u32,
    channels: usize,
    crate_staging_bytes: usize,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct DecoderTimingCase {
    case_key: String,
    phase: String,
    primary_unit: String,
    distribution: CallbackTailDistribution,
    throughput_frames_per_second: Option<TrialDistribution>,
    realtime_factor: Option<TrialDistribution>,
    expected_timing_samples: usize,
    iterations_per_timing_sample: usize,
    work_validation: DecoderWorkValidation,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct DecoderAllocationEvidence {
    phase: String,
    allocation_calls: usize,
    deallocation_calls: usize,
    reallocation_calls: usize,
    peak_live_bytes: usize,
    retained_bytes: usize,
    crate_staging_bytes: usize,
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
struct DecoderReport {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: DecoderConditions,
    cases: Vec<DecoderTimingCase>,
    allocation_evidence: Vec<DecoderAllocationEvidence>,
    baseline: Option<BaselineReference>,
    comparisons: Vec<RegressionComparison>,
}

struct SteadyTrial {
    ns_per_frame: f64,
    frames_per_second: f64,
    realtime_factor: f64,
    decoded_frames: u64,
    packets: usize,
    all_finite: bool,
    hash: u64,
    sample_rate_hz: u32,
    channels: usize,
    staging_bytes: usize,
}

fn main() -> Result<(), String> {
    let args = PerfArgs::parse(std::env::args().skip(1).collect())?;
    if args.help {
        print_help();
        return Ok(());
    }

    let fixture = ensure_deterministic_pcm_fixture()?;
    let fixture_validation = validate_fixture(&fixture)?;
    let (ordinary_samples, seek_samples, warmups, ordinary_iterations) = workload(args.mode);
    let seek_targets = vec![1.0, 5.0, 9.0, 2.5, 7.5, 10.5];
    let conditions = DecoderConditions {
        fixture_path: fixture_path_display(&fixture.path),
        fixture: fixture.metadata.clone(),
        ordinary_samples_per_case: ordinary_samples,
        ordinary_iterations_per_sample: ordinary_iterations,
        seek_samples_per_case: seek_samples,
        warmups,
        seek_targets_seconds: seek_targets.clone(),
        source_cache_state: "warm local filesystem cache after untimed fixture validation and warmups"
            .to_string(),
        timer_scope: "each phase excludes fixture generation, validation, warmup, object drop, report construction, and JSON I/O; open/probe/build/first-PCM raw trials are averages of the declared repeated phase operations"
            .to_string(),
        allocator_scope: "Rust global allocator calls only; phase objects remain alive until the allocation snapshot is captured"
            .to_string(),
        crate_staging_scope: "StreamingDecoder fixed interleaved f64 staging capacity reported exactly"
            .to_string(),
        opaque_allocation_disclosure: "Symphonia internals and any native/system allocations not routed through the Rust global allocator are not separately attributable"
            .to_string(),
        network_scope: "deterministic local RIFF/WAVE only; no HTTP request or live network service"
            .to_string(),
        validated_sample_rate_hz: fixture_validation.sample_rate_hz,
        validated_channels: fixture_validation.channels,
        validated_decoded_frames: fixture_validation.decoded_frames,
        validated_decoded_pcm_fnv1a64: fixture_validation.output_fnv1a64,
    };

    warm_decoder_phases(&fixture.path, warmups, &seek_targets)?;

    let mut cases = vec![
        benchmark_source_open(&fixture, ordinary_samples, ordinary_iterations)?,
        benchmark_probe(&fixture, ordinary_samples, ordinary_iterations)?,
        benchmark_build(&fixture, ordinary_samples, ordinary_iterations)?,
        benchmark_first_pcm(&fixture, ordinary_samples, ordinary_iterations)?,
        benchmark_steady_decode(&fixture, ordinary_samples)?,
        benchmark_seek_command(&fixture, seek_samples, &seek_targets)?,
        benchmark_seek_to_first_pcm(&fixture, seek_samples, &seek_targets)?,
    ];
    cases.sort_by(|left, right| left.case_key.cmp(&right.case_key));

    let allocation_evidence = measure_allocations(&fixture)?;
    let mut report = DecoderReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: PROBE.to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment: BenchEnvironment::capture(),
        conditions,
        cases,
        allocation_evidence,
        baseline: None,
        comparisons: Vec::new(),
    };

    if let Some(path) = args.baseline.as_deref() {
        let baseline: DecoderReport = read_json(path, "decoder baseline report")?;
        validate_performance_baseline(
            "decoder",
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
        write_json_round_trip(path, &report, "decoder performance report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }
    Ok(())
}

fn workload(mode: BenchMode) -> (usize, usize, usize, usize) {
    match mode {
        BenchMode::Quick => (9, 24, 1, 16),
        BenchMode::Full => (21, 96, 2, 32),
        BenchMode::Heavy => (61, 480, 3, 64),
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_decoder_perf -- [--quick|--heavy] [--enforce] [--out <json>] [--baseline <json>] [--max-median-regression-pct <pct>]\n\
         Measures deterministic local source open, probe, decoder build, first borrowed PCM, steady borrowed decode, and seek latency.\n\
         Rust allocation accounting excludes opaque Symphonia/native allocations; quick mode performs no network I/O."
    );
}

fn validate_fixture(fixture: &DeterministicPcmFixture) -> Result<DecoderWorkValidation, String> {
    let validation = decode_full(&fixture.path)?;
    let repeated = decode_full(&fixture.path)?;
    if validation != repeated {
        return Err(format!(
            "deterministic decoder fixture changed across repeated full decodes: first={validation:?}, repeated={repeated:?}"
        ));
    }
    if validation.sample_rate_hz != fixture.metadata.sample_rate_hz
        || validation.channels != fixture.metadata.channels
        || validation.decoded_frames != fixture.metadata.frames
        || !validation.all_samples_finite
        || validation.packet_count == 0
        || validation.first_packet_samples == 0
        || validation.output_fnv1a64.is_empty()
    {
        return Err(format!(
            "deterministic decoder fixture validation failed: rate={}/{}, channels={}/{}, frames={}/{}, packets={}, first_packet_samples={}, finite={}, hash={}",
            validation.sample_rate_hz,
            fixture.metadata.sample_rate_hz,
            validation.channels,
            fixture.metadata.channels,
            validation.decoded_frames,
            fixture.metadata.frames,
            validation.packet_count,
            validation.first_packet_samples,
            validation.all_samples_finite,
            validation.output_fnv1a64
        ));
    }
    Ok(validation)
}

fn warm_decoder_phases(path: &Path, warmups: usize, seek_targets: &[f64]) -> Result<(), String> {
    for index in 0..warmups {
        let source = OpenedMediaSource::open_local(path, None)
            .map_err(|error| format!("decoder warmup source open failed: {error}"))?;
        let builder = StreamingDecoder::probe_opened_source(source, None)
            .map_err(|error| format!("decoder warmup probe failed: {error}"))?;
        let mut decoder = builder
            .build()
            .map_err(|error| format!("decoder warmup build failed: {error}"))?;
        let _ = decoder
            .decode_next_borrowed()
            .map_err(|error| format!("decoder warmup first PCM failed: {error}"))?;
        decoder
            .seek(seek_targets[index % seek_targets.len()])
            .map_err(|error| format!("decoder warmup seek failed: {error}"))?;
        let _ = decoder
            .decode_next_borrowed()
            .map_err(|error| format!("decoder warmup post-seek PCM failed: {error}"))?;
        black_box(decoder);
    }
    Ok(())
}

fn benchmark_source_open(
    fixture: &DeterministicPcmFixture,
    samples: usize,
    iterations_per_sample: usize,
) -> Result<DecoderTimingCase, String> {
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut total_ns = 0.0;
        for _ in 0..iterations_per_sample {
            let start = Instant::now();
            let source = OpenedMediaSource::open_local(&fixture.path, None)
                .map_err(|error| format!("timed local source open failed: {error}"))?;
            total_ns += elapsed_ns(start);
            black_box(source);
        }
        timings.push(total_ns / iterations_per_sample as f64);
    }
    simple_case(
        "phase=source_open;container=wav;source=local",
        "local_source_open",
        "ns/open",
        timings,
        samples,
        iterations_per_sample,
    )
}

fn benchmark_probe(
    fixture: &DeterministicPcmFixture,
    samples: usize,
    iterations_per_sample: usize,
) -> Result<DecoderTimingCase, String> {
    let mut timings = Vec::with_capacity(samples);
    let mut staging_bytes = None;
    for _ in 0..samples {
        let mut total_ns = 0.0;
        for _ in 0..iterations_per_sample {
            let source = OpenedMediaSource::open_local(&fixture.path, None)
                .map_err(|error| format!("probe source open failed: {error}"))?;
            let start = Instant::now();
            let builder = StreamingDecoder::probe_opened_source(source, None)
                .map_err(|error| format!("timed decoder probe failed: {error}"))?;
            total_ns += elapsed_ns(start);
            staging_bytes = Some(
                builder
                    .staging_buffer_bytes()
                    .map_err(|error| format!("staging byte query failed: {error}"))?,
            );
            black_box(builder);
        }
        timings.push(total_ns / iterations_per_sample as f64);
    }
    let mut case = simple_case(
        "phase=probe;container=wav;codec=pcm_s16le",
        "container_probe",
        "ns/probe",
        timings,
        samples,
        iterations_per_sample,
    )?;
    case.work_validation.crate_staging_bytes = staging_bytes.unwrap_or(0);
    case.work_validation.valid &= staging_bytes.is_some_and(|bytes| bytes > 0);
    Ok(case)
}

fn benchmark_build(
    fixture: &DeterministicPcmFixture,
    samples: usize,
    iterations_per_sample: usize,
) -> Result<DecoderTimingCase, String> {
    let mut timings = Vec::with_capacity(samples);
    let mut staging_bytes = None;
    for _ in 0..samples {
        let mut total_ns = 0.0;
        for _ in 0..iterations_per_sample {
            let builder = open_builder(&fixture.path)?;
            let start = Instant::now();
            let decoder = builder
                .build()
                .map_err(|error| format!("timed decoder build failed: {error}"))?;
            total_ns += elapsed_ns(start);
            let observed_staging = decoder.staging_buffer_bytes();
            if observed_staging == 0 {
                return Err("decoder build produced zero staging bytes".to_string());
            }
            check_stable(
                &mut staging_bytes,
                observed_staging,
                "decoder build staging bytes",
            )?;
            black_box(decoder);
        }
        timings.push(total_ns / iterations_per_sample as f64);
    }
    let mut case = simple_case(
        "phase=decoder_build;container=wav;codec=pcm_s16le",
        "decoder_build",
        "ns/build",
        timings,
        samples,
        iterations_per_sample,
    )?;
    case.work_validation.crate_staging_bytes = staging_bytes.unwrap_or(0);
    Ok(case)
}

fn benchmark_first_pcm(
    fixture: &DeterministicPcmFixture,
    samples: usize,
    iterations_per_sample: usize,
) -> Result<DecoderTimingCase, String> {
    let mut timings = Vec::with_capacity(samples);
    let mut first_samples = 0usize;
    let mut all_finite = true;
    let mut staging_bytes = None;
    for _ in 0..samples {
        let mut total_ns = 0.0;
        for _ in 0..iterations_per_sample {
            let mut decoder = StreamingDecoder::open(MediaLocation::local(fixture.path.clone()))
                .map_err(|error| format!("first PCM decoder open failed: {error}"))?;
            check_stable(
                &mut staging_bytes,
                decoder.staging_buffer_bytes(),
                "first PCM staging bytes",
            )?;
            let start = Instant::now();
            let pcm = decoder
                .decode_next_borrowed()
                .map_err(|error| format!("timed first borrowed PCM failed: {error}"))?
                .ok_or_else(|| {
                    "first borrowed PCM unexpectedly reached end of stream".to_string()
                })?;
            total_ns += elapsed_ns(start);
            first_samples = pcm.len();
            all_finite &= pcm.iter().all(|sample| sample.is_finite());
            black_box(pcm);
        }
        timings.push(total_ns / iterations_per_sample as f64);
    }
    let mut case = simple_case(
        "phase=first_borrowed_pcm;container=wav;codec=pcm_s16le",
        "first_borrowed_pcm",
        "ns/first-packet",
        timings,
        samples,
        iterations_per_sample,
    )?;
    case.work_validation.first_packet_samples = first_samples;
    case.work_validation.all_samples_finite = all_finite;
    case.work_validation.crate_staging_bytes = staging_bytes.unwrap_or(0);
    case.work_validation.valid &= first_samples > 0 && all_finite;
    Ok(case)
}

fn benchmark_steady_decode(
    fixture: &DeterministicPcmFixture,
    samples: usize,
) -> Result<DecoderTimingCase, String> {
    let mut ns_per_frame = Vec::with_capacity(samples);
    let mut frames_per_second = Vec::with_capacity(samples);
    let mut realtime_factor = Vec::with_capacity(samples);
    let mut observed_frames = None;
    let mut observed_packets = None;
    let mut observed_hash = None;
    let mut observed_sample_rate = None;
    let mut observed_channels = None;
    let mut observed_staging = None;
    let mut all_finite = true;

    for _ in 0..samples {
        let trial = steady_trial(&fixture.path)?;
        ns_per_frame.push(trial.ns_per_frame);
        frames_per_second.push(trial.frames_per_second);
        realtime_factor.push(trial.realtime_factor);
        check_stable(
            &mut observed_frames,
            trial.decoded_frames,
            "steady decoded frames",
        )?;
        check_stable(&mut observed_packets, trial.packets, "steady packet count")?;
        check_stable(&mut observed_hash, trial.hash, "steady output hash")?;
        check_stable(
            &mut observed_sample_rate,
            trial.sample_rate_hz,
            "steady sample rate",
        )?;
        check_stable(&mut observed_channels, trial.channels, "steady channels")?;
        check_stable(
            &mut observed_staging,
            trial.staging_bytes,
            "steady staging bytes",
        )?;
        all_finite &= trial.all_finite;
    }

    let decoded_frames = observed_frames.unwrap_or(0);
    let packets = observed_packets.unwrap_or(0);
    let hash = observed_hash.unwrap_or(FNV_OFFSET);
    let mut case = DecoderTimingCase {
        case_key: "phase=steady_borrowed_decode;container=wav;codec=pcm_s16le".to_string(),
        phase: "steady_borrowed_decode".to_string(),
        primary_unit: "ns/frame".to_string(),
        distribution: summarize_callback_samples(ns_per_frame)?,
        throughput_frames_per_second: Some(summarize_trials(frames_per_second)?),
        realtime_factor: Some(summarize_trials(realtime_factor)?),
        expected_timing_samples: samples,
        iterations_per_timing_sample: 1,
        work_validation: DecoderWorkValidation {
            valid: decoded_frames > 0 && packets > 0 && all_finite,
            observed_operations: samples,
            expected_operations: samples,
            decoded_frames,
            expected_decoded_frames: decoded_frames,
            first_packet_samples: 0,
            packet_count: packets,
            all_samples_finite: all_finite,
            output_fnv1a64: format!("{hash:016x}"),
            maximum_seek_error_frames: 0,
            sample_rate_hz: observed_sample_rate.unwrap_or(0),
            channels: observed_channels.unwrap_or(0),
            crate_staging_bytes: observed_staging.unwrap_or(0),
        },
    };
    case.work_validation.valid &= case.distribution.samples.len() == samples;
    Ok(case)
}

fn benchmark_seek_command(
    fixture: &DeterministicPcmFixture,
    samples: usize,
    targets: &[f64],
) -> Result<DecoderTimingCase, String> {
    let mut decoder = StreamingDecoder::open(MediaLocation::local(fixture.path.clone()))
        .map_err(|error| format!("seek decoder open failed: {error}"))?;
    let mut timings = Vec::with_capacity(samples);
    let mut max_error = 0u64;
    for index in 0..samples {
        let target = targets[index % targets.len()];
        let start = Instant::now();
        decoder
            .seek(target)
            .map_err(|error| format!("timed decoder seek failed at {target}s: {error}"))?;
        timings.push(elapsed_ns(start));
        max_error = max_error.max(seek_error_frames(&decoder, target));
    }
    let mut case = simple_case(
        "phase=seek_command;mode=coarse;container=wav;codec=pcm_s16le",
        "coarse_seek_command",
        "ns/seek",
        timings,
        samples,
        1,
    )?;
    case.work_validation.maximum_seek_error_frames = max_error;
    case.work_validation.valid &= max_error <= StreamingDecoder::SEEK_COARSE_TOLERANCE_FRAMES;
    Ok(case)
}

fn benchmark_seek_to_first_pcm(
    fixture: &DeterministicPcmFixture,
    samples: usize,
    targets: &[f64],
) -> Result<DecoderTimingCase, String> {
    let mut decoder = StreamingDecoder::open(MediaLocation::local(fixture.path.clone()))
        .map_err(|error| format!("seek-to-PCM decoder open failed: {error}"))?;
    let mut timings = Vec::with_capacity(samples);
    let mut max_error = 0u64;
    let mut first_packet_samples = 0usize;
    let mut all_finite = true;
    for index in 0..samples {
        let target = targets[index % targets.len()];
        let start = Instant::now();
        decoder
            .seek(target)
            .map_err(|error| format!("seek-to-PCM seek failed at {target}s: {error}"))?;
        max_error = max_error.max(seek_error_frames(&decoder, target));
        let pcm = decoder
            .decode_next_borrowed()
            .map_err(|error| format!("post-seek borrowed PCM failed at {target}s: {error}"))?
            .ok_or_else(|| format!("post-seek borrowed PCM reached end at {target}s"))?;
        timings.push(elapsed_ns(start));
        first_packet_samples = pcm.len();
        all_finite &= pcm.iter().all(|sample| sample.is_finite());
        black_box(pcm);
    }
    let mut case = simple_case(
        "phase=seek_to_first_pcm;mode=coarse;container=wav;codec=pcm_s16le",
        "coarse_seek_to_first_borrowed_pcm",
        "ns/seek-plus-first-packet",
        timings,
        samples,
        1,
    )?;
    case.work_validation.first_packet_samples = first_packet_samples;
    case.work_validation.all_samples_finite = all_finite;
    case.work_validation.maximum_seek_error_frames = max_error;
    case.work_validation.valid &= first_packet_samples > 0
        && all_finite
        && max_error <= StreamingDecoder::SEEK_COARSE_TOLERANCE_FRAMES;
    Ok(case)
}

fn simple_case(
    case_key: &str,
    phase: &str,
    unit: &str,
    timings: Vec<f64>,
    expected_timing_samples: usize,
    iterations_per_timing_sample: usize,
) -> Result<DecoderTimingCase, String> {
    let observed_operations = timings.len().saturating_mul(iterations_per_timing_sample);
    let expected_operations = expected_timing_samples.saturating_mul(iterations_per_timing_sample);
    Ok(DecoderTimingCase {
        case_key: case_key.to_string(),
        phase: phase.to_string(),
        primary_unit: unit.to_string(),
        distribution: summarize_callback_samples(timings)?,
        throughput_frames_per_second: None,
        realtime_factor: None,
        expected_timing_samples,
        iterations_per_timing_sample,
        work_validation: DecoderWorkValidation {
            valid: observed_operations == expected_operations,
            observed_operations,
            expected_operations,
            decoded_frames: 0,
            expected_decoded_frames: 0,
            first_packet_samples: 0,
            packet_count: 0,
            all_samples_finite: true,
            output_fnv1a64: String::new(),
            maximum_seek_error_frames: 0,
            sample_rate_hz: 0,
            channels: 0,
            crate_staging_bytes: 0,
        },
    })
}

fn steady_trial(path: &Path) -> Result<SteadyTrial, String> {
    let mut decoder = StreamingDecoder::open(MediaLocation::local(path.to_path_buf()))
        .map_err(|error| format!("steady decoder open failed: {error}"))?;
    let first = decoder
        .decode_next_borrowed()
        .map_err(|error| format!("steady decoder priming failed: {error}"))?
        .ok_or_else(|| "steady decoder fixture has no first packet".to_string())?;
    black_box(first);

    let channels = decoder.info().channels.max(1);
    let sample_rate_hz = decoder.info().sample_rate;
    let sample_rate = sample_rate_hz as f64;
    let staging_bytes = decoder.staging_buffer_bytes();
    let mut decoded_frames = 0u64;
    let mut packets = 0usize;
    let mut all_finite = true;
    let mut hash = FNV_OFFSET;
    let start = Instant::now();
    while let Some(pcm) = decoder
        .decode_next_borrowed()
        .map_err(|error| format!("steady borrowed decode failed: {error}"))?
    {
        decoded_frames = decoded_frames.saturating_add(pcm.len() as u64 / channels as u64);
        packets += 1;
        all_finite &= pcm.iter().all(|sample| sample.is_finite());
        hash = hash_samples(hash, pcm);
        black_box(pcm);
    }
    let elapsed_ns = elapsed_ns(start);
    if decoded_frames == 0 || packets == 0 {
        return Err("steady decode fixture produced no post-first-packet work".to_string());
    }
    let elapsed_seconds = elapsed_ns / 1.0e9;
    let media_seconds = decoded_frames as f64 / sample_rate;
    Ok(SteadyTrial {
        ns_per_frame: elapsed_ns / decoded_frames as f64,
        frames_per_second: decoded_frames as f64 / elapsed_seconds,
        realtime_factor: media_seconds / elapsed_seconds,
        decoded_frames,
        packets,
        all_finite,
        hash,
        sample_rate_hz,
        channels,
        staging_bytes,
    })
}

fn decode_full(path: &Path) -> Result<DecoderWorkValidation, String> {
    let mut decoder = StreamingDecoder::open(MediaLocation::local(path.to_path_buf()))
        .map_err(|error| format!("fixture decoder open failed: {error}"))?;
    let channels = decoder.info().channels.max(1);
    let sample_rate_hz = decoder.info().sample_rate;
    let expected_frames = decoder.info().total_frames.unwrap_or(0);
    let mut decoded_frames = 0u64;
    let mut packet_count = 0usize;
    let mut first_packet_samples = 0usize;
    let mut all_finite = true;
    let mut hash = FNV_OFFSET;
    while let Some(pcm) = decoder
        .decode_next_borrowed()
        .map_err(|error| format!("fixture borrowed decode failed: {error}"))?
    {
        if packet_count == 0 {
            first_packet_samples = pcm.len();
        }
        decoded_frames = decoded_frames.saturating_add(pcm.len() as u64 / channels as u64);
        packet_count += 1;
        all_finite &= pcm.iter().all(|sample| sample.is_finite());
        hash = hash_samples(hash, pcm);
    }
    Ok(DecoderWorkValidation {
        valid: decoded_frames == expected_frames && packet_count > 0 && all_finite,
        observed_operations: packet_count,
        expected_operations: packet_count,
        decoded_frames,
        expected_decoded_frames: expected_frames,
        first_packet_samples,
        packet_count,
        all_samples_finite: all_finite,
        output_fnv1a64: format!("{hash:016x}"),
        maximum_seek_error_frames: 0,
        sample_rate_hz,
        channels,
        crate_staging_bytes: decoder.staging_buffer_bytes(),
    })
}

fn open_builder(
    path: &Path,
) -> Result<audio_engine_core::decoder::StreamingDecoderBuilder, String> {
    let source = OpenedMediaSource::open_local(path, None)
        .map_err(|error| format!("decoder source open failed: {error}"))?;
    StreamingDecoder::probe_opened_source(source, None)
        .map_err(|error| format!("decoder probe failed: {error}"))
}

fn measure_allocations(
    fixture: &DeterministicPcmFixture,
) -> Result<Vec<DecoderAllocationEvidence>, String> {
    let open_scope = AllocationScope::start();
    let source = OpenedMediaSource::open_local(&fixture.path, None)
        .map_err(|error| format!("allocation source open failed: {error}"))?;
    black_box(&source);
    let open_snapshot = open_scope.finish();
    drop(source);

    let source = OpenedMediaSource::open_local(&fixture.path, None)
        .map_err(|error| format!("allocation probe source open failed: {error}"))?;
    let probe_scope = AllocationScope::start();
    let builder = StreamingDecoder::probe_opened_source(source, None)
        .map_err(|error| format!("allocation probe failed: {error}"))?;
    let builder_staging = builder
        .staging_buffer_bytes()
        .map_err(|error| format!("allocation builder staging query failed: {error}"))?;
    black_box(&builder);
    let probe_snapshot = probe_scope.finish();

    let build_scope = AllocationScope::start();
    let mut decoder = builder
        .build()
        .map_err(|error| format!("allocation decoder build failed: {error}"))?;
    let decoder_staging = decoder.staging_buffer_bytes();
    black_box(&decoder);
    let build_snapshot = build_scope.finish();

    let first_scope = AllocationScope::start();
    let first = decoder
        .decode_next_borrowed()
        .map_err(|error| format!("allocation first PCM failed: {error}"))?
        .ok_or_else(|| "allocation first PCM reached end of stream".to_string())?;
    black_box(first);
    let first_snapshot = first_scope.finish();
    drop(decoder);

    let mut steady_decoder = StreamingDecoder::open(MediaLocation::local(fixture.path.clone()))
        .map_err(|error| format!("allocation steady decoder open failed: {error}"))?;
    let _ = steady_decoder
        .decode_next_borrowed()
        .map_err(|error| format!("allocation steady priming failed: {error}"))?;
    let steady_scope = AllocationScope::start();
    while let Some(pcm) = steady_decoder
        .decode_next_borrowed()
        .map_err(|error| format!("allocation steady decode failed: {error}"))?
    {
        black_box(pcm);
    }
    let steady_snapshot = steady_scope.finish();

    Ok(vec![
        allocation_evidence("local_source_open", open_snapshot, 0),
        allocation_evidence("container_probe", probe_snapshot, builder_staging),
        allocation_evidence("decoder_build", build_snapshot, decoder_staging),
        allocation_evidence("first_borrowed_pcm", first_snapshot, decoder_staging),
        allocation_evidence("steady_borrowed_decode", steady_snapshot, decoder_staging),
    ])
}

fn allocation_evidence(
    phase: &str,
    snapshot: AllocationSnapshot,
    staging_bytes: usize,
) -> DecoderAllocationEvidence {
    DecoderAllocationEvidence {
        phase: phase.to_string(),
        allocation_calls: snapshot.allocations,
        deallocation_calls: snapshot.deallocations,
        reallocation_calls: snapshot.reallocations,
        peak_live_bytes: snapshot.peak_live_bytes,
        retained_bytes: snapshot.live_bytes,
        crate_staging_bytes: staging_bytes,
    }
}

fn seek_error_frames(decoder: &StreamingDecoder, target_seconds: f64) -> u64 {
    let target = (target_seconds * decoder.info().sample_rate as f64).round() as u64;
    decoder.current_frame().abs_diff(target)
}

fn check_stable<T: Copy + Eq + std::fmt::Display>(
    observed: &mut Option<T>,
    value: T,
    label: &str,
) -> Result<(), String> {
    if let Some(previous) = observed {
        if *previous != value {
            return Err(format!(
                "{label} changed across trials: first {previous}, later {value}"
            ));
        }
    } else {
        *observed = Some(value);
    }
    Ok(())
}

fn hash_samples(mut hash: u64, samples: &[f64]) -> u64 {
    for sample in samples {
        for byte in sample.to_bits().to_le_bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

fn elapsed_ns(start: Instant) -> f64 {
    start.elapsed().as_nanos().max(1) as f64
}

fn print_report(report: &DecoderReport) -> Result<(), String> {
    println!(
        "audio_decoder_perf mode={} cases={} fixture={} environment={}",
        report.mode.as_str(),
        report.cases.len(),
        report.conditions.fixture.content_fnv1a64,
        environment_json(&report.environment)?
    );
    for case in &report.cases {
        println!(
            "decoder case={} unit={} median={:.3} p95={:.3} p99={:.3} max={:.3} samples={} valid={}",
            case.case_key,
            case.primary_unit,
            case.distribution.median,
            case.distribution.p95,
            case.distribution.p99,
            case.distribution.max,
            case.distribution.samples.len(),
            case.work_validation.valid
        );
        if let (Some(throughput), Some(realtime)) =
            (&case.throughput_frames_per_second, &case.realtime_factor)
        {
            println!(
                "decoder steady throughput_fps_median={:.3} realtime_factor_median={:.3}",
                throughput.median, realtime.median
            );
        }
    }
    for memory in &report.allocation_evidence {
        println!(
            "decoder allocation phase={} allocs={} deallocs={} reallocs={} peak_bytes={} retained_bytes={} staging_bytes={}",
            memory.phase,
            memory.allocation_calls,
            memory.deallocation_calls,
            memory.reallocation_calls,
            memory.peak_live_bytes,
            memory.retained_bytes,
            memory.crate_staging_bytes
        );
    }
    Ok(())
}

fn enforce_report(report: &DecoderReport) -> Result<(), String> {
    validate_case_key_set(
        report.cases.iter().map(|case| case.case_key.clone()),
        EXPECTED_CASE_KEYS.map(str::to_string),
        "decoder",
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
                    .any(|value| !value.is_finite() || *value <= 0.0)
        })
        .map(|case| case.case_key.as_str())
        .collect::<Vec<_>>();
    if !invalid.is_empty() {
        return Err(format!(
            "decoder work/report integrity gate failed for cases: {}",
            invalid.join(", ")
        ));
    }
    if report.cases.iter().any(|case| {
        case.throughput_frames_per_second
            .as_ref()
            .is_some_and(|distribution| distribution.samples.len() != case.expected_timing_samples)
            || case.realtime_factor.as_ref().is_some_and(|distribution| {
                distribution.samples.len() != case.expected_timing_samples
            })
    }) {
        return Err(
            "decoder throughput/realtime distributions have an invalid sample count".to_string(),
        );
    }
    if report
        .allocation_evidence
        .iter()
        .find(|memory| memory.phase == "decoder_build")
        .is_none_or(|memory| memory.crate_staging_bytes == 0)
    {
        return Err("decoder allocation report is missing fixed staging bytes".to_string());
    }
    if let Some(error) = regression_gate_error(
        &report.comparisons,
        "decoder median regression gate failed",
        "primary-unit",
    ) {
        return Err(error);
    }
    Ok(())
}
