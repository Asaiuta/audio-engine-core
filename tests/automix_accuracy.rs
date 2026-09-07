#[path = "../benches/support/automix_accuracy/mod.rs"]
pub mod accuracy;
#[path = "../benches/support/mod.rs"]
pub mod support;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use accuracy::corpus::{self, Annotation, Manifest, MetricKind};
use accuracy::{evaluate_corpora, metrics, Options, Report};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Goldens {
    reference: String,
    beat_cases: Vec<BeatGolden>,
    key_cases: Vec<KeyGolden>,
    tempo_cases: Vec<TempoGolden>,
}

#[derive(Deserialize)]
struct TempoGolden {
    reference: f64,
    prediction: Option<f64>,
    accuracy1: f64,
    accuracy2: f64,
}

#[derive(Deserialize)]
struct BeatGolden {
    name: String,
    start_sec: f64,
    end_sec: f64,
    #[serde(default)]
    already_trimmed: bool,
    reference: Vec<f64>,
    prediction: Vec<f64>,
    f_measure: f64,
    amlt: f64,
}

#[derive(Deserialize)]
struct KeyGolden {
    reference: metrics::Key,
    prediction: Option<metrics::Key>,
    exact: f64,
    weighted: f64,
}

#[test]
fn scores_match_independent_mir_eval_082_goldens() {
    let goldens: Goldens = serde_json::from_str(include_str!(
        "../benches/support/automix_accuracy/metric_goldens.json"
    ))
    .unwrap();
    assert_eq!(goldens.reference, "mir_eval==0.8.2");
    for case in goldens.tempo_cases {
        assert_eq!(
            metrics::tempo_accuracy(case.reference, case.prediction, false),
            case.accuracy1
        );
        assert_eq!(
            metrics::tempo_accuracy(case.reference, case.prediction, true),
            case.accuracy2
        );
    }
    for case in goldens.beat_cases {
        let (reference, prediction) = if case.already_trimmed {
            (case.reference, case.prediction)
        } else {
            (
                metrics::trim_beats(&case.reference, case.start_sec, case.end_sec),
                metrics::trim_beats(&case.prediction, case.start_sec, case.end_sec),
            )
        };
        assert!(
            (metrics::beat_f_measure(&reference, &prediction) - case.f_measure).abs() < 1e-12,
            "{} F-measure",
            case.name
        );
        assert!(
            (metrics::beat_amlt(&reference, &prediction) - case.amlt).abs() < 1e-12,
            "{} AMLt: got {}, expected {}",
            case.name,
            metrics::beat_amlt(&reference, &prediction),
            case.amlt
        );
    }
    assert_eq!(goldens.key_cases.len(), 600);
    for case in goldens.key_cases {
        assert_eq!(
            metrics::key_scores(case.reference, case.prediction),
            (case.exact, case.weighted),
            "{:?} -> {:?}",
            case.reference,
            case.prediction
        );
    }
}

#[test]
fn tempo_tolerance_and_metrical_multipliers_are_explicit() {
    for ratio in [1.0 / 3.0, 0.5, 1.0, 2.0, 3.0] {
        for boundary in [0.96, 1.0, 1.04] {
            assert_eq!(
                metrics::tempo_accuracy(120.0, Some(120.0 * ratio * boundary), true),
                1.0
            );
        }
        for outside in [0.9599, 1.0401] {
            assert_eq!(
                metrics::tempo_accuracy(120.0, Some(120.0 * ratio * outside), true),
                0.0
            );
        }
    }
    assert_eq!(metrics::tempo_accuracy(120.0, Some(60.0), false), 0.0);
    assert_eq!(metrics::tempo_accuracy(120.0, None, true), 0.0);
    assert_eq!(metrics::tempo_accuracy(120.0, Some(f64::NAN), false), 0.0);
    assert_eq!(metrics::tempo_accuracy(120.0, Some(0.0), true), 0.0);
}

