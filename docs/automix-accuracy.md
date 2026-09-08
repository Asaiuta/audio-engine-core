# AutoMix accuracy protocol

`audio_automix_accuracy` is an offline custom-harness bench. It calls the
public AutoMix `Head` entrypoint with a 60-second cap at the file's native
sample rate and channel count. Synthetic audio tests detector precision;
separately supplied, checksum-verified evaluation corpora test MIR accuracy.

```sh
cargo bench --bench audio_automix_accuracy -- --quick --enforce --out target/accuracy-quick.json
cargo bench --bench audio_automix_accuracy -- --corpus-manifest /data/manifest.json --corpus-root /data --require-corpus giantsteps-tempo --require-corpus ballroom-beats --enforce --out target/accuracy-full.json
cargo bench --bench audio_automix_accuracy -- --corpus-manifest target/automix-development-data/manifest.json --split development --require-corpus gtzan-mini-development --enforce --out target/accuracy-development.json
```

`--quick` permits synthetic fixtures only. Omitting the manifest also runs
synthetic fixtures and reports external metrics as unavailable. The runner
does not download media. `--require-corpus` is repeatable; it requires every
selected input and every track in the selected split of that corpus. Unknown IDs,
conflicting duplicate options, and requirements without a manifest fail.

`--split evaluation|development` defaults to `evaluation` and requires a
manifest. The runner verifies manifest-wide recording/hash separation but only
opens media and annotation files belonging to the selected split. Development
corpus scores are diagnostic `report` rows, including under `--enforce`;
synthetic correctness gates and required/invalid-input failures still apply.

The JSON report is written before enforcing failed metric gates. Invalid
selected input and incomplete required input fail even without `--enforce`.
A missing optional corpus is an explicit `skipped` metric with `measured`
and `passed` both JSON null. A partially available corpus cannot produce a
passing aggregate. A detector's abstention on valid audio is a scored miss.

## Input format

Manifest schema version 1 is defined by the typed records in
`benches/support/automix_accuracy/corpus.rs`. The following illustrates one
track; replace digest placeholders with SHA-256 of the exact local bytes.

```json
{
  "schema_version": 1,
  "corpora": [{
    "id": "giantsteps-tempo",
    "dataset_name": "GiantSteps Tempo",
    "dataset_version": "pinned source version",
    "source_revision": "immutable source revision",
    "source_url": "upstream provenance URL",
    "license_note": "actual source and local use terms",
    "annotation_format": "automix_annotation_v1",
    "normalization_revision": "pinned normalization revision",
    "split_id": "pinned evaluation split",
    "metrics": ["tempo_accuracy1", "tempo_accuracy2"],
    "expected_track_count": 1,
    "tracks": [{
      "id": "track-1",
      "recording_id": "stable source recording identity",
      "split": "evaluation",
      "audio": {"path": "audio/track-1.wav", "sha256": "<64 lowercase hex digits>"},
      "annotation": {"path": "annotations/track-1.json", "sha256": "<64 lowercase hex digits>"}
    }],
    "exclusions": []
  }]
}
```

All input paths are relative to the corpus root and use `/`. Absolute,
drive, UNC, traversal, backslash, and escaping symlink paths are rejected.
The native root path may contain spaces. Manifest SHA-256, per-file digests,
provenance, normalized annotation revision, split identity, and coverage
counts are retained in the report. The manifest is never silently repaired.

`split` is `development` or `evaluation`; only the explicitly selected split
is scored. Stable recording IDs and identical audio hashes must not cross the two splits,
including recordings shared by multiple corpora. IDs must be unique within
each corpus. `expected_track_count` equals included tracks plus explicit
exclusions. Each exclusion has a `recording_id` and fixed `reason`, disjoint
from included tracks. A corpus may contain only development tracks. An empty
selected split produces null `skipped` metrics and `not_selected` status; it
cannot satisfy `--require-corpus`.

