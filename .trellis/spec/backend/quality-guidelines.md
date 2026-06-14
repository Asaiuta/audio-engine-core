# Quality Guidelines

> Code-quality and evidence standards for this crate. The evidence policy below
> is derived from the algorithm audit
> (`.trellis/tasks/06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`).

---

## Evidence Policy (the core rule)

This crate makes audio-quality and performance claims. Every such claim must be
backed by one of:

1. a passing unit/integration test,
2. current benchmark output from `benches/`, or
3. an explicit, honest limitation note.

Concretely:

- **Do not strengthen README / doc claims without current measured evidence.**
  Regenerate the number, or label it as machine/config-specific, or keep the
  limitation note. The README already keeps one such note visible (the
  sample-peak/lookahead limiter is not an intersample-true-peak guarantee) —
  that note stays until the true-peak limiter task proves otherwise with
  measurements.
- **No marketing absolutes.** "Industry-leading", "all algorithms are optimal",
  etc. are forbidden unless a measurement backs the specific claim. The audit
  classifies IIR/FIR EQ, crossfeed, saturation, and FFT convolution as classic,
  useful DSP — not automatically best-in-class.
- **Missing external corpora are skipped, not silently passed.** The EBU Tech
  3341/3342 corpus check is skipped when reference vectors are absent rather
  than reported as a pass. A report-only benchmark is not a conformance gate;
  do not present it as one.

## Forbidden Patterns

- Allocation / locks / logging / I/O / panics on the hot path — see
  `realtime-safety.md`. This is the highest-priority quality rule.
- `unwrap()` / `expect()` / `panic!` in DSP/callback code.
- Resizing channel/sample buffers during processing instead of presizing during
  setup.
- Per-sample coefficient recomputation when it can be done once on a parameter
  change.

## Required Patterns

- New tunable parameters go through the lock-free atomic snapshots in
  `lockfree_params.rs`.
- New DSP processors ship with: unit tests (mono + stereo at minimum), a
  no-steady-state-allocation test (`assert_no_alloc`) for the processing path,
  and a benchmark entry if they touch the callback budget.
- Public names must not conflate distinct guarantees (e.g. "sample peak" vs
  "true peak").

## Testing Requirements

- `cargo test --lib` must pass. The crate already carries ~150 unit tests; new
  behavior must add tests, not rely on existing ones.
- Cover continuity across buffers, reset behavior, silence, and edge inputs
  (non-finite samples, sample-rate changes) where the processor is stateful.
- Run `cargo clippy --all-targets -- -D warnings` clean.

## Benchmark Gate Convention

The quality benches (`benches/`, custom-main `harness = false`) follow a
report-vs-gate contract established by `audio_quality_measurements.rs`. New or
extended benches must keep it:

- **Classify every metric** as `gate` (fails the run), `report` (evidence only,
  never fails), or `skipped` (a gate whose reference inputs are absent — e.g.
  the EBU corpus). A missing corpus is reported as `skipped` with the
  missing-file count, **never a silent pass**.
- **`--enforce`** turns gate failures into a non-zero exit; the diagnostic must
  name the metric and print measured-vs-threshold. Without `--enforce` the bench
  only reports.
- **`--out <path>`** emits machine-readable JSON (classified metric table +
  conditions) so README/doc values are traceable to a specific run.
- **Conservative thresholds.** Gate margins must survive debug/release and
  cross-CPU/compiler variance. Tight gates are only for deterministic
  bit-parity metrics (e.g. `LoudnessMeter` vs `ebur128` at `1e-6 LU`);
  float/FFT/timing-dependent metrics get wide margins or stay `report`. Record
  the observed value and margin rationale in the task's benchmark inventory.
- **No network**, and `--quick` must stay fast for local dev.

## Code Review Checklist

- [ ] Hot path: no alloc/lock/log/IO/panic/unbounded work.
- [ ] Claims in docs/README backed by a test, current bench output, or a
      limitation note.
- [ ] Feature-gated code builds under `--no-default-features` and each feature
      toggled individually.
- [ ] New tunables use the lock-free snapshot mechanism.
- [ ] Tests cover continuity/reset/edge cases, plus a no-alloc assertion.
