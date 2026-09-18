//! Versioned local input contract and integrity validation. No network I/O.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::metrics::Key;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    TempoAccuracy1,
    TempoAccuracy2,
    BeatFMeasure,
    BeatAmlt,
    KeyExact,
    KeyMirexWeighted,
}

impl MetricKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::TempoAccuracy1 => "tempo_accuracy1",
            Self::TempoAccuracy2 => "tempo_accuracy2",
            Self::BeatFMeasure => "beat_f_measure",
            Self::BeatAmlt => "beat_amlt",
            Self::KeyExact => "key_exact",
            Self::KeyMirexWeighted => "key_mirex_weighted",
        }
    }

    pub fn bars(self) -> (f64, f64) {
        match self {
            Self::TempoAccuracy1 => (0.55, 0.70),
            Self::TempoAccuracy2 => (0.90, 0.95),
            Self::BeatFMeasure => (0.55, 0.70),
            Self::BeatAmlt => (0.70, 0.85),
            Self::KeyExact => (0.45, 0.55),
            Self::KeyMirexWeighted => (0.60, 0.68),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub corpora: Vec<Corpus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    pub id: String,
    pub dataset_name: String,
    pub dataset_version: String,
    pub source_revision: String,
    pub source_url: String,
    pub license_note: String,
    pub annotation_format: String,
    pub normalization_revision: String,
    pub split_id: String,
    pub metrics: Vec<MetricKind>,
    pub expected_track_count: usize,
    pub tracks: Vec<Track>,
    pub exclusions: Vec<Exclusion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub id: String,
    pub recording_id: String,
    pub split: Split,
    pub audio: InputFile,
    pub annotation: InputFile,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Development,
    #[default]
    Evaluation,
}

impl Split {
    pub fn name(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Evaluation => "evaluation",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    pub recording_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    pub schema_version: u32,
    pub tempo_bpm: Option<f64>,
    pub beats_sec: Option<Vec<f64>>,
    pub key: Option<Key>,
}

impl Annotation {
    pub fn validate(&self, metrics: &[MetricKind]) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported annotation schema_version (expected 1)".into());
        }
        if self
            .tempo_bpm
            .is_some_and(|bpm| !bpm.is_finite() || bpm <= 0.0)
        {
            return Err("tempo_bpm must be positive and finite".into());
        }
        if let Some(beats) = &self.beats_sec {
            if beats.is_empty()
                || beats.iter().any(|beat| !beat.is_finite() || *beat < 0.0)
                || beats.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(
                    "beats_sec must be nonempty, strictly increasing, finite and nonnegative"
                        .into(),
                );
            }
        }
        if self.key.is_some_and(|key| key.pitch_class > 11) {
            return Err("key pitch_class must be in 0..11".into());
        }
        for metric in metrics {
            let present = match metric {
                MetricKind::TempoAccuracy1 | MetricKind::TempoAccuracy2 => self.tempo_bpm.is_some(),
                MetricKind::BeatFMeasure | MetricKind::BeatAmlt => self.beats_sec.is_some(),
                MetricKind::KeyExact | MetricKind::KeyMirexWeighted => self.key.is_some(),
            };
            if !present {
                return Err(format!("missing label required by {}", metric.name()));
            }
        }
        Ok(())
    }
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn nonempty(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("empty {field}"))
    } else {
        Ok(())
    }
}

fn identity(value: &str) -> Result<(), String> {
    nonempty(value, "identity")?;
    if value
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '/' | '\\' | ':'))
    {
        return Err(format!("invalid identity: {value:?}"));
    }
    Ok(())
}

pub fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains(['\\', ':', '\0'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(path).is_absolute()
    {
        return Err(format!(
            "input path must use relative forward-slash components: {path:?}"
        ));
    }
    Ok(())
}

fn validate_file(file: &InputFile) -> Result<(), String> {
    validate_relative_path(&file.path)?;
    if file.sha256.len() != 64
        || !file
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!("invalid lowercase SHA-256: {}", file.path));
    }
    Ok(())
}

