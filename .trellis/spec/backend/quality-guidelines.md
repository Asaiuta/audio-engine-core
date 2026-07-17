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
  limitation note. The limiter now defaults to 4x-oversampled true-peak
  detection (measured: an intersample-stress input at +0.10 dBTP is pulled to
  -1.00 dBTP); the remaining visible limitation is that the limiter runs at
  source rate, so the full output-chain true-peak probe stays report-only
  because downstream resampling/quantization can re-introduce intersample
  peaks.
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
- Stateful DSP adapters must distinguish scalar/control updates from
  coefficient-geometry updates. Mix/strength/enabled changes should preserve
  filter history unless the processor contract explicitly requires a reset;
  sample-rate, cutoff, latency-window, or topology changes may reset state when
  documented and tested.
- Streaming processor implementations and drivers must follow
  `streaming-lifecycle.md`: validated zero-copy blocks, complete 1:1 in-place
  progress, explicit backpressure, idempotent finish, native-state reset, and
  rate-tagged latency/tail timing. Use `process_checked` / `finish_checked` at
  chain/direct-driver boundaries.
- New DSP processors ship with: unit tests (mono + stereo at minimum), a
  no-steady-state-allocation test (`assert_no_alloc`) for the processing path,
  and a benchmark entry if they touch the callback budget.
- Public names must not conflate distinct guarantees (e.g. "sample peak" vs
  "true peak").

## Saturation Quality Modes

`SaturationQuality::Direct` is the legacy source-rate waveshaper. Higher-quality
antialiasing modes are explicit public modes, currently
`SaturationQuality::Oversampled2x` and `SaturationQuality::Oversampled4x`.

Contracts to preserve when changing or extending this path:

- Public control must flow through both `Saturation::set_quality(...)` and
  `AtomicSaturationParams::set_quality(SaturationQualityValue::...)` so direct
  processor use and callback-chain use expose the same modes.
- Per-channel oversampling/FIR history must be sized by
  `Saturation::set_channel_count(...)` during setup. Do not resize, allocate,
  log, or lock inside `process_with_channels`, `process_fullband_oversampled`,
  or high-pass processing.
- Quality-mode claims need objective evidence in
  `audio_quality_measurements.rs`. The current gate is
  `saturation_oversampled4x_alias_reduction`, which compares folded harmonic
  alias energy from the Direct and Oversampled4x Tube paths at equivalent
  drive/mix settings.
- Callback-budget changes need `audio_callback_chain_perf`; the active DSP
  scenario intentionally enables `SaturationQualityValue::Oversampled4x` so the
  measured 512-frame callback cost includes the upgraded path.

Tests required for this contract:

- Unit tests cover all saturation types across Direct/Oversampled2x/Oversampled4x.
- High-pass mode, multichannel setup, reset, and sample-rate changes remain
  finite and bounded.
- A no-steady-state-allocation assertion covers the oversampled processing path
  after setup.

## FFT Convolution Routing

`FFTConvolver::new(...)` owns both convolution strategies: the overlap-save
engine for short/medium FIRs and the uniform partitioned engine for long impulse
responses. Keep the public constructor as the routing point so callers,
adapters, FIR EQ, and benches do not need to duplicate IR-length logic.

Contracts to preserve when changing this path:

- `PARTITIONED_CONVOLUTION_IR_THRESHOLD` and
  `PARTITIONED_CONVOLUTION_PARTITION_SIZE` are public evidence-backed routing
  constants. Changing either requires fresh `audio_convolver_perf --quick` and
  `audio_fir_eq_perf --quick` output plus README/doc updates for any cited
  values.
- FIR EQ tap counts must stay on `ConvolutionStrategy::OverlapSave` unless a
  benchmark proves the partitioned path does not regress the FIR apply budget.
- Partitioned processing must precompute IR spectra, history buffers, FFT plans,
  and in-place scratch buffers during construction. `process_into` and
  `process_inplace` must not allocate, lock, log, perform I/O, or do unbounded
  work after setup.
- Correctness tests must compare the partitioned output against the overlap-save
  reference within a fixed tolerance for stereo and at least one mono/surround
  channel count, and cover cross-buffer continuity, reset, and in-place paths.

## Testing Requirements

- `cargo test --lib` must pass. The crate already carries ~150 unit tests; new
  behavior must add tests, not rely on existing ones.
- Cover continuity across buffers, reset behavior, silence, and edge inputs
  (non-finite samples, sample-rate changes) where the processor is stateful.
- Offline finalize changes must cover last-frame impulse survival,
  raw-vs-compensated content equivalence, finite tail propagation through every
  downstream rate domain, and timing metadata. Unknown-tail tests must prove
  both block-size-independent retained output and early energy termination; a
  test that generates the full safety maximum and only checks post-trim samples
  is insufficient performance evidence.
- When fixing adapter-level parameter continuity, compare the adapter against a
  direct processor reference that preserves history, and include a reset
  reference when possible so the test proves it would catch the old click/glitch
  behavior.
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

## Release Documentation Checklist

Use this when changing `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`,
`NOTICE`, Cargo features, or public crate metadata.

### 1. Scope / Trigger

- Trigger: release-facing documentation, public API wording, package metadata,
  feature flags, examples, or native dependency claims.
- Target files: `Cargo.toml`, `src/lib.rs`, `README.md`, `CHANGELOG.md`,
  `CONTRIBUTING.md`, `NOTICE`, and `examples/`.

### 2. Signatures

- Feature checks:
  - `cargo build`
  - `cargo build --no-default-features`
  - `cargo build --no-default-features --features http`
  - `cargo build --no-default-features --features loudness-db`
- Docs/package checks:
  - `cargo doc --no-deps`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`
  - `cargo package --allow-dirty`

### 3. Contracts

- `http` controls the optional `reqwest` dependency and network error surface.
- `loudness-db` controls the optional `rusqlite` dependency and SQLite cache
  types.
- SoXR is a required native dependency today because `src/processor/resampler.rs`
  is part of the core crate and `soxr` is not optional in `Cargo.toml`.
  Therefore `default-features = false` removes HTTP and SQLite, but it does not
  remove libsoxr or the resampler API.
- README quality/performance numbers must name the benchmark or test family that
  produced them and must preserve explicit limitation notes for report-only
  probes.

### 4. Validation & Error Matrix

- A feature combination fails to compile -> fix the `#[cfg(feature = "...")]`
  boundary or update the feature contract before release.
- `cargo doc` emits a public warning -> treat it as a release blocker, even if
  the command exits successfully.
- `cargo package` fails while verifying the package -> fix package contents or
  metadata.
- `cargo package` fails only while updating the registry/index in a sandboxed
  environment -> rerun in a normal network/credential environment before
  classifying it as a package-content failure.

### 5. Good/Base/Bad Cases

- Good: "`default-features = false` removes HTTP and SQLite dependencies; SoXR
  remains required because resampling is core."
- Base: "Both Cargo features are default-on and can be disabled independently."
- Bad: "`default-features = false` creates a dependency-free DSP-only build" or
  "building without resampling avoids libsoxr" while `soxr` is still required.

### 6. Tests Required

- Build default, no-default, and each optional feature independently.
- Run all-features tests and no-default-features tests before publishing.
- Run examples listed in the README.
- Run `cargo package --allow-dirty` or `cargo publish --dry-run`.

### 7. Wrong vs Correct

#### Wrong

```text
If you build without the resampling functionality, libsoxr is not linked.
```

#### Correct

```text
SoXR-backed resampling is part of the core crate today. No Cargo feature
currently disables the `soxr` dependency, so building the crate links libsoxr
even when default features are disabled.
```