Normalized annotation JSON has `schema_version: 1` and the labels required
by its declared metrics. Undeclared labels may be omitted or null:

```json
{
  "schema_version": 1,
  "tempo_bpm": 127.3,
  "beats_sec": [0.217, 0.688327572663, 1.159655145326],
  "key": {"pitch_class": 0, "mode": "major"}
}
```

Tempo is positive and finite. Beats are strictly increasing, nonnegative,
finite seconds from decoded source origin. Key tonic is 0..11; mode is
`major` or `minor`. A beat corpus must supply actual beat annotations;
GiantSteps tempo labels cannot supply the beat gates by inventing a grid.
For dual-tempo source labels, normalization selects the higher-salience
primary label, with lower BPM winning an exact tie, independently of the
estimator. Keep those rules and their revision beside the manifest.

## Scores and evidence

| Metric | Definition | Minimum | Report-only target |
|---|---|---:|---:|
| `tempo_accuracy1` | Relative error at most 4% | 0.55 | 0.70 |
| `tempo_accuracy2` | Same tolerance against reference x {1/3, 1/2, 1, 2, 3} | 0.90 | 0.95 |
| `beat_f_measure` | One-to-one matches within inclusive 70 ms | 0.55 | 0.70 |
| `beat_amlt` | mir_eval 0.8.2 total continuity, strict 0.175 phase/period errors | 0.70 | 0.85 |
| `key_exact` | Exact tonic and mode | 0.45 | 0.55 |
| `key_mirex_weighted` | Exact 1, fifth 0.5, relative 0.3, parallel 0.2, other/None 0 | 0.60 | 0.68 |

All corpus scores are fractions. Per-track scores are macro-averaged
independently for each complete selected split. Report schema v2 records
`conditions.corpus_split`, `conditions.corpus_acceptance_gates`, each case's
`split`, and per-corpus `development_count`, `evaluation_count`,
`selected_split`, `selected_count` and actual `evaluated_count`. Schema v1
reports remain historical evaluation evidence. Key scoring is reusable
infrastructure; the public detector currently abstains on key and exposes no
key DTO fields.

Coverage is counted by independently decoding the same selected head frames,
including the decoder's actual EOF. The requested cap and container duration
are not substituted for decoded coverage. Both predicted and reference beats
are intersected with that realized `[start,end)` interval and then trimmed at
`>= 5 seconds`. Predictions come solely from the returned BPM and phase.
Inputs too short for this protocol, including beat labels with fewer than
two remaining beats, require explicit exclusions decided before evaluation.

AMLt considers original, offbeat, double, even-half and odd-half references.
It does not add a triple-tempo variation. Each annotation can be matched only
once; a single reference or predicted beat scores zero. Weighted key fifth
means estimated tonic `(reference + 7) % 12` in the same mode; relative means
major to minor at +9 or minor to major at +3.

The committed metric goldens come from `mir_eval==0.8.2`, including 600 key
pairs/abstentions, metrical alternatives, crowded matching, trim boundaries,
and no predictions. Regeneration needs Python, NumPy, SciPy and that pinned
reference version; Rust tests and the bench do not:

```sh
python benches/support/automix_accuracy/generate_metric_goldens.py --reference-root .tmp/automix-metric-reference
cargo test --test automix_accuracy
```

Synthetic audio is deterministic PCM16 generated by a separate synthesis
clock at 44.1/48/96 kHz, with 60-second windows, 60..200 BPM, and explicit
127.3/174.6 cases. Accented 70/140 and 90/180 fixtures supply weaker
subdivisions to identify the annotated beat level. Equal unaccented pulses
alone do not establish a unique perceptual metrical level. Synthetic gates
are BPM error <=0.05, phase error <=10 ms, and full-window drift <=20 ms,
including rounding of the public BPM. Swung beats, off-grid onsets, ramps,
silence, noise, and a short input are separately identified in each report.

## Version 4 estimator