pub fn validate_manifest(manifest: &mut Manifest) -> Result<(), String> {
    if manifest.schema_version != 1 || manifest.corpora.is_empty() {
        return Err("manifest requires schema_version 1 and nonempty corpora".into());
    }
    let mut corpus_ids = BTreeSet::new();
    let mut recording_splits = BTreeMap::new();
    let mut audio_splits = BTreeMap::new();
    for corpus in &mut manifest.corpora {
        identity(&corpus.id)?;
        if !corpus_ids.insert(corpus.id.clone()) {
            return Err(format!("duplicate corpus id: {}", corpus.id));
        }
        for (field, value) in [
            ("dataset_name", &corpus.dataset_name),
            ("dataset_version", &corpus.dataset_version),
            ("source_revision", &corpus.source_revision),
            ("source_url", &corpus.source_url),
            ("license_note", &corpus.license_note),
            ("normalization_revision", &corpus.normalization_revision),
            ("split_id", &corpus.split_id),
        ] {
            nonempty(value, field)?;
        }
        if corpus.annotation_format != "automix_annotation_v1" {
            return Err(format!(
                "{}: annotation_format must be automix_annotation_v1",
                corpus.id
            ));
        }
        let metrics: BTreeSet<_> = corpus.metrics.iter().copied().collect();
        if metrics.is_empty() || metrics.len() != corpus.metrics.len() {
            return Err(format!("{}: empty or duplicate metrics", corpus.id));
        }
        if corpus.tracks.is_empty()
            || corpus.expected_track_count != corpus.tracks.len() + corpus.exclusions.len()
        {
            return Err(format!(
                "{}: expected count mismatch or empty included tracks",
                corpus.id
            ));
        }
        let mut track_ids = BTreeSet::new();
        let mut recordings = BTreeSet::new();
        for track in &corpus.tracks {
            identity(&track.id)?;
            nonempty(&track.recording_id, "recording_id")?;
            if !track_ids.insert(&track.id) || !recordings.insert(&track.recording_id) {
                return Err(format!(
                    "{}: duplicate track or recording: {}",
                    corpus.id, track.id
                ));
            }
            validate_file(&track.audio)?;
            validate_file(&track.annotation)?;
            for (map, id, label) in [
                (&mut recording_splits, &track.recording_id, "recording"),
                (&mut audio_splits, &track.audio.sha256, "audio hash"),
            ] {
                if map
                    .insert(id.clone(), track.split)
                    .is_some_and(|previous| previous != track.split)
                {
                    return Err(format!("development/evaluation {label} leakage: {id}"));
                }
            }
        }
        for exclusion in &corpus.exclusions {
            nonempty(&exclusion.recording_id, "excluded recording_id")?;
            nonempty(&exclusion.reason, "exclusion reason")?;
            if !recordings.insert(&exclusion.recording_id) {
                return Err(format!(
                    "{}: duplicate or included exclusion: {}",
                    corpus.id, exclusion.recording_id
                ));
            }
        }
        corpus.tracks.sort_by(|a, b| a.id.cmp(&b.id));
        corpus
            .exclusions
            .sort_by(|a, b| a.recording_id.cmp(&b.recording_id));
        corpus.metrics.sort();
    }
    manifest.corpora.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(())
}

pub struct LoadedTrack {
    pub track: Track,
    pub audio_path: PathBuf,
    pub annotation: Annotation,
}

pub struct CorpusInputs {
    pub tracks: Vec<LoadedTrack>,
    pub missing: Vec<String>,
    pub integrity_errors: Vec<String>,
    pub missing_track_count: usize,
}

// Check every existing ancestor as well as the leaf, so a missing file under
// an escaping symlink is an invalid input, not an innocent optional skip.
fn resolve_input(root: &Path, relative: &str) -> Result<Option<PathBuf>, String> {
    validate_relative_path(relative)?;
    let resolved_root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("corpus root: {error}")),
    };
    let mut candidate = resolved_root.clone();
    for component in relative.split('/') {
        candidate.push(component);
        match candidate.canonicalize() {
            Ok(resolved) if resolved.starts_with(&resolved_root) => candidate = resolved,
            Ok(_) => return Err(format!("symlink escapes corpus root: {relative}")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{relative}: {error}")),
        }
    }
    if !candidate.is_file() {
        return Err(format!("not a regular input file: {relative}"));
    }
    Ok(Some(candidate))
}

fn checked_file(root: &Path, input: &InputFile) -> Result<Option<PathBuf>, String> {
    let Some(path) = resolve_input(root, &input.path)? else {
        return Ok(None);
    };
    if sha256_file(&path)? != input.sha256 {
        return Err(format!("SHA-256 mismatch: {}", input.path));
    }
    Ok(Some(path))
}

pub fn load_corpus(root: &Path, corpus: &Corpus, split: Split) -> CorpusInputs {
    let mut inputs = CorpusInputs {
        tracks: Vec::new(),
        missing: Vec::new(),
        integrity_errors: Vec::new(),
        missing_track_count: 0,
    };
    // Manifest identities/digests are checked across both splits, but only the
    // selected split's media and labels may enter this run.
    for track in corpus.tracks.iter().filter(|track| track.split == split) {
        let mut paths = Vec::with_capacity(2);
        let mut missing_track = false;
        for input in [&track.audio, &track.annotation] {
            match checked_file(root, input) {
                Ok(Some(path)) => paths.push(Some(path)),
                Ok(None) => {
                    missing_track = true;
                    inputs.missing.push(input.path.clone());
                    paths.push(None);
                }
                Err(error) => {
                    inputs
                        .integrity_errors
                        .push(format!("{}: {error}", track.id));
                    paths.push(None);
                }
            }
        }
        inputs.missing_track_count += usize::from(missing_track);
        // Validate a present annotation even when its corresponding audio is
        // missing. Invalid selected input must never become an optional skip.
        if let Some(annotation_path) = &paths[1] {
            let annotation = fs::read(annotation_path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<Annotation>(&bytes).map_err(|error| error.to_string())
                })
                .and_then(|annotation| annotation.validate(&corpus.metrics).map(|()| annotation));
            match annotation {
                Ok(annotation) => {
                    if let Some(audio_path) = paths[0].take() {
                        inputs.tracks.push(LoadedTrack {
                            track: track.clone(),
                            audio_path,
                            annotation,
                        });
                    }
                }
                Err(error) => inputs
                    .integrity_errors
                    .push(format!("{}: invalid annotation: {error}", track.id)),
            }
        }
    }
    inputs.missing.sort();
    inputs.missing.dedup();
    inputs.integrity_errors.sort();
    inputs
}
