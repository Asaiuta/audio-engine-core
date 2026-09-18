//! Bench-local AutoMix accuracy runner, shared with its integration tests.

pub mod corpus;
pub mod fixtures;
pub mod metrics;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use audio_automix::{analyze_automix, AutomixAnalysisMode, AutomixAnalysisOptions};
use audio_engine_core::decoder::{MediaLocation, StreamingDecoder};
use serde::Serialize;

use crate::support::{generated_unix_ms, write_json, BenchEnvironment};
use corpus::{Annotation, Corpus, Manifest, MetricKind, Split};

pub const ESTIMATOR_CONFIGURATION: &str =
    "automix_v4_logband24flux200_window46ms_fluxclock_acfblur10ms_prior120_sigma1.5_subdiv6relative_peakcontrast_top2fallback_channelmean_margin5pct_compat1pct_dp100_grid125ms_v10_dev";
pub const ANALYSIS_CAP_SEC: f64 = 60.0;

#[derive(Debug, Default)]
pub struct Options {
    pub quick: bool,
    pub split: Split,
    pub manifest: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub required: BTreeSet<String>,
    pub enforce: bool,
    pub out: Option<PathBuf>,
    pub help: bool,
}

impl Options {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut result = Self::default();
        let mut selected_split = None;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--quick" => result.quick = true,
                "--enforce" => result.enforce = true,
                "--help" | "-h" => result.help = true,
                "--bench" => (), // Cargo's custom-harness marker.
                "--split" => {
                    let split = match args.next().as_deref() {
                        Some("development") => Split::Development,
                        Some("evaluation") => Split::Evaluation,
                        _ => return Err("--split needs development or evaluation".into()),
                    };
                    if selected_split.is_some_and(|previous| previous != split) {
                        return Err("conflicting duplicate option --split".into());
                    }
                    selected_split = Some(split);
                    result.split = split;
                }
                "--corpus-manifest" | "--corpus-root" | "--out" => {
                    let value = args
                        .next()
                        .filter(|value| !value.starts_with("--") && !value.is_empty())
                        .ok_or_else(|| format!("{arg} needs a path"))?;
                    let target = match arg.as_str() {
                        "--corpus-manifest" => &mut result.manifest,
                        "--corpus-root" => &mut result.root,
                        _ => &mut result.out,
                    };
                    let path = PathBuf::from(value);
                    if target.as_ref().is_some_and(|previous| previous != &path) {
                        return Err(format!("conflicting duplicate option {arg}"));
                    }
                    *target = Some(path);
                }
                "--require-corpus" => {
                    let id = args
                        .next()
                        .filter(|id| !id.starts_with("--") && !id.trim().is_empty())
                        .ok_or("--require-corpus needs an id")?;
                    result.required.insert(id);
                }
                _ => return Err(format!("unknown option: {arg}")),
            }
        }
        if result.quick
            && (result.manifest.is_some()
                || result.root.is_some()
                || !result.required.is_empty()
                || selected_split.is_some())
        {
            return Err("--quick cannot select or require a corpus".into());
        }
        if result.manifest.is_none()
            && (result.root.is_some() || !result.required.is_empty() || selected_split.is_some())
        {
            return Err("corpus root/requirements/split need --corpus-manifest".into());
        }
        Ok(result)
    }
}

#[derive(Debug, Serialize)]
pub struct Interval {
    pub start_sec: f64,
    pub end_sec: f64,
    pub frames: u64,
    pub sample_rate_hz: u32,
    pub channels: usize,
}

#[derive(Debug, Serialize)]
pub struct Prediction {
    pub analysis_version: u32,
    pub bpm: Option<f64>,
    pub bpm_confidence: Option<f64>,
    pub first_beat_pos: Option<f64>,
    pub beat_grid_stability: Option<f64>,
    pub key: Option<metrics::Key>,
}

#[derive(Debug, Serialize)]
pub struct Case {
    pub case_key: String,
    pub corpus_id: String,
    pub track_id: String,
    pub split: Split,
    pub reference: Option<Annotation>,
    pub prediction: Option<Prediction>,
    pub analyzed_interval: Option<Interval>,
    pub scores: BTreeMap<String, f64>,
    pub input_failure: Option<String>,
    pub fixture: Option<fixtures::Fixture>,
}