#[test]
fn beat_matching_trims_both_sides_and_never_reuses_a_prediction() {
    assert_eq!(
        metrics::trim_beats(&[0.0, 4.99, 5.0, 5.5, 6.0, 7.0], 0.0, 6.0),
        [5.0, 5.5]
    );
    assert_eq!(metrics::beat_f_measure(&[0.0], &[0.07]), 1.0);
    assert_eq!(metrics::beat_f_measure(&[5.0, 5.05], &[5.02]), 2.0 / 3.0);
    let beats = metrics::predicted_beats(Some(120.0), Some(0.25), 6.0);
    assert_eq!(metrics::trim_beats(&beats, 0.0, 6.0), [5.25, 5.75]);
}

fn options(args: &[&str]) -> Result<Options, String> {
    Options::parse(args.iter().map(|value| (*value).to_string()))
}

#[test]
fn cli_rejects_conflicts_and_missing_selection() {
    for args in [
        vec!["--quick", "--corpus-manifest", "data.json"],
        vec!["--quick", "--require-corpus", "tempo"],
        vec!["--require-corpus", "tempo"],
        vec!["--corpus-root", "data"],
        vec!["--out", "a", "--out", "b"],
        vec!["--corpus-manifest", "a", "--corpus-manifest", "b"],
        vec!["--unknown"],
        vec!["--out"],
        vec!["--out", "--quick"],
    ] {
        assert!(options(&args).is_err(), "{args:?}");
    }
    let valid = options(&[
        "--corpus-manifest",
        "m.json",
        "--require-corpus",
        "a",
        "--require-corpus",
        "b",
        "--enforce",
    ])
    .unwrap();
    assert_eq!(valid.required.len(), 2);
    assert!(valid.enforce);
}

struct Inputs {
    root: PathBuf,
    manifest: Value,
}

impl Inputs {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/automix accuracy tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        let audio = accuracy::fixtures::pcm16_wav(8_000, 1, &vec![0.0; 52_000]);
        let annotation = serde_json::to_vec(&json!({ "schema_version": 1, "tempo_bpm": 120.0, "beats_sec": [0.0, 0.5, 5.0, 5.5, 6.0, 6.5, 7.0] })).unwrap();
        fs::write(root.join("track.wav"), &audio).unwrap();
        fs::write(root.join("track.json"), &annotation).unwrap();
        let manifest = json!({
            "schema_version": 1,
            "corpora": [{
                "id": "local-test", "dataset_name": "generated silence", "dataset_version": "1",
                "source_revision": "generated-v1", "source_url": "local:generated-test",
                "license_note": "generated test data", "annotation_format": "automix_annotation_v1",
                "normalization_revision": "test-v1", "split_id": "fixed-evaluation-v1",
                "metrics": ["tempo_accuracy1", "tempo_accuracy2", "beat_f_measure", "beat_amlt"],
                "expected_track_count": 1,
                "tracks": [{ "id": "track", "recording_id": "recording-1", "split": "evaluation",
                    "audio": { "path": "track.wav", "sha256": corpus::sha256_bytes(&audio) },
                    "annotation": { "path": "track.json", "sha256": corpus::sha256_bytes(&annotation) }
                }],
                "exclusions": []
            }]
        });
        Self { root, manifest }
    }

    fn selected(&self, required: bool) -> Options {
        let manifest = self.root.join("manifest.json");
        fs::write(
            &manifest,
            serde_json::to_vec_pretty(&self.manifest).unwrap(),
        )
        .unwrap();
        let mut options = Options {
            manifest: Some(manifest),
            out: Some(self.root.join("report.json")),
            ..Options::default()
        };
        if required {
            options.required.insert("local-test".into());
        }
        options
    }

    fn evaluate(&self, required: bool) -> (Report, Options) {
        let options = self.selected(required);
        let mut report = Report::new(&options);
        evaluate_corpora(&mut report, &options);
        (report, options)
    }

    fn replace_annotation(&mut self, annotation: Value) {
        let bytes = serde_json::to_vec(&annotation).unwrap();
        fs::write(self.root.join("track.json"), &bytes).unwrap();
        self.manifest["corpora"][0]["tracks"][0]["annotation"]["sha256"] =
            corpus::sha256_bytes(&bytes).into();
    }
}