v4 is unreleased pending the coordinated Structure integration. Existing
tempo fields retain their names; `beat_grid_stability` is the only added
field and is JSON null when there is no fitted grid. Recompute cached v3
analysis before applying the new confidence thresholds. The additive JSON
change also affects Rust struct literals in the planned 2.0 release cycle.

The spectral hop is `(sample_rate / 200).clamp(1, 512)` with a 1024-point
symmetric Hann window. The first observation uses the window-center timestamp.
Positive log-magnitude flux has its +/-100 ms local mean removed, then is
rectified and RMS-normalized. ACF alone receives a 10 ms Gaussian blur (at
least one observation, truncated at three sigma). This prevents narrow
off-grid transients favoring an integer-aligned multiple. DP and fitting use
the unblurred ODF, preserving onset precision.

The prior centers on 120 BPM with 1.5-octave sigma, across 55..200 BPM. For
normalized ACF `c` and refined period `p`, harmonic evidence is
`h=(c(p)+.5*c(2p)+.25*c(3p))/1.75`; rank by
`c(p)*h*prior * (1 - .60*c(p/2)^4 - .30*c(p/3)^4)`. Fourth powers limit the
penalty to strong subdivisions; scaling by the candidate's own evidence
prevents an absolute penalty from promoting unrelated weak periodicities.
Parabolic peaks and 1x/2x/3x refinement feed DP tracking, with transition
penalty `100*ln(interval/period)^2`.

At least six supported beats are needed to fit a grid. One robust refit may
exclude timing outliers, but stability still includes every supported beat:
`clamp(1 - RMS_residual/min(period/4, 0.125 seconds), 0, 1)`.
At the .80 stability bar, RMS phase error must not exceed 25 ms, even if a
half-time grid is selected. The grid is discarded when residual reaches the
smaller of one quarter of a beat and 125 ms; zero stability cannot justify
publishing a tempo. With `mean` equal to the mean
prior-weighted ACF in the search range, salience is
`clamp((c(p)*prior-mean)/max(.01,c(p)*prior),0,1)`. This is the fraction of the
selected peak above background, not distance from an ideal correlation of 1.
Below .15 the estimator abstains from fitting. Confidence is salience times
stability times observed-beat
support, or the available salience without a fit; silence/short inputs use
null. Confidence is an evidence score, not a calibrated probability.

Cut snapping uses individual beats when confidence >=.35 and stability >=.80.
The fitted period remains unrounded internally. The subdivision and contrast
corrections use the separate GTZAN mini development set; synthetic
phase/jitter/noise tests constrain the timing contract. These are not
calibrated probability claims. Frozen evaluation per-track results have not
been used for parameter selection.

## Required corpus CI

`.github/workflows/automix-accuracy.yml` prepares pinned public sources through
workflow dispatch or a reusable workflow call. No rehosting of music is needed.
The standard-library Python adapter downloads from the authors, verifies the
source hashes, checks archive paths/types/expanded sizes, and extracts only
matched WAV files. GiantSteps previews remain their original MP3 bytes.
Local preparation uses the same entrypoint (Python >=3.11, about 5 GB of disk):

```sh
python -B benches/support/automix_accuracy/prepare_public_corpus.py
sha256sum --check benches/support/automix_accuracy/public_corpus.sha256
python -B -m unittest discover -s benches/support/automix_accuracy -p 'test_*.py'
```

Defaults are `--source-cache target/automix-corpus-sources`,
`--out target/automix-corpus-data`, and `--workers 4` (1..8). Existing source
files must pass their checksums before reuse. Failed downloads never replace
verified bytes; missing or corrupt sources fail preparation. Only the source
download stage uses the network; the Rust evaluation runner stays offline.

