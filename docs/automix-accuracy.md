# AutoMix accuracy protocol

`audio_automix_accuracy` is an offline custom-harness bench. It calls the
public AutoMix `Head` entrypoint with a 60-second cap at the file's native
sample rate and channel count. Synthetic audio tests detector precision;
separately supplied, checksum-verified evaluation corpora test MIR accuracy.

```sh
cargo bench --bench audio_automix_accuracy -- --quick --enforce --out target/accuracy-quick.json
cargo bench --bench audio_automix_accuracy -- --corpus-manifest /data/manifest.json --corpus-root /data --require-corpus giantsteps-tempo --require-corpus ballroom-beats --enforce --out target/accuracy-full.json
```

`--quick` permits synthetic fixtures only. Omitting the manifest also runs
synthetic fixtures and reports external metrics as unavailable. The runner
does not download media. `--require-corpus` is repeatable; it requires every
declared input and every evaluation track of that corpus. Unknown IDs,
conflicting duplicate options, and requirements without a manifest fail.

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

`split` is `development` or `evaluation`; only the latter is scored. Stable
recording IDs and identical audio hashes must not cross the two splits,
including recordings shared by multiple corpora. IDs must be unique within
each corpus. `expected_track_count` equals included tracks plus explicit
exclusions. Each exclusion has a `recording_id` and fixed `reason`, disjoint
from included tracks. Every corpus needs a nonempty evaluation split.

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
independently for each complete corpus. Key scoring is reusable infrastructure;
the public detector currently abstains on key and exposes no key DTO fields.

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

Freeze detector parameters on development evidence before a held-out run.
Pin the manifest digest and required metric names in CI provisioning, then
invoke the offline bench with both tempo and beat corpora required. Missing
inputs or provisioning failure must fail that evaluation job. Archive reports
on failure. A skipped local corpus is not a real-music accuracy claim, and a
local subset is not evidence for a complete published dataset.