#[derive(Debug, Serialize)]
pub struct Metric {
    pub name: String,
    pub corpus_id: String,
    pub classification: String,
    pub comparison: String,
    pub measured: Option<f64>,
    pub threshold: f64,
    pub unit: String,
    pub passed: Option<bool>,
    pub case_count: usize,
    pub detail: String,
}

impl Metric {
    fn corpus(
        kind: MetricKind,
        corpus_id: &str,
        measured: Option<f64>,
        count: usize,
        target: bool,
        split: Split,
        detail: &str,
    ) -> Self {
        let (minimum, goal) = kind.bars();
        let threshold = if target { goal } else { minimum };
        Self {
            name: format!("{}{}", kind.name(), if target { "_target" } else { "" }),
            corpus_id: corpus_id.into(),
            classification: if measured.is_none() {
                "skipped"
            } else if target || split == Split::Development {
                "report"
            } else {
                "gate"
            }
            .into(),
            comparison: ">=".into(),
            measured,
            threshold,
            unit: "fraction".into(),
            passed: measured.map(|value| value >= threshold),
            case_count: count,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CorpusReport {
    pub id: String,
    pub declaration: Option<Corpus>,
    pub manifest_sha256: Option<String>,
    pub resolved_root: Option<PathBuf>,
    pub required: bool,
    pub status: String,
    pub expected_count: usize,
    pub included_count: usize,
    pub development_count: usize,
    pub evaluation_count: usize,
    pub selected_split: Split,
    pub selected_count: usize,
    pub evaluated_count: usize,
    pub excluded_count: usize,
    pub missing_track_count: usize,
    pub missing_paths: Vec<String>,
    pub integrity_errors: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub probe: String,
    pub generated_unix_ms: u128,
    pub mode: String,
    pub environment: BenchEnvironment,
    pub conditions: serde_json::Value,
    pub corpora: Vec<CorpusReport>,
    pub cases: Vec<Case>,
    pub metrics: Vec<Metric>,
    pub input_errors: Vec<String>,
}

impl Report {
    pub fn new(options: &Options) -> Self {
        Self {
            schema_version: 2,
            probe: "audio_automix_accuracy".into(),
            generated_unix_ms: generated_unix_ms(),
            mode: if options.quick { "quick" } else { "full" }.into(),
            environment: BenchEnvironment::capture(),
            conditions: serde_json::json!({
                "analysis_mode": "head", "max_analyze_time_sec": ANALYSIS_CAP_SEC,
                "estimator_configuration": ESTIMATOR_CONFIGURATION,
                "estimator_source_sha256": corpus::sha256_bytes(&[include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).as_slice(),include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/decode.rs")).as_slice(),include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/features.rs")).as_slice(),include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tempo.rs")).as_slice(),include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/placement.rs")).as_slice()].concat()),
                "decode_source_sha256": corpus::sha256_bytes(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/decode.rs"))),
                "features_source_sha256": corpus::sha256_bytes(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/features.rs"))),
                "placement_source_sha256": corpus::sha256_bytes(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/placement.rs"))),
                "tempo_source_sha256": corpus::sha256_bytes(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tempo.rs"))),
                "fixture_revision": fixtures::FIXTURE_REVISION,
                "metric_reference": "mir_eval==0.8.2",
                "tempo_relative_tolerance": metrics::TEMPO_TOLERANCE,
                "tempo_accuracy2_multipliers": [1.0/3.0, 0.5, 1.0, 2.0, 3.0],
                "beat_collar_sec_inclusive": metrics::BEAT_COLLAR_SEC,
                "amlt_phase_period_tolerance_strict": metrics::CONTINUITY_TOLERANCE,
                "beat_trim_sec_inclusive": metrics::TRIM_SEC,
                "beat_interval": "intersection with realized [start,end), then trim both arrays at >=5 s",
                "corpus_split": options.split,
                "corpus_acceptance_gates": options.split == Split::Evaluation,
                "aggregation": "per-corpus selected split macro mean; development scores are diagnostic; no partial passing aggregate",
                "coverage": "independently decode the same bounded head window and count selected frames",
                "key_detector": "not integrated; labeled key predictions abstain",
                "synthetic_bpm_error_max": 0.05, "synthetic_phase_error_sec_max": 0.010,
                "synthetic_drift_sec_max": 0.020
            }),
            corpora: Vec::new(),
            cases: Vec::new(),
            metrics: Vec::new(),
            input_errors: Vec::new(),
        }
    }

    pub fn sort(&mut self) {
        self.corpora.sort_by(|a, b| a.id.cmp(&b.id));
        self.cases.sort_by(|a, b| a.case_key.cmp(&b.case_key));
        self.metrics
            .sort_by(|a, b| (&a.corpus_id, &a.name).cmp(&(&b.corpus_id, &b.name)));
        self.input_errors.sort();
    }

    pub fn exit_result(&self, enforce: bool) -> Result<(), String> {
        if !self.input_errors.is_empty() {
            return Err(self.input_errors.join("; "));
        }
        let failed = self
            .metrics
            .iter()
            .filter(|metric| metric.classification == "gate" && metric.passed == Some(false))
            .count();
        if enforce && failed > 0 {
            Err(format!("{failed} accuracy gates failed"))
        } else {
            Ok(())
        }
    }

    pub fn write_then_enforce(&mut self, options: &Options) -> Result<(), String> {
        self.sort();
        if let Some(path) = &options.out {
            write_json(path, self, "AutoMix accuracy report")?;
        }
        self.exit_result(options.enforce)
    }
}

/// Count decoded frames, respecting the same metadata-selected head boundary
/// as AutoMix. Metadata bounds selection; it is never used as measured coverage.
pub fn measure_interval(path: &Path) -> Result<Interval, String> {
    let mut decoder = StreamingDecoder::open(MediaLocation::Local(path.to_path_buf()))
        .map_err(|error| error.to_string())?;
    let sample_rate = decoder.info().sample_rate;
    let channels = decoder.info().channels.max(1);
    if sample_rate == 0 {
        return Err("zero sample rate".into());
    }
    let plausible = |duration: f64| duration.is_finite() && duration > 0.0 && duration <= 86_400.0;
    let cap = (ANALYSIS_CAP_SEC * f64::from(sample_rate)).ceil() as u64;
    let declared_frames = decoder
        .info()
        .total_frames
        .filter(|frames| plausible(*frames as f64 / f64::from(sample_rate)))
        .or_else(|| {
            decoder
                .info()
                .duration_secs
                .filter(|duration| plausible(*duration))
                .map(|duration| (duration * f64::from(sample_rate)).ceil() as u64)
        });
    let limit = declared_frames.map_or(cap, |frames| frames.min(cap));
    let mut frames = 0;
    while frames < limit {
        let Some(samples) = decoder
            .decode_next_borrowed()
            .map_err(|error| error.to_string())?
        else {
            break;
        };
        frames += (samples.len() as u64 / channels as u64).min(limit - frames);
    }
    Ok(Interval {
        start_sec: 0.0,
        end_sec: frames as f64 / f64::from(sample_rate),
        frames,
        sample_rate_hz: sample_rate,
        channels,
    })
}

pub fn analyze_file(path: &Path) -> Result<(Prediction, Interval), String> {
    let interval = measure_interval(path)?;
    let result = analyze_automix(
        MediaLocation::Local(path.to_path_buf()),
        None,
        AutomixAnalysisOptions {
            mode: AutomixAnalysisMode::Head,
            max_analyze_time_sec: ANALYSIS_CAP_SEC,
        },
    )
    .map_err(|error| error.to_string())?;
    let prediction = Prediction {
        analysis_version: result.version,
        bpm: result.bpm,
        bpm_confidence: result.bpm_confidence,
        first_beat_pos: result.first_beat_pos,
        beat_grid_stability: result.beat_grid_stability,
        key: None,
    };
    if [
        prediction.bpm,
        prediction.bpm_confidence,
        prediction.first_beat_pos,
        prediction.beat_grid_stability,
    ]
    .into_iter()
    .flatten()
    .any(|value| !value.is_finite())
    {
        return Err("non-finite public prediction".into());
    }
    Ok((prediction, interval))
}

pub fn score(
    reference: &Annotation,
    prediction: &Prediction,
    interval: &Interval,
    kinds: &[MetricKind],
) -> BTreeMap<String, f64> {
    let predicted_beats = metrics::trim_beats(
        &metrics::predicted_beats(prediction.bpm, prediction.first_beat_pos, interval.end_sec),
        interval.start_sec,
        interval.end_sec,
    );
    let reference_beats = metrics::trim_beats(
        reference.beats_sec.as_deref().unwrap_or(&[]),
        interval.start_sec,
        interval.end_sec,
    );
    kinds
        .iter()
        .map(|kind| {
            let value = match kind {
                MetricKind::TempoAccuracy1 | MetricKind::TempoAccuracy2 => metrics::tempo_accuracy(
                    reference.tempo_bpm.expect("validated tempo label"),
                    prediction.bpm,
                    *kind == MetricKind::TempoAccuracy2,
                ),
                MetricKind::BeatFMeasure => {
                    metrics::beat_f_measure(&reference_beats, &predicted_beats)
                }
                MetricKind::BeatAmlt => metrics::beat_amlt(&reference_beats, &predicted_beats),
                MetricKind::KeyExact | MetricKind::KeyMirexWeighted => {
                    let (exact, weighted) = metrics::key_scores(
                        reference.key.expect("validated key label"),
                        prediction.key,
                    );
                    if *kind == MetricKind::KeyExact {
                        exact
                    } else {
                        weighted
                    }
                }
            };
            (kind.name().into(), value)
        })
        .collect()
}

fn skipped_external(report: &mut Report) {
    for (id, kinds) in [
        (
            "giantsteps-tempo",
            [MetricKind::TempoAccuracy1, MetricKind::TempoAccuracy2],
        ),
        (
            "ballroom-beats",
            [MetricKind::BeatFMeasure, MetricKind::BeatAmlt],
        ),
    ] {
        report.corpora.push(CorpusReport {
            id: id.into(),
            declaration: None,
            manifest_sha256: None,
            resolved_root: None,
            required: false,
            status: "not_selected".into(),
            expected_count: 0,
            included_count: 0,
            development_count: 0,
            evaluation_count: 0,
            selected_split: Split::Evaluation,
            selected_count: 0,
            evaluated_count: 0,
            excluded_count: 0,
            missing_track_count: 0,
            missing_paths: Vec::new(),
            integrity_errors: Vec::new(),
        });
        for kind in kinds {
            report.metrics.push(Metric::corpus(
                kind,
                id,
                None,
                0,
                false,
                Split::Evaluation,
                "no corpus selected; no real-music accuracy evidence",
            ));
        }
    }
}

pub fn evaluate_corpora(report: &mut Report, options: &Options) {
    let Some(path) = &options.manifest else {
        skipped_external(report);
        return;
    };
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            report
                .input_errors
                .push(format!("selected manifest {}: {error}", path.display()));
            return;
        }
    };
    let digest = corpus::sha256_bytes(&bytes);
    report.conditions["manifest_sha256"] = digest.clone().into();
    let mut manifest = match serde_json::from_slice::<Manifest>(&bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            report
                .input_errors
                .push(format!("malformed manifest: {error}"));
            return;
        }
    };
    if let Err(error) = corpus::validate_manifest(&mut manifest) {
        report
            .input_errors
            .push(format!("invalid manifest: {error}"));
        return;
    }
    for required in &options.required {
        if !manifest.corpora.iter().any(|corpus| &corpus.id == required) {
            report
                .input_errors
                .push(format!("unknown required corpus: {required}"));
        }
    }
    let root = options
        .root
        .clone()
        .unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")).to_path_buf());
    let root = if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    };
    let root = root
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(root));
    for corpus in manifest.corpora {
        let inputs = corpus::load_corpus(&root, &corpus, options.split);
        let required = options.required.contains(&corpus.id);
        let mut summary = CorpusReport {
            id: corpus.id.clone(),
            declaration: Some(corpus.clone()),
            manifest_sha256: Some(digest.clone()),
            resolved_root: Some(root.clone()),
            required,
            status: "evaluated".into(),
            expected_count: corpus.expected_track_count,
            included_count: corpus.tracks.len(),
            development_count: corpus
                .tracks
                .iter()
                .filter(|track| track.split == Split::Development)
                .count(),
            evaluation_count: corpus
                .tracks
                .iter()
                .filter(|track| track.split == Split::Evaluation)
                .count(),
            selected_split: options.split,
            selected_count: corpus
                .tracks
                .iter()
                .filter(|track| track.split == options.split)
                .count(),
            evaluated_count: 0,
            excluded_count: corpus.exclusions.len(),
            missing_track_count: inputs.missing_track_count,
            missing_paths: inputs.missing,
            integrity_errors: inputs.integrity_errors,
        };
        let mut aggregate: BTreeMap<_, f64> = corpus
            .metrics
            .iter()
            .map(|kind| (kind.name(), 0.0))
            .collect();
        for loaded in inputs.tracks {
            let outcome = analyze_file(&loaded.audio_path).and_then(|(prediction, interval)| {
                if interval.end_sec <= metrics::TRIM_SEC {
                    return Err("track too short for the fixed evaluation protocol; declare a pre-evaluation exclusion".into());
                }
                if corpus.metrics.iter().any(|kind| matches!(kind, MetricKind::BeatFMeasure | MetricKind::BeatAmlt))
                    && metrics::trim_beats(loaded.annotation.beats_sec.as_deref().unwrap_or(&[]), interval.start_sec, interval.end_sec).len() < 2
                {
                    return Err("fewer than two reference beats in the evaluation interval; declare a pre-evaluation exclusion".into());
                }
                Ok((prediction, interval))
            });
            let mut case = Case {
                case_key: format!("{}:{}", corpus.id, loaded.track.id),
                corpus_id: corpus.id.clone(),
                track_id: loaded.track.id,
                split: loaded.track.split,
                reference: Some(loaded.annotation),
                prediction: None,
                analyzed_interval: None,
                scores: BTreeMap::new(),
                input_failure: None,
                fixture: None,
            };
            match outcome {
                Ok((prediction, interval)) => {
                    case.scores = score(
                        case.reference.as_ref().expect("reference set"),
                        &prediction,
                        &interval,
                        &corpus.metrics,
                    );
                    for (name, value) in &case.scores {
                        *aggregate.get_mut(name.as_str()).expect("declared metric") += value;
                    }
                    summary.evaluated_count += 1;
                    case.prediction = Some(prediction);
                    case.analyzed_interval = Some(interval);
                }
                Err(error) => {
                    summary
                        .integrity_errors
                        .push(format!("{}: {error}", case.track_id));
                    case.input_failure = Some(error);
                }
            }
            report.cases.push(case);
        }
        // List every selected identity, including inputs that could not be
        // loaded. Missing inputs cannot silently vanish from the report.
        for track in corpus
            .tracks
            .iter()
            .filter(|track| track.split == options.split)
        {
            let key = format!("{}:{}", corpus.id, track.id);
            if !report.cases.iter().any(|case| case.case_key == key) {
                report.cases.push(Case {
                    case_key: key,
                    corpus_id: corpus.id.clone(),
                    track_id: track.id.clone(),
                    split: track.split,
                    reference: None,
                    prediction: None,
                    analyzed_interval: None,
                    scores: BTreeMap::new(),
                    input_failure: Some(
                        "missing or invalid input; see corpus coverage/integrity errors".into(),
                    ),
                    fixture: None,
                });
            }
        }
        let complete = summary.selected_count > 0
            && summary.missing_paths.is_empty()
            && summary.integrity_errors.is_empty()
            && summary.evaluated_count == summary.selected_count;
        if summary.selected_count == 0 {
            summary.status = "not_selected".into();
        } else if !summary.integrity_errors.is_empty() {
            summary.status = "invalid".into();
            report.input_errors.push(format!(
                "{}: {}",
                corpus.id,
                summary.integrity_errors.join("; ")
            ));
        } else if !complete {
            summary.status = "missing".into();
        }
        if required && !complete {
            report.input_errors.push(format!(
                "required corpus {} incomplete: evaluated {}/{} {} tracks, missing {} selected tracks",
                corpus.id,
                summary.evaluated_count,
                summary.selected_count,
                options.split.name(),
                summary.missing_track_count
            ));
        }
        for kind in &corpus.metrics {
            let measured =
                complete.then(|| aggregate[kind.name()] / summary.evaluated_count as f64);
            for target in [false, true] {
                report.metrics.push(Metric::corpus(
                    *kind,
                    &corpus.id,
                    measured,
                    if complete { summary.evaluated_count } else { 0 },
                    target,
                    options.split,
                    if complete {
                        if options.split == Split::Development {
                            "development split; diagnostic per-track macro mean, not held-out acceptance"
                        } else {
                            "frozen evaluation split; per-track macro mean"
                        }
                    } else if summary.selected_count == 0 {
                        "no tracks in selected split; aggregate unavailable"
                    } else {
                        "incomplete/invalid input; aggregate unavailable"
                    },
                ));
            }
        }
        report.corpora.push(summary);
    }
}