| Corpus | Frozen source | Evaluation scope |
|---|---|---|
| GiantSteps Tempo | `GiantSteps/giantsteps-tempo-dataset` at `d51ab2422e76abacfaa86616a57054bc222ec9fd`, v2 MIREX labels; authors' `cp.jku.at/datasets/giantsteps/backup/` audio mirror | 661 of 664; exclude the three author-declared no-tempo tracks |
| Ballroom beats | `CPJKU/BallroomAnnotations` at `1db08914a8ae15edb01f104046e30bad88effe67`; UPF `ismir2004/contest/tempoContest/data1.tar.gz` audio | 685 of 698; keep the lexicographically first stem of each of the 13 author-declared replica pairs |

The normalization revision is `official-tempo-beats-v1`. GiantSteps uses the
higher-salience label, or lower BPM on an exact tie. Ballroom retains every
native beat timestamp and bar position; bar IDs are discarded only after
validation. All included music is evaluation data; the initial detector's
parameters were selected only on synthetic development fixtures. Exclusions
are recorded before running the detector, never selected by score.

`evaluation-plan.json` records the source hashes, normalization-script hash,
fixed counts and exclusions. `manifest.json` records SHA-256 for every audio
file and normalized annotation; `provenance.json` binds both files and the
archive inspection results. The normalization script has pinned LF endings,
so its identity and manifest bytes agree across Windows and Linux. CI checks
the entire manifest against the committed `public_corpus.sha256`, then runs
both corpus IDs with `--require-corpus` and all four minimum bars with
`--enforce`. It retains the report and provenance on failure, without uploading
audio or annotation files. A changed split or annotation is a reviewable hash
change, not an automatically passing replacement.

Media and native/normalized annotation files stay under ignored `target/` and
outside the published crate. GiantSteps has no standalone repository license
and uses Beatport previews; do not redistribute them. mirdata describes
Ballroom as CC BY-NC-SA 4.0; use it for local noncommercial evaluation and cite
Krebs, Boeck and Widmer (ISMIR 2013). These scopes differ from some published
leaderboards, so scores are not directly comparable without matching their
versions, labels, exclusions and evaluation intervals.

For an independently hosted frozen corpus, the generic provisioner remains
available with `--manifest-url`, `--manifest-sha256`, `--data-url`, and `--out`.
It verifies all downloaded SHA-256 values and both required metric sets.

## First frozen corpus result

The 2026-09-08 Windows/default-feature v4 run evaluated all 661 GiantSteps
and 685 Ballroom tracks, with zero missing/invalid inputs and zero skips.
Manifest SHA-256 is
`dfc3ee46868e3d080b46a3a96025cdff7d49735b18505e4639170c77a6e5a729`.

| Corpus metric | Measured | Minimum | Result |
|---|---:|---:|---|
| GiantSteps Accuracy1 | 0.2602118003 | 0.55 | Failed |
| GiantSteps Accuracy2 | 0.4114977307 | 0.90 | Failed |
| Ballroom F-measure | 0.6385640141 | 0.55 | Passed |
| Ballroom AMLt | 0.6824830941 | 0.70 | Failed |

The enforced command exited 1 after saving the complete report. All 135
synthetic gates still passed, but three of the four real-music gates failed.
GiantSteps had 112 abstentions; even among the 549 published tempo estimates,
Accuracy2 was only 0.4954462659. Removing abstentions or changing confidence
thresholds does not establish acceptable tempo inference.

The full local report is `target/tempo-v4-corpus-validation.json`; aggregate
evidence is retained in the task's `research/tempo-v4-corpus-summary.json`.
Raw Git discovery was unavailable inside the Windows sandbox; compiled source
hashes match the separately verified pre-evaluation freeze. The remote CI
workflow has not run. These results left the Tempo task incomplete and led
to the separate development collection below. This first run must not be
relabeled as independent evidence after tuning on its failures.

## Separate real-music development

The ISMIR 2021 tutorial's GTZAN mini collection supplies 100 additional
recordings from ten genres. Preparation is source-pinned and requires the
original GiantSteps/Ballroom manifest as an overlap guard:

```sh
python -B benches/support/automix_accuracy/prepare_development_corpus.py
cargo bench --bench audio_automix_accuracy -- --corpus-manifest target/automix-development-data/manifest.json --split development --require-corpus gtzan-mini-development --enforce --out target/accuracy-development.json
```

