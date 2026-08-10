use std::fs::File;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use audio_engine_core::{MediaLocation, StreamingDecoder};

#[allow(dead_code)]
mod support;

use support::{
    generated_unix_ms, summarize_trials, write_json, BenchEnvironment, BenchMode, PerfArgs,
    TrialDistribution, REPORT_SCHEMA_VERSION,
};

const PROBE: &str = "decoder_gapless_hybrid_verification";
const DEFAULT_OGG_FIXTURE: &str = "target/decoder-bench-corpus/stereo_s16_48k_80s.ogg";
const DEFAULT_FLAC_FIXTURE: &str = "target/decoder-bench-corpus/stereo_s16_48k_80s.flac";
const SEEK_SEARCH_RADIUS_FRAMES: u64 = 8_192;
const SEEK_COMPARE_FRAMES: usize = 128;

#[derive(Debug, Serialize)]
struct Conditions {
    fixture_env: String,
    warmups_per_mode: usize,
    timed_trials_per_mode: usize,
    trial_order: String,
    process_priority: String,
    timing_units: String,
    project_path: String,
    native_path: String,
    correctness_scope: Vec<String>,
    excludes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DecodeValidation {
    path: String,
    extension: String,
    status: String,
    project_sample_rate_hz: u32,
    native_sample_rate_hz: u32,
    project_channels: usize,
    native_channels: usize,
    project_frames: usize,
    native_frames: usize,
    project_hash: String,
    native_hash: String,
    max_abs_delta: f64,
    rms_delta: f64,
    full_output_equivalent: bool,
    seek_target_secs: Option<f64>,
    project_seek_frame: Option<u64>,
    native_seek_frame: Option<u64>,
    project_seek_chunk_frames: Option<usize>,
    native_seek_chunk_frames: Option<usize>,
    project_seek_nearest_reference_frame: Option<u64>,
    native_seek_nearest_reference_frame: Option<u64>,
    project_seek_nearest_rms: Option<f64>,
    native_seek_nearest_rms: Option<f64>,
    seek_chunk_max_abs_delta: Option<f64>,
    seek_chunk_rms_delta: Option<f64>,
    seek_chunks_equivalent: Option<bool>,
    seek_reference_equivalent: Option<bool>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TimingCase {
    case_key: String,
    path: String,
    mode: String,
    decode_frames_per_trial: Vec<usize>,
    decode_hashes_per_trial: Vec<String>,
    open_ms: TrialDistribution,
    decode_ms: TrialDistribution,
}

#[derive(Debug, Serialize)]
struct ComparisonCase {
    case_key: String,
    path: String,
    paired_trials: Vec<PairedDecodeTrial>,
    native_to_project_decode_ratio: TrialDistribution,
    project_first_decode_ratio: TrialDistribution,
    native_first_decode_ratio: TrialDistribution,
}

#[derive(Debug, Serialize)]
struct PairedDecodeTrial {
    round: usize,
    order: String,
    project_ms: f64,
    native_ms: f64,
    native_to_project_ratio: f64,
}

struct TimingSamples {
    native: bool,
    open_ms: Vec<f64>,
    decode_ms: Vec<f64>,
    frame_counts: Vec<usize>,
    hashes: Vec<String>,
}

/// A fixture that was present and attempted but whose correctness probe could
/// not produce a verdict.
///
/// This is deliberately not folded into `skipped`: `skipped` records work the
/// run never owed (an absent fixture), while this records work that was owed and
/// failed. Keeping them apart is what lets `--enforce` stay honest.
#[derive(Debug, Serialize)]
struct ProbeFailure {
    path: String,
    error: String,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    probe: String,
    generated_unix_ms: u128,
    mode: BenchMode,
    environment: BenchEnvironment,
    conditions: Conditions,
    validations: Vec<DecodeValidation>,
    cases: Vec<TimingCase>,
    comparisons: Vec<ComparisonCase>,
    probe_failures: Vec<ProbeFailure>,
    skipped: Vec<String>,
}

struct NativeDecoder {
    format_reader: Box<dyn FormatReader + 'static>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: usize,
    time_base: Option<TimeBase>,
    start_ts: Timestamp,
    staging: Vec<f64>,
}

struct DecodeOutput {
    sample_rate: u32,
    channels: usize,
    samples: Vec<f64>,
    finite: bool,
}

struct ConsumeOutput {
    frames: usize,
    hash: u64,
}

struct SeekOutput {
    realized_frame: u64,
    chunk: Vec<f64>,
}

fn main() -> Result<(), String> {
    let args = PerfArgs::parse(std::env::args().skip(1).collect())?;
    if args.help {
        print_help();
        return Ok(());
    }

    let (warmups, trials) = workload(args.mode);
    let environment = BenchEnvironment::capture();
    let fixture_env = std::env::var("AUDIO_GAPLESS_FIXTURES").unwrap_or_else(|_| {
        "target/decoder-bench-corpus/*.ogg;target/decoder-bench-corpus/*.flac".to_string()
    });
    let fixtures = resolve_fixtures(&fixture_env);

    let conditions = Conditions {
        fixture_env,
        warmups_per_mode: warmups,
        timed_trials_per_mode: trials,
        trial_order: "ABBA across consecutive rounds; project_hybrid is A and native_gapless is B"
            .to_string(),
        process_priority: std::env::var("AUDIO_GAPLESS_PROCESS_PRIORITY")
            .unwrap_or_else(|_| "inherited".to_string()),
        timing_units: "milliseconds; open/probe and borrowed streaming decode measured separately"
            .to_string(),
        project_path: "StreamingDecoder hybrid policy (native MP3/Vorbis + Track fallback)"
            .to_string(),
        native_path: "Symphonia AudioDecoderOptions::gapless(true)".to_string(),
        correctness_scope: vec![
            "full sequential output frame count and finite samples".to_string(),
            "FNV-1a output hash plus max/RMS sample delta".to_string(),
            "coarse seek first chunk against the full native reference".to_string(),
        ],
        excludes: vec![
            "MP3/LAME and CAF fixtures unless explicitly supplied".to_string(),
            "audio-device callback and end-to-end playback".to_string(),
            "decode_all allocation timing".to_string(),
        ],
    };

    let mut validations = Vec::new();
    let mut cases = Vec::new();
    let mut comparisons = Vec::new();
    let mut probe_failures = Vec::new();
    let mut skipped = Vec::new();

    if fixtures.is_empty() {
        skipped.push(format!(
            "no readable fixture matched AUDIO_GAPLESS_FIXTURES={}",
            conditions.fixture_env
        ));
    }
    for (extension, description) in [("mp3", "MP3/LAME"), ("caf", "CAF")] {
        if !has_extension(&fixtures, extension) {
            skipped.push(format!(
                "{description} gapless fixture absent; include a .{extension} path in AUDIO_GAPLESS_FIXTURES"
            ));
        }
    }

    for path in fixtures {
        match validate_fixture(&path) {
            Ok(validation) => validations.push(validation),
            Err(error) => {
                probe_failures.push(ProbeFailure {
                    path: path.display().to_string(),
                    error,
                });
                continue;
            }
        }

        let mut timing = [
            TimingSamples::new(false, trials),
            TimingSamples::new(true, trials),
        ];

        for round in 0..warmups {
            for index in round_order(round) {
                let result = timed_run(&path, timing[index].native)?;
                black_box((result.frames, result.hash));
            }
        }

        for round in 0..trials {
            for index in round_order(round) {
                let result = timed_run(&path, timing[index].native)?;
                timing[index].record(result);
            }
        }

        let paired_trials = timing[0]
            .decode_ms
            .iter()
            .zip(&timing[1].decode_ms)
            .enumerate()
            .map(|(round, (project, native))| PairedDecodeTrial {
                round,
                order: if round.is_multiple_of(2) {
                    "project_hybrid_then_native_gapless"
                } else {
                    "native_gapless_then_project_hybrid"
                }
                .to_string(),
                project_ms: *project,
                native_ms: *native,
                native_to_project_ratio: native / project,
            })
            .collect::<Vec<_>>();
        let paired_decode_ratios = paired_trials
            .iter()
            .map(|trial| trial.native_to_project_ratio)
            .collect();
        let project_first_decode_ratios = paired_trials
            .iter()
            .filter(|trial| trial.round.is_multiple_of(2))
            .map(|trial| trial.native_to_project_ratio)
            .collect();
        let native_first_decode_ratios = paired_trials
            .iter()
            .filter(|trial| !trial.round.is_multiple_of(2))
            .map(|trial| trial.native_to_project_ratio)
            .collect();
        comparisons.push(ComparisonCase {
            case_key: format!("path={};comparison=native_to_project", path.display()),
            path: path.display().to_string(),
            paired_trials,
            native_to_project_decode_ratio: summarize_trials(paired_decode_ratios)?,
            project_first_decode_ratio: summarize_trials(project_first_decode_ratios)?,
            native_first_decode_ratio: summarize_trials(native_first_decode_ratios)?,
        });

        for samples in timing {
            cases.push(samples.finish(&path)?);
        }
    }

    let report = Report {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: PROBE.to_string(),
        generated_unix_ms: generated_unix_ms(),
        mode: args.mode,
        environment,
        conditions,
        validations,
        cases,
        comparisons,
        probe_failures,
        skipped,
    };

    print_report(&report);
    if let Some(path) = &args.out {
        write_json(path, &report, "gapless comparison report")?;
    }
    if args.enforce {
        enforce_report(&report)?;
    }
    Ok(())
}

fn workload(mode: BenchMode) -> (usize, usize) {
    match mode {
        BenchMode::Quick => (2, 5),
        BenchMode::Full => (3, 9),
        BenchMode::Heavy => (5, 15),
    }
}

fn round_order(round: usize) -> [usize; 2] {
    if round.is_multiple_of(2) {
        [0, 1]
    } else {
        [1, 0]
    }
}

impl TimingSamples {
    fn new(native: bool, trials: usize) -> Self {
        Self {
            native,
            open_ms: Vec::with_capacity(trials),
            decode_ms: Vec::with_capacity(trials),
            frame_counts: Vec::with_capacity(trials),
            hashes: Vec::with_capacity(trials),
        }
    }

    fn record(&mut self, result: TimedResult) {
        self.open_ms.push(result.open_ms);
        self.decode_ms.push(result.decode_ms);
        self.frame_counts.push(result.frames);
        self.hashes.push(format!("{:016x}", result.hash));
        black_box((&self.open_ms, &self.decode_ms, &self.frame_counts));
    }

    fn finish(self, path: &Path) -> Result<TimingCase, String> {
        let mode_name = if self.native {
            "native_gapless"
        } else {
            "project_hybrid"
        };
        Ok(TimingCase {
            case_key: format!("path={};mode={mode_name}", path.display()),
            path: path.display().to_string(),
            mode: mode_name.to_string(),
            decode_frames_per_trial: self.frame_counts,
            decode_hashes_per_trial: self.hashes,
            open_ms: summarize_trials(self.open_ms)?,
            decode_ms: summarize_trials(self.decode_ms)?,
        })
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_gapless_comparison_perf -- [--quick|--heavy] [--enforce] [--out <json>]\n\
         \n\
         Set AUDIO_GAPLESS_FIXTURES to a semicolon-separated list of MP3/Ogg/CAF/WAV/FLAC paths.\n\
         Without a fixture, the report is explicitly skipped. Timing and correctness are report-only\n\
         unless --enforce is supplied."
    );
}

fn resolve_fixtures(spec: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for item in spec.split(';').flat_map(|part| part.split(',')) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let path = PathBuf::from(item);
        if item.contains('*') || item.contains('?') {
            let (parent, pattern) = split_glob(&path);
            let Ok(entries) = std::fs::read_dir(parent) else {
                continue;
            };
            for entry in entries.flatten() {
                let candidate = entry.path();
                if candidate.is_file()
                    && wildcard_match(
                        pattern,
                        candidate
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default(),
                    )
                {
                    paths.push(candidate);
                }
            }
        } else if path.is_file() {
            paths.push(path);
        }
    }
    if paths.is_empty() && spec.contains("target/decoder-bench-corpus") {
        for fallback in [DEFAULT_OGG_FIXTURE, DEFAULT_FLAC_FIXTURE] {
            let path = PathBuf::from(fallback);
            if path.is_file() {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn has_extension(paths: &[PathBuf], expected: &str) -> bool {
    paths.iter().any(|path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
    })
}

fn split_glob(path: &Path) -> (&Path, &str) {
    let mut parent = path;
    while parent
        .to_str()
        .is_some_and(|value| value.contains('*') || value.contains('?'))
    {
        parent = parent.parent().unwrap_or_else(|| Path::new("."));
    }
    let pattern = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    (parent, pattern)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    wildcard_match_bytes(pattern.as_bytes(), value.as_bytes())
}

fn wildcard_match_bytes(pattern: &[u8], value: &[u8]) -> bool {
    match pattern.first() {
        None => value.is_empty(),
        Some(b'*') => {
            wildcard_match_bytes(&pattern[1..], value)
                || (!value.is_empty() && wildcard_match_bytes(pattern, &value[1..]))
        }
        Some(b'?') => !value.is_empty() && wildcard_match_bytes(&pattern[1..], &value[1..]),
        Some(byte) => {
            value.first() == Some(byte) && wildcard_match_bytes(&pattern[1..], &value[1..])
        }
    }
}

fn validate_fixture(path: &Path) -> Result<DecodeValidation, String> {
    let project = decode_project_full(path)?;
    let native = decode_native_full(path)?;
    let (max_abs_delta, rms_delta) = sample_delta(&project.samples, &native.samples);
    let full_output_equivalent = project.sample_rate == native.sample_rate
        && project.channels == native.channels
        && project.finite
        && native.finite
        && project.samples.len() == native.samples.len()
        && max_abs_delta <= 1.0e-5;

    let seek_target_secs = choose_seek_target(&project);
    let mut notes = Vec::new();
    let (project_seek, native_seek, seek_equivalent) = if let Some(target) = seek_target_secs {
        let project_seek = project_seek(path, target)?;
        let native_seek = native_seek(path, target)?;
        let project_match = nearest_reference(
            &native.samples,
            &project_seek.chunk,
            project_seek.realized_frame,
            native.channels,
        );
        let native_match = nearest_reference(
            &native.samples,
            &native_seek.chunk,
            native_seek.realized_frame,
            native.channels,
        );
        let (seek_max_abs_delta, seek_rms_delta) =
            sample_delta(&project_seek.chunk, &native_seek.chunk);
        let seek_chunks_equivalent =
            project_seek.chunk.len() == native_seek.chunk.len() && seek_max_abs_delta <= 1.0e-5;
        let equivalent =
            seek_chunks_equivalent && project_match.1 <= 1.0e-4 && native_match.1 <= 1.0e-4;
        if !equivalent {
            notes.push("at least one post-seek chunk did not match the full native reference within 1e-4 RMS".to_string());
        }
        (
            Some((
                project_seek,
                project_match,
                seek_max_abs_delta,
                seek_rms_delta,
                seek_chunks_equivalent,
            )),
            Some((native_seek, native_match)),
            Some(equivalent),
        )
    } else {
        notes.push("fixture is too short for a stable seek comparison".to_string());
        (None, None, None)
    };

    let status = if full_output_equivalent && seek_equivalent != Some(false) {
        "pass"
    } else {
        "mismatch"
    };

    Ok(DecodeValidation {
        path: path.display().to_string(),
        extension: path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string(),
        status: status.to_string(),
        project_sample_rate_hz: project.sample_rate,
        native_sample_rate_hz: native.sample_rate,
        project_channels: project.channels,
        native_channels: native.channels,
        project_frames: project.samples.len() / project.channels.max(1),
        native_frames: native.samples.len() / native.channels.max(1),
        project_hash: format!("{:016x}", fnv1a(&project.samples)),
        native_hash: format!("{:016x}", fnv1a(&native.samples)),
        max_abs_delta,
        rms_delta,
        full_output_equivalent,
        seek_target_secs,
        project_seek_frame: project_seek
            .as_ref()
            .map(|(seek, _, _, _, _)| seek.realized_frame),
        native_seek_frame: native_seek.as_ref().map(|(seek, _)| seek.realized_frame),
        project_seek_chunk_frames: project_seek
            .as_ref()
            .map(|(seek, _, _, _, _)| seek.chunk.len() / project.channels.max(1)),
        native_seek_chunk_frames: native_seek
            .as_ref()
            .map(|(seek, _)| seek.chunk.len() / native.channels.max(1)),
        project_seek_nearest_reference_frame: project_seek
            .as_ref()
            .map(|(_, nearest, _, _, _)| nearest.0),
        native_seek_nearest_reference_frame: native_seek.as_ref().map(|(_, nearest)| nearest.0),
        project_seek_nearest_rms: project_seek.as_ref().map(|(_, nearest, _, _, _)| nearest.1),
        native_seek_nearest_rms: native_seek.as_ref().map(|(_, nearest)| nearest.1),
        seek_chunk_max_abs_delta: project_seek.as_ref().map(|(_, _, max_abs, _, _)| *max_abs),
        seek_chunk_rms_delta: project_seek.as_ref().map(|(_, _, _, rms, _)| *rms),
        seek_chunks_equivalent: project_seek
            .as_ref()
            .map(|(_, _, _, _, equivalent)| *equivalent),
        seek_reference_equivalent: seek_equivalent,
        notes,
    })
}

fn decode_project_full(path: &Path) -> Result<DecodeOutput, String> {
    let mut decoder = StreamingDecoder::open(MediaLocation::local(path.to_path_buf()))
        .map_err(|error| format!("project open: {error:?}"))?;
    let sample_rate = decoder.info().sample_rate;
    let channels = decoder.info().channels;
    let samples = decoder
        .decode_all()
        .map_err(|error| format!("project decode: {error:?}"))?;
    let finite = samples.iter().all(|sample| sample.is_finite());
    Ok(DecodeOutput {
        sample_rate,
        channels,
        samples,
        finite,
    })
}

fn decode_native_full(path: &Path) -> Result<DecodeOutput, String> {
    let mut decoder = NativeDecoder::open(path, true)?;
    let sample_rate = decoder.sample_rate;
    let channels = decoder.channels;
    let mut samples = Vec::new();
    decoder.decode_into(&mut samples)?;
    let finite = samples.iter().all(|sample| sample.is_finite());
    Ok(DecodeOutput {
        sample_rate,
        channels,
        samples,
        finite,
    })
}

struct TimedResult {
    open_ms: f64,
    decode_ms: f64,
    frames: usize,
    hash: u64,
}

fn timed_run(path: &Path, native: bool) -> Result<TimedResult, String> {
    let open_start = Instant::now();
    let decode_start;
    let result = if native {
        let mut decoder = NativeDecoder::open(path, true)?;
        decode_start = Instant::now();
        let output = decoder.consume()?;
        (
            output,
            decode_start.duration_since(open_start).as_secs_f64() * 1.0e3,
        )
    } else {
        let mut decoder = StreamingDecoder::open(MediaLocation::local(path.to_path_buf()))
            .map_err(|error| format!("project open: {error:?}"))?;
        decode_start = Instant::now();
        let output = consume_project(&mut decoder)?;
        (
            output,
            decode_start.duration_since(open_start).as_secs_f64() * 1.0e3,
        )
    };
    Ok(TimedResult {
        open_ms: result.1,
        decode_ms: Instant::now().duration_since(decode_start).as_secs_f64() * 1.0e3,
        frames: result.0.frames,
        hash: result.0.hash,
    })
}

fn consume_project(decoder: &mut StreamingDecoder) -> Result<ConsumeOutput, String> {
    let mut samples = 0usize;
    let mut hash = FNV_OFFSET;
    while let Some(chunk) = decoder
        .decode_next_borrowed()
        .map_err(|error| format!("project streaming decode: {error:?}"))?
    {
        samples += chunk.len();
        hash = fnv1a_extend(hash, chunk);
        black_box(chunk.len());
    }
    let channels = decoder.info().channels.max(1);
    Ok(ConsumeOutput {
        frames: samples / channels,
        hash,
    })
}

impl NativeDecoder {
    fn open(path: &Path, gapless: bool) -> Result<Self, String> {
        let file = File::open(path)
            .map_err(|error| format!("native open '{}': {error}", path.display()))?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
            hint.with_extension(extension);
        }
        let format_reader = symphonia::default::get_probe()
            .probe(
                &hint,
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|error| format!("native probe: {error}"))?;
        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| "native probe found no audio track".to_string())?;
        let track_id = track.id;
        let time_base = track.time_base;
        let start_ts = track.start_ts;
        let codec_params = track
            .codec_params
            .as_ref()
            .and_then(CodecParameters::audio)
            .ok_or_else(|| "native track has no audio codec parameters".to_string())?;
        let sample_rate = codec_params.sample_rate.unwrap_or(44_100);
        let channels = codec_params
            .channels
            .as_ref()
            .map(symphonia::core::audio::Channels::count)
            .unwrap_or(2);
        let staging_frames = codec_params
            .max_frames_per_packet
            .filter(|frames| *frames > 0)
            .or(codec_params.frames_per_block.filter(|frames| *frames > 0))
            .unwrap_or(65_536);
        let staging_samples = usize::try_from(staging_frames)
            .ok()
            .and_then(|frames| frames.checked_mul(channels))
            .ok_or_else(|| "native staging size overflow".to_string())?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(
                codec_params,
                &AudioDecoderOptions::default().gapless(gapless),
            )
            .map_err(|error| format!("native decoder construction: {error}"))?;

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            sample_rate,
            channels,
            time_base,
            start_ts,
            staging: vec![0.0; staging_samples],
        })
    }