pub fn evaluate_synthetic(report: &mut Report) -> Result<(), String> {
    let root = PathBuf::from("target/audio-benchmark-fixtures/automix-accuracy");
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    for fixture in fixtures::suite() {
        let path = root.join(format!("{}.wav", fixture.id));
        let (bytes, reference) = fixture.render();
        fs::write(&path, bytes).map_err(|error| error.to_string())?;
        let (prediction, interval) = analyze_file(&path)?;
        let mut scores = BTreeMap::new();
        if fixture.precision_gate() {
            let period = 60.0 / fixture.bpm;
            let phase_error = prediction.first_beat_pos.map(|phase| {
                (phase - fixture.phase_sec + period / 2.0).rem_euclid(period) - period / 2.0
            });
            let bpm_error = prediction.bpm.map(|bpm| (bpm - fixture.bpm).abs());
            let drift = prediction.bpm.zip(phase_error).map(|(bpm, phase)| {
                let last = ((interval.end_sec - fixture.phase_sec) / period).floor();
                phase
                    .abs()
                    .max((phase + last * (60.0 / bpm - period)).abs())
            });
            for (name, measured, threshold, unit) in [
                ("bpm_error", bpm_error, 0.05, "BPM"),
                ("phase_error", phase_error.map(f64::abs), 0.010, "seconds"),
                ("grid_drift", drift, 0.020, "seconds"),
            ] {
                if let Some(value) = measured {
                    scores.insert(name.into(), value);
                }
                report.metrics.push(Metric {
                    name: format!("{}:{name}", fixture.id), corpus_id: "synthetic".into(), classification: "gate".into(), comparison: "<=".into(),
                    measured, threshold, unit: unit.into(), passed: Some(measured.is_some_and(|value| value <= threshold)), case_count: 1,
                    detail: "audio-to-public-result; missing prediction is a miss; drift uses rounded public BPM".into(),
                });
            }
        }
        let (checks, comparison) = if fixture.precision_gate() {
            (
                vec![("grid_stability", prediction.beat_grid_stability, 0.90)],
                ">=",
            )
        } else if matches!(
            fixture.pattern,
            fixtures::Pattern::Ramp | fixtures::Pattern::OffGrid
        ) {
            (
                vec![
                    (
                        "unstable_grid",
                        Some(prediction.beat_grid_stability.unwrap_or(0.0)),
                        0.80,
                    ),
                    (
                        "uncertain_tempo",
                        Some(prediction.bpm_confidence.unwrap_or(0.0)),
                        0.35,
                    ),
                ],
                "<",
            )
        } else {
            let abstains = prediction.bpm.is_none()
                && prediction.first_beat_pos.is_none()
                && prediction.beat_grid_stability.is_none();
            (
                vec![(
                    "tempo_abstention",
                    Some(if abstains { 1.0 } else { 0.0 }),
                    1.0,
                )],
                ">=",
            )
        };
        for (name, measured, threshold) in checks {
            if let Some(value) = measured {
                scores.insert(name.into(), value);
            }
            report.metrics.push(Metric {
                name: format!("{}:{name}", fixture.id),
                corpus_id: "synthetic".into(),
                classification: "gate".into(),
                comparison: comparison.into(),
                measured,
                threshold,
                unit: "fraction".into(),
                passed: Some(measured.is_some_and(|value| if comparison == "<" {
                    value < threshold
                } else {
                    value >= threshold
                })),
                case_count: 1,
                detail: "constant fixtures require stable grids; drifting fixtures cannot authorize snapping; noise/silence/short inputs abstain".into(),
            });
        }
        report.cases.push(Case {
            case_key: format!("synthetic:{}", fixture.id),
            corpus_id: "synthetic".into(),
            track_id: fixture.id.clone(),
            split: Split::Development,
            reference: Some(reference),
            prediction: Some(prediction),
            analyzed_interval: Some(interval),
            scores,
            input_failure: None,
            fixture: Some(fixture),
        });
    }
    Ok(())
}

pub fn run(options: &Options) -> Result<Report, String> {
    let mut report = Report::new(options);
    evaluate_corpora(&mut report, options);
    if let Err(error) = evaluate_synthetic(&mut report) {
        report
            .input_errors
            .push(format!("synthetic fixture: {error}"));
    }
    report.sort();
    Ok(report)
}