impl Drop for Inputs {
    fn drop(&mut self) {
        // This unique directory was created by this test, never caller supplied.
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn abstention_is_scored_and_report_is_written_before_enforcement() {
    let inputs = Inputs::new();
    let (mut report, mut options) = inputs.evaluate(true);
    assert!(
        report.exit_result(false).is_ok(),
        "{:?}",
        report.input_errors
    );
    assert!(report.exit_result(true).is_err());
    assert_eq!(report.corpora[0].evaluated_count, 1);
    let interval = report.cases[0].analyzed_interval.as_ref().unwrap();
    assert_eq!(interval.frames, 52_000);
    assert_eq!(interval.end_sec, 6.5);
    assert_eq!(interval.sample_rate_hz, 8_000);
    assert_eq!(interval.channels, 1);
    assert_eq!(report.cases[0].prediction.as_ref().unwrap().bpm, None);
    for metric in &report.metrics {
        assert_eq!(metric.measured, Some(0.0));
        assert_eq!(metric.passed, Some(false));
        assert_eq!(metric.case_count, 1);
    }
    options.enforce = true;
    assert!(report.write_then_enforce(&options).is_err());
    let persisted: Value =
        serde_json::from_slice(&fs::read(options.out.unwrap()).unwrap()).unwrap();
    assert_eq!(persisted["cases"][0]["prediction"]["bpm"], Value::Null);
    assert_eq!(
        persisted["corpora"][0]["manifest_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
}

#[test]
fn optional_partial_corpus_has_no_passing_aggregate_and_required_always_fails() {
    let mut inputs = Inputs::new();
    let mut missing = inputs.manifest["corpora"][0]["tracks"][0].clone();
    missing["id"] = "missing".into();
    missing["recording_id"] = "recording-2".into();
    missing["audio"]["path"] = "missing.wav".into();
    inputs.manifest["corpora"][0]["tracks"]
        .as_array_mut()
        .unwrap()
        .push(missing);
    inputs.manifest["corpora"][0]["expected_track_count"] = 2.into();
    let (report, _) = inputs.evaluate(false);
    assert!(report.exit_result(true).is_ok());
    assert_eq!(report.corpora[0].evaluated_count, 1);
    assert_eq!(report.corpora[0].missing_track_count, 1);
    assert_eq!(report.cases.len(), 2);
    for metric in report.metrics {
        assert_eq!(metric.classification, "skipped");
        assert_eq!(metric.measured, None);
        assert_eq!(metric.passed, None);
    }
    let (mut report, options) = inputs.evaluate(true);
    assert!(report
        .exit_result(false)
        .unwrap_err()
        .contains("required corpus"));
    assert!(report.write_then_enforce(&options).is_err());
    assert!(options.out.unwrap().is_file());
}

#[test]
fn invalid_selected_inputs_fail_without_enforcement_even_when_optional() {
    let mut inputs = Inputs::new();
    fs::write(inputs.root.join("track.wav"), b"corrupt").unwrap();
    let (report, _) = inputs.evaluate(false);
    assert!(report
        .exit_result(false)
        .unwrap_err()
        .contains("SHA-256 mismatch"));
    // Correct digest does not make invalid audio decodable.
    inputs.manifest["corpora"][0]["tracks"][0]["audio"]["sha256"] =
        corpus::sha256_bytes(b"corrupt").into();
    let (report, _) = inputs.evaluate(false);
    assert!(report.exit_result(false).is_err());
    assert!(report.cases[0].input_failure.is_some());
    inputs.replace_annotation(json!({"schema_version":1,"tempo_bpm":120.0}));
    let (report, _) = inputs.evaluate(false);
    assert!(report
        .exit_result(false)
        .unwrap_err()
        .contains("missing label"));
}

#[test]
fn malformed_manifest_unknown_id_and_short_tracks_are_input_failures() {
    let inputs = Inputs::new();
    let mut options = inputs.selected(false);
    options.required.insert("unknown".into());
    let mut report = Report::new(&options);
    evaluate_corpora(&mut report, &options);
    assert!(report
        .exit_result(false)
        .unwrap_err()
        .contains("unknown required corpus"));
    fs::write(options.manifest.as_ref().unwrap(), b"{").unwrap();
    let mut report = Report::new(&options);
    evaluate_corpora(&mut report, &options);
    assert!(report
        .exit_result(false)
        .unwrap_err()
        .contains("malformed manifest"));

    let mut short = Inputs::new();
    let bytes = accuracy::fixtures::pcm16_wav(8_000, 1, &vec![0.0; 32_000]);
    fs::write(short.root.join("track.wav"), &bytes).unwrap();
    short.manifest["corpora"][0]["tracks"][0]["audio"]["sha256"] =
        corpus::sha256_bytes(&bytes).into();
    let (report, _) = short.evaluate(false);
    assert!(report
        .exit_result(false)
        .unwrap_err()
        .contains("pre-evaluation exclusion"));
}

#[test]
fn manifest_rejects_paths_counts_labels_and_cross_split_leakage() {
    for path in [
        "../track.wav",
        "/track.wav",
        "C:/track.wav",
        "//host/share/a",
        "dir\\a.wav",
        "./a.wav",
        "a//b.wav",
        "https://host/a.wav",
    ] {
        assert!(corpus::validate_relative_path(path).is_err(), "{path}");
    }
    assert!(corpus::validate_relative_path("audio with spaces/track.wav").is_ok());
    let inputs = Inputs::new();
    let mut manifest: Manifest = serde_json::from_value(inputs.manifest.clone()).unwrap();
    corpus::validate_manifest(&mut manifest).unwrap();
    let mut wrong_count = manifest.clone();
    wrong_count.corpora[0].expected_track_count += 1;
    assert!(corpus::validate_manifest(&mut wrong_count).is_err());
    let mut leaked = manifest.clone();
    let mut development = leaked.corpora[0].clone();
    development.id = "development-corpus".into();
    development.tracks[0].split = corpus::Split::Development;
    let mut evaluation = development.tracks[0].clone();
    evaluation.id = "different-track".into();
    evaluation.recording_id = "new-recording".into();
    evaluation.audio.sha256 = "a".repeat(64);
    evaluation.split = corpus::Split::Evaluation;
    development.tracks.push(evaluation);
    development.expected_track_count = 2;
    leaked.corpora.push(development);
    assert!(corpus::validate_manifest(&mut leaked)
        .unwrap_err()
        .contains("recording leakage"));
    leaked.corpora[1].tracks[0].recording_id = "different-recording-same-audio".into();
    assert!(corpus::validate_manifest(&mut leaked)
        .unwrap_err()
        .contains("audio hash leakage"));
    for value in [
        json!({"schema_version":1,"tempo_bpm":0.0}),
        json!({"schema_version":1,"tempo_bpm":120.0,"beats_sec":[1.0,1.0]}),
        json!({"schema_version":1,"tempo_bpm":120.0,"beats_sec":[-1.0,1.0]}),
        json!({"schema_version":1,"tempo_bpm":120.0,"key":{"pitch_class":12,"mode":"major"}}),
    ] {
        let annotation: Annotation = serde_json::from_value(value).unwrap();
        assert!(annotation.validate(&[MetricKind::TempoAccuracy1]).is_err());
    }
}

#[test]
fn symlink_escape_is_invalid_even_when_the_leaf_is_absent() {
    let mut inputs = Inputs::new();
    let external = Inputs::new();
    let link = inputs.root.join("escape");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&external.root, &link).unwrap();
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_dir(&external.root, &link) {
        if error.raw_os_error() == Some(1314) {
            eprintln!("symlink test requires Windows Developer Mode; mandatory on Unix CI");
            return;
        }
        panic!("symlink creation failed: {error}");
    }
    inputs.manifest["corpora"][0]["tracks"][0]["audio"]["path"] = "escape/absent.wav".into();
    let (report, _) = inputs.evaluate(false);
    assert!(report
        .exit_result(false)
        .unwrap_err()
        .contains("symlink escapes"));
}

#[test]
fn absent_external_inputs_are_explicit_null_skips() {
    let options = options(&["--quick"]).unwrap();
    let mut report = Report::new(&options);
    evaluate_corpora(&mut report, &options);
    assert_eq!(report.corpora.len(), 2);
    assert_eq!(report.metrics.len(), 4);
    assert!(report.exit_result(true).is_ok());
    for metric in report.metrics {
        assert_eq!(metric.classification, "skipped");
        assert_eq!(metric.measured, None);
        assert_eq!(metric.passed, None);
    }
}