    fn next_interleaved(&mut self) -> Result<Option<&[f64]>, String> {
        loop {
            let packet = match self.format_reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(error) => return Err(format!("native packet read: {error}")),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(error) => return Err(format!("native packet decode: {error}")),
            };
            if decoded.num_planes() != self.channels {
                return Err(format!(
                    "native channel count changed from {} to {}",
                    self.channels,
                    decoded.num_planes()
                ));
            }
            let required = decoded.frames().saturating_mul(self.channels);
            if required > self.staging.len() {
                return Err(format!(
                    "native decoded packet exceeds staging: {} > {}",
                    required,
                    self.staging.len()
                ));
            }
            decoded.copy_to_slice_interleaved(&mut self.staging[..required]);
            return Ok(Some(&self.staging[..required]));
        }
    }

    fn decode_into(&mut self, output: &mut Vec<f64>) -> Result<(), String> {
        while let Some(chunk) = self.next_interleaved()? {
            output.extend_from_slice(chunk);
        }
        Ok(())
    }

    fn consume(&mut self) -> Result<ConsumeOutput, String> {
        let mut frames = 0usize;
        let mut hash = FNV_OFFSET;
        let channels = self.channels.max(1);
        while let Some(chunk) = self.next_interleaved()? {
            frames += chunk.len() / channels;
            hash = fnv1a_extend(hash, chunk);
            black_box(chunk.len());
        }
        Ok(ConsumeOutput { frames, hash })
    }

    fn seek(&mut self, time_secs: f64) -> Result<u64, String> {
        let time =
            Time::try_from_secs_f64(time_secs).ok_or_else(|| "invalid seek time".to_string())?;
        let seeked = self
            .format_reader
            .seek(
                SeekMode::Coarse,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|error| format!("native seek: {error}"))?;
        self.decoder.reset();
        timestamp_to_frame_offset(
            seeked.actual_ts,
            self.start_ts,
            self.time_base,
            self.sample_rate,
        )
        .ok_or_else(|| "native seek timestamp overflow".to_string())
    }

    fn seek_chunk(&mut self, time_secs: f64) -> Result<SeekOutput, String> {
        let realized_frame = self.seek(time_secs)?;
        let mut chunk = Vec::new();
        while chunk.is_empty() {
            let Some(samples) = self.next_interleaved()? else {
                break;
            };
            chunk.extend_from_slice(samples);
        }
        Ok(SeekOutput {
            realized_frame,
            chunk,
        })
    }
}