Defaults are `--out target/automix-development-data`,
`--source-cache target/automix-development-sources`, and `--workers 4` (1..8).
`--evaluation-manifest` may point to another local copy of the same pinned
evaluation manifest. Development output must be disjoint from that evaluation
directory. Verified individual downloads survive a failed preparation run.

`gtzan_mini_sources.json` pins the 100 audio Git objects at
`a61439e86c13037011fde8e0f0743ec55c50bce3`. The adapter checks each object's
identity, native WAV coverage and original-file SHA-256. Human labels are
fixed at `fd9cafdebb6426fee41c3718791be61306323dd7`; the ZIP checksum and member
inspection precede reads. Preserve native BPM and beat seconds, including
tempos outside the detector's current range. Exact native-PCM duplicates keep
the lexicographically first identity; every exclusion is explicit. No music
is selected using detector scores or genre-specific success.

`development-plan.json` is written before predictions and binds the source
inventory, normalizer/helper hashes and evaluation-manifest hash. Provenance
retains native coverage, input hashes, exclusions and the overlap-check limit:
recording IDs and original-file hashes cannot prove absence of differently
encoded or offset copies across datasets. Do not call this acoustic
deduplication. All mini music is development data; its scores cannot satisfy
the frozen evaluation's acceptance bars. Music and labels remain ignored and
must not be redistributed in the crate.

Candidate tuning may use this development set and synthetic fixtures. Freeze
source/configuration identity before running the original evaluation again;
retain every result and describe it as repeated frozen-corpus evaluation,
not fresh unseen validation. The task's
`research/development-evaluation-policy.md` records this policy before the
first mini prediction.

The selected candidate improved development Accuracy1/2 from .56/.83 to
.63/.87 and F-measure/AMLt from .6918/.7821 to .7342/.8196. These 100 tracks
remain development diagnostics. A higher-scoring intermediate candidate was
rejected because it violated the off-grid stability contract. The final
candidate preserves the synthetic phase/jitter guarantees by capping the
grid residual scale at 125 ms. The task's
`research/development-candidate-freeze.md` retains the experiment aggregates,
source identities, rejected-candidate reason and frozen verification sequence.

## Repeated frozen corpus result

The 2026-09-08 development candidate was frozen before repeating the same
661 GiantSteps and 685 Ballroom tracks. Both compiled production-source
hashes match the freeze and final development refresh. The evaluation
manifest, exclusions, labels, thresholds and scored denominators are unchanged.

| Corpus metric | First checkpoint | Development candidate | Minimum | Candidate result |
|---|---:|---:|---:|---|
| GiantSteps Accuracy1 | .2602118003 | .2980332829 | .55 | Failed |
| GiantSteps Accuracy2 | .4114977307 | .4508320726 | .90 | Failed |
| Ballroom F-measure | .6385640141 | .6542758404 | .55 | Passed |
| Ballroom AMLt | .6824830941 | .7234320970 | .70 | Passed |

The enforced command returned exit 1: 137 gates passed (135 synthetic plus
both beat metrics), two tempo gates failed, zero skips and zero input errors.
The complete report is `target/tempo-v4-corpus-repeat.json`, SHA-256
`3aca534ce7b9c433eb3cb9a0429b684f13e2450d960774c22e62af04cd57efbb`.
The task retains `research/tempo-v4-corpus-repeat-summary.json` and the prior
report. Environment metadata records verified base HEAD `dcb5baa` plus task
edits; music and per-track labels remain outside committed artifacts.

This is repeated frozen-corpus evaluation after a published baseline, not
fresh unseen validation. Both tempo bars remain substantially unmet, so the
task stays in progress and v4 is not accepted for release. No further
parameter changes were made from these results. Subsequent development must
keep this evidence intact and predeclare any new independent validation data.
The remote corpus workflow has not run.
