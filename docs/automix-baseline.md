# AutoMix retained baseline and integration status

The production implementation remains **v10/P3**. The complete Metrical
`harmonic/hist_15 + P3` package is retained research evidence with
`production_promoted: false`. Its native performance targets passed; its
GiantSteps Acc2 acceptance gate and independent incremental-benefit evidence
do not permit promotion.

The repository's dated, hash-verifiable handoff is
`.trellis/tasks/09-14-automix-baseline-exposure-governance/research/`.
`baseline.json` separates production, research, performance and quality status;
`evidence-lock.json` seals the retained files. This is a repository handoff,
not a newly published crate release or deployed service.

## Production API

The public exports in `src/analysis/mod.rs` lead to these synchronous functions:

```rust
pub fn analyze_automix(
    location: MediaLocation,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
) -> Result<AutomixAnalysis, AutomixError>;

pub fn analyze_automix_with_cancel(
    location: MediaLocation,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<AutomixAnalysis, AutomixError>;
```

- Run analysis on a worker, outside the audio callback. It performs decoding,
  allocation and computation; the caller owns scheduling and result publication.
- `Head` analyzes the bounded head window. Default `Full` adds the bounded tail
  window; it does not decode the complete intervening track.
- `max_analyze_time_sec` caps source-audio duration per window, not wall-clock
  execution time. `duration` is the placement timeline, not decoded coverage.
- Cancellation is cooperative and one-way through `DecodeCancelToken`.
  Application scheduling must handle cancellation/errors and stale results;
  the token is not a hard deadline or an immediately preemptive interrupt.
- `AutomixAnalysis.version` is **4**. The research label **v10** names the
  detector family; it is not this serialized DTO version.
- `bpm` and `first_beat_pos` may be absent. `bpm_confidence` is an evidence
  score, not a calibrated probability of correct tempo. A present constant
  tempo does not establish a stable grid; inspect `beat_grid_stability`.
- `first_beat_pos` is in absolute source seconds and is not a downbeat claim.
  P3 conservatively reports a short isolated head event at the origin while
  retaining the internal fitted grid used for stability and cut snapping.

No API rename or compatibility wrapper was needed for this consolidation.
Consumers should use these existing contracts and preserve abstentions.

## Native research package

The retained package is
`target/tempo-h2-native-tail/package-2780ace98675/`. Its Prefix, expert and tail
DLLs preserve the frozen models and current P3 adapter. It still requires
Python orchestration, NumPy, SciPy's exact float32 FFT/mel frontend, SoXR HQ,
threadpoolctl and declared BLAS/DLL dependencies. It is not the crate's default
execution path and has no established stable public cross-platform ABI or
integrated cancellation contract.

The historical serial benchmark measured the complete Metrical/P3 paths:

| Measurement | Reference | Native | Scope |
|---|---:|---:|---|
| D2 conditional mean | 505.188 ms | 251.169 ms | D2 calls, 60 measurements per path |
| Metrical weighted warm mean | 350.145 ms | 294.189 ms | Fixed strata and population weights |
| Cold launch to first result mean | 4,859.965 ms | 1,316.577 ms | Nine fresh processes per path |
| Cold mean peak RSS | 412.617 MiB | 183.889 MiB | Isolated processes |

All four predeclared native performance targets passed. These are fixed-sample
measurements, not a universal latency/memory guarantee. Cold startup retained
the OS file cache. There is no new performance run in this consolidation.

## Quality and data boundary

Latest GiantSteps Acc2 is **583/661 = 88.1997%**, below the fixed **90%** gate
(at least 595 hits). Paired improvement over D1 does not override that failure.
Harmonix's 388 evaluations established no Metrical increment over Selective;
GuitarSet's 358 evaluations also did not establish that increment.

The exposure registry contains these historical uses:

| Role | Source rows | Permitted future use in this handoff |
|---|---:|---|
| MTG fit | 561 | Fit/calibration/selection with original folds and a fixed protocol |
| MTG validation | 588 | Frozen regression; no new selection |
| GTZAN guard | 100 | Frozen regression; no new selection |
| SMC/Hainsworth domain training | 439 | Retained historical training/regression; no new fit authorization |
| GiantSteps/Ballroom evaluation | 1,346 | Frozen regression; no tuning or unseen claim |
| Harmonix evaluation | 388 | Frozen regression; no tuning or unseen claim |
| GuitarSet evaluation | 358 | Frozen regression; no tuning or unseen claim |
| Source-preparation quarantine | 526 | Identity/overlap screening; excluded from model scoring |

There are **3,780 model-exposed rows**, plus **526 quarantined source rows**:
**4,306** source IDs in **4,275** conservative alias/content components.
Components are exclusion groups, not proof of unique songs or artists. The 439
general-domain rows have feature identities, not original audio hashes.
The arithmetic 1,346 + 388 equals 1,734; it does not define the full exposure pool.

Use the task's `governance.py preflight` before a future study. It verifies
input bytes and recording/component roles. Only the original complete MTG fit
population, with folds **168/191/202**, may select new mechanisms or parameters.
The command is an offline workflow gate; it does not intercept arbitrary
commands or modify frozen runners. Unknown audio does not automatically pass
independent-evaluation readiness.

Candidate-risk R1 has already stopped: masked Acc1 precision was 456/479
(95.1983%), but actual recoveries stayed 5/9 and every actual cached output
equalled C0. Do not restart R1 or lower its probability cutoff to recover a
named recording. The task's research plan specifies the next mechanism-study
boundary, and its independent-corpus protocol specifies the missing source work.

## Gates before an H2 production integration

1. Establish a fixed, fit-selected hypothesis with complete recording folds,
   then demonstrate improvement on a new licensed, aligned and overlap-audited
   independent population. Keep the existing exposed regression gates.
2. Define a private backend/resource boundary with exact frontend/model
   identities, ownership, cancellation/error handling, deterministic teardown,
   supported platforms and off-callback scheduling before adding a public API.
3. Retain public abstention/version/timing semantics and cache identity. Any
   public contract change requires the project's API/SemVer review and tests.
4. Run complete reference/native parity, physical/absolute-first-beat checks,
   appropriate Rust feature/quality gates, and serial cost/memory measurements
   on the proposed final integration. Historical research checks do not certify
   an integration that has not been implemented.

Production v10/P3 remains available through the existing API while this work
proceeds. See `docs/automix-accuracy.md` for the scoring contract and the dated
task report for the evidence inventory and actual validation performed.