fn project_seek(path: &Path, time_secs: f64) -> Result<SeekOutput, String> {
    let mut decoder = StreamingDecoder::open(MediaLocation::local(path.to_path_buf()))
        .map_err(|error| format!("project seek open: {error:?}"))?;
    decoder
        .seek(time_secs)
        .map_err(|error| format!("project seek: {error:?}"))?;
    let realized_frame = decoder.current_frame();
    let mut chunk = Vec::new();
    while chunk.is_empty() {
        let Some(samples) = decoder
            .decode_next_borrowed()
            .map_err(|error| format!("project seek decode: {error:?}"))?
        else {
            break;
        };
        chunk.extend_from_slice(samples);
    }
    Ok(SeekOutput {
        realized_frame,
        chunk,
    })
}

fn native_seek(path: &Path, time_secs: f64) -> Result<SeekOutput, String> {
    NativeDecoder::open(path, true)?.seek_chunk(time_secs)
}

fn choose_seek_target(output: &DecodeOutput) -> Option<f64> {
    let duration =
        output.samples.len() as f64 / output.channels.max(1) as f64 / output.sample_rate as f64;
    (duration > 2.0).then_some(1.0_f64.min(duration * 0.5))
}

fn nearest_reference(
    reference: &[f64],
    chunk: &[f64],
    center_frame: u64,
    channels: usize,
) -> (u64, f64) {
    if reference.is_empty() || chunk.is_empty() {
        return (0, f64::INFINITY);
    }
    let channels = channels.max(1);
    let compare_samples = chunk.len().min(SEEK_COMPARE_FRAMES * channels);
    let compare_frames = compare_samples / channels;
    let total_frames = reference.len() / channels;
    let max_start = total_frames.saturating_sub(compare_frames);
    let low = center_frame
        .saturating_sub(SEEK_SEARCH_RADIUS_FRAMES)
        .min(max_start as u64);
    let high = center_frame
        .saturating_add(SEEK_SEARCH_RADIUS_FRAMES)
        .min(max_start as u64);
    let mut best = (low, f64::INFINITY);
    for frame in low..=high {
        let start = frame as usize * channels;
        let end = start + compare_samples;
        let candidate = &reference[start..end];
        let (_, rms) = sample_delta(&chunk[..compare_samples], candidate);
        if rms < best.1 {
            best = (frame, rms);
        }
    }
    best
}

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv1a(samples: &[f64]) -> u64 {
    fnv1a_extend(FNV_OFFSET, samples)
}

fn fnv1a_extend(mut hash: u64, samples: &[f64]) -> u64 {
    for sample in samples {
        hash ^= sample.to_bits();
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn sample_delta(left: &[f64], right: &[f64]) -> (f64, f64) {
    let count = left.len().min(right.len());
    if count == 0 {
        return (f64::INFINITY, f64::INFINITY);
    }
    let mut max_abs = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    for (a, b) in left.iter().zip(right.iter()).take(count) {
        let delta = (*a - *b).abs();
        max_abs = max_abs.max(delta);
        sum_sq += delta * delta;
    }
    (max_abs, (sum_sq / count as f64).sqrt())
}

fn timestamp_to_frame_offset(
    timestamp: Timestamp,
    start_ts: Timestamp,
    time_base: Option<TimeBase>,
    sample_rate: u32,
) -> Option<u64> {
    let relative_ts = timestamp.get().checked_sub(start_ts.get())?;
    if relative_ts <= 0 {
        return Some(0);
    }
    let Some(time_base) = time_base else {
        return u64::try_from(relative_ts).ok();
    };
    let numerator = i128::from(relative_ts)
        .checked_mul(i128::from(time_base.numer.get()))?
        .checked_mul(i128::from(sample_rate))?;
    let frames = numerator / i128::from(time_base.denom.get());
    u64::try_from(frames).ok()
}

fn print_report(report: &Report) {
    println!(
        "audio_gapless_comparison_perf mode={} verified_fixtures={} probe_failed={} warmups={} trials={}",
        report.mode.as_str(),
        report.validations.len(),
        report.probe_failures.len(),
        report.conditions.warmups_per_mode,
        report.conditions.timed_trials_per_mode
    );
    println!(
        "audio_gapless_comparison_environment {:?}",
        report.environment
    );
    for validation in &report.validations {
        println!(
            "validation path={} status={} frames(project/native)={}/{} max_abs_delta={:.6e} rms_delta={:.6e} seek_rms(project/native)={:?}/{:?}",
            validation.path,
            validation.status,
            validation.project_frames,
            validation.native_frames,
            validation.max_abs_delta,
            validation.rms_delta,
            validation.project_seek_nearest_rms,
            validation.native_seek_nearest_rms
        );
    }
    for case in &report.cases {
        println!(
            "timing mode={} path={} open_ms(median/p95)={:.3}/{:.3} decode_ms(median/p95)={:.3}/{:.3}",
            case.mode,
            case.path,
            case.open_ms.median,
            case.open_ms.p95,
            case.decode_ms.median,
            case.decode_ms.p95
        );
    }
    for comparison in &report.comparisons {
        println!(
            "comparison path={} paired_native_vs_project_decode_pct(min/median/max)={:.2}/{:.2}/{:.2} order_median_pct(project-first/native-first)={:.2}/{:.2}",
            comparison.path,
            (comparison.native_to_project_decode_ratio.min - 1.0) * 100.0,
            (comparison.native_to_project_decode_ratio.median - 1.0) * 100.0,
            (comparison.native_to_project_decode_ratio.max - 1.0) * 100.0,
            (comparison.project_first_decode_ratio.median - 1.0) * 100.0,
            (comparison.native_first_decode_ratio.median - 1.0) * 100.0,
        );
    }
    for failure in &report.probe_failures {
        println!("probe-failed path={} error={}", failure.path, failure.error);
    }
    for skipped in &report.skipped {
        println!("skipped {skipped}");
    }
}

fn enforce_report(report: &Report) -> Result<(), String> {
    if !report.probe_failures.is_empty() {
        // An attempted fixture that could not be verified is a correctness
        // result, not absent work. Reporting it as skipped would let one
        // succeeding fixture turn a failed probe into a green run.
        return Err(format!(
            "gapless correctness probes failed: {}",
            report
                .probe_failures
                .iter()
                .map(|failure| format!("{} ({})", failure.path, failure.error))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if report.validations.is_empty() {
        return Err("gapless comparison has no validation fixtures".to_string());
    }
    let failures = report
        .validations
        .iter()
        .filter(|validation| validation.status != "pass")
        .map(|validation| validation.path.as_str())
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "gapless correctness mismatches: {}",
            failures.join(", ")
        ))
    }
}
