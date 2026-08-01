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
  documented and tested. Crossfade branch ownership, config publication,
  coefficient-oracle, and sample-rate state boundaries additionally follow
  `dsp-state-correctness.md`.
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
- Armed Direct/2x/4x modes share a four-source-frame timeline. Oversampled
  paths filter only `waveshaped - interpolated` and add that residual to the
  delayed dry signal. Every oversampled phase advances FIR history, but only
  one 17-/33-tap dot product is evaluated per source output.
- Setup/reset-time hard bypass is zero-latency. Runtime effect enable and
  quality changes retain the four-frame timeline and use a 32-source-frame
  complementary smoothstep. Sparse automation is caller-borrowed, sorted by
  block-relative frame offset, and never owned or allocated by the callback.
- Quality-mode claims need objective evidence in
  `audio_quality_measurements.rs`. Gates include
  `saturation_oversampled4x_alias_reduction` and
  `saturation_oversampled4x_fundamental_delta`: folded alias energy must
  improve by at least 6 dB and the wanted fundamental may not fall more than
  0.5 dB versus Direct at equivalent drive/mix settings.
- Callback-budget changes need `audio_callback_chain_perf`; the active DSP
  scenario intentionally enables `SaturationQualityValue::Oversampled4x` so the
  measured 512-frame callback cost includes the upgraded path.
- Fixed-ratio or const-generic oversampling kernels may specialize the hot path
  at the block boundary. The specialization must preserve the transfer
  function, FIR coefficients, phase order, residual-only topology, latency,
  and per-channel state-update order; it must not introduce an approximate
  waveshaper or a runtime allocation/lock/logging path.
- Every specialized oversampling kernel requires a dynamic-reference oracle
  that compares output and updated state sample-by-sample (bit-for-bit for
  deterministic f64 arithmetic). A benchmark win without this parity check is
  insufficient evidence for retaining the specialization.
- A mirrored FIR history must expose a contiguous newest-to-oldest window so
  the coefficient and accumulator order remains unchanged. Symmetric FIR
  coefficients do not permit reversing the f64 reduction: mathematical
  equivalence is weaker than the bit-for-bit oracle required for this path.

Tests required for this contract:

- Unit tests cover all saturation types across Direct/Oversampled2x/Oversampled4x.
- High-pass mode, multichannel setup, reset, and sample-rate changes remain
  finite and bounded.
- A no-steady-state-allocation assertion covers the oversampled processing path
  after setup.
- Specialized-vs-dynamic kernel tests cover each oversampled 2x/4x phase/tap
  combination, while the Direct path keeps its direct-saturation oracle;
  high-pass processing is covered where the specialized kernel applies.
- Mirrored-history changes include an independent legacy circular-ring oracle
  that crosses multiple wraps and repeats after reset/initialization.
- Below-threshold all-mix identity, partial-mix affine behavior, high-pass
  topology/selectivity, harmonic spectrum, exact finite support, irregular
  chunks, event offsets, retargeting, and finish-near-transition have
  independent numerical oracles.

## FFT Convolution Routing

`FFTConvolver::new(...) -> Result<FFTConvolver, ProcessError>` owns both
convolution strategies: the overlap-save engine for short/medium FIRs and the
uniform partitioned engine for long impulse responses. Keep the public constructor as the routing point so callers,
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
- A real-valued partitioned implementation may store only bins `0..=N/2` for
  real audio. The DC and Nyquist bins remain single real values, and inverse
  normalization must use the full FFT length. Real-FFT backend failures in the
  callback path use static `ProcessError::Backend` diagnostics; they must not
  allocate an error string or panic.
- Partitioned IR spectra and input-history spectra use row-major contiguous
  buffers. A time-distributed tail accumulator may consume older partition
  rows before the newest row is committed, but its quantum order must be
  deterministic and independent of callback chunking. Direct
  channel/partition/history cursors avoid nested-`Vec` indirection and
  per-quantum division/modulo. Any layout or accumulation-order change must
  retain the overlap-save/direct oracle tolerance, measure the maximum delta
  against the prior engine, and include a same-machine throughput comparison
  for both small and large tail rings.
- Work spreading inside a fixed partition period is driven by frames advanced,
  never by callback count. Persistent spectra and scheduler cursors are
  preallocated, cleared after inverse FFT, and cleared together on reset. The
  older-pass schedule must complete by construction before the boundary; use a
  debug assertion and fixed 64/128/256/512 plus non-dividing irregular chunk
  tests rather than a boundary-time fallback loop. The newest history slot is
  unavailable until the forward commit and therefore stays on the boundary
  path.
- The 2026-07-22 same-machine sweep selected the 1024-frame partition for the
  real-FFT tail. At 8192/65536 taps and six channels, 512 frames measured
  47.35/304.19 ns/sample, 1024 measured 28.99/130.08, and 2048 measured
  31.54/179.49. The smaller and larger alternatives also had higher 64-frame
  callback p99 utilization, so changing this public constant requires a fresh
  sweep rather than a speculative tuning change.
- Constructor and processing entries reject empty/zero-channel/incomplete
  interleaved geometry with typed errors. Public realtime-capable wrappers do
  not retain a panicking compatibility path.
- Correctness tests must compare the partitioned output against the overlap-save
  reference within a fixed tolerance for stereo and at least one mono/surround
  channel count, and cover cross-buffer continuity, reset, and in-place paths.

## Testing Requirements

- Both `cargo test --all-features` and
  `cargo test --no-default-features --features rubato` must pass; new behavior
  must add focused regressions, not rely on test count. Bare
  `--no-default-features` is intentionally unsupported because the crate
  requires either the `soxr` or `rubato` resampler backend.
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
- Run `cargo clippy --all-targets --all-features -- -D warnings` and
  the following pure-Rust matrix clean:

  ```bash
  cargo clippy --all-targets --no-default-features --features rubato -- -D warnings
  ```

## Decoder Upgrade Evidence

When changing the Symphonia version or decoder staging path, a decoder
performance claim requires a same-machine before/after comparison rather than
an upstream release-note citation alone.

- Compare the real crate configuration in release mode. Record the compiler,
  target, CPU, base revisions, dirty state, crate feature set, and the exact
  Symphonia feature sets; 0.6's default `opt-simd` means `all` is not a feature
  parity claim with 0.5.
- Prefer one comparator process that links distinct old/new package identities
  and alternates versions in ABBA order after untimed warmups. If Cargo rejects
  two same-name path packages in one lockfile, a temporary baseline package
  rename is acceptable when decoder source and build configuration are
  otherwise unchanged.
- Time `StreamingDecoder::open`/probe separately from borrowed streaming decode
  (`decode_next_borrowed`). Keep `decode_all` allocation timing separate; do not
  merge it into the realtime-oriented streaming number.
- Retain raw trial samples and report min/median/p95/max. Validate every input
  before timing with sample rate, channel count, frame count, finite samples,
  and a full output hash or pointwise delta. If output frame counts differ,
  timing comparisons are invalid; if lossy codec floats differ, report the
  maximum and RMS deltas explicitly.
- Use multiple codec/channel workloads and state cache temperature and corpus
  provenance. A decoder-only result is not evidence of end-to-end playback or
  callback latency, and missing codec/cold-disk/network coverage must remain a
  visible limitation.
- When comparing Symphonia native gapless with the crate-owned manual path,
  validate both uninterrupted output and a post-seek chunk. Classify the
  native/manual comparison as report-only until real fixtures cover every
  stateful codec in scope; an Ogg/Vorbis reset mismatch must remain visible and
  must not be hidden by widening a sample-delta threshold. Keep missing MP3,
  CAF, or other format fixtures explicitly `skipped` in the JSON report.
- The production owner policy is an explicit codec allowlist, not a container
  extension check: MP3 and Vorbis use native decoder trimming; other codecs use
  Track fallback. An owner-policy change requires source inspection proving the
  decoder consumes `AudioDecoderOptions::gapless`, a focused allowlist test,
  and an enforced real-codec comparison of sequential output and post-seek
  output. A native reset packet that decodes to zero frames is internal control
  flow and must not surface as a successful empty streaming chunk.

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

## Scenario: Versioned Benchmark Evidence And Compatible Baselines

### 1. Scope / Trigger

- Trigger: changing `audio_quality_measurements`,
  `audio_callback_chain_perf`, `audio_callback_tail_perf`,
  `audio_resampler_streaming_perf`, shared `audio_convolver_perf`,
  `audio_fir_eq_perf`, `benches/support/` code, benchmark CI wiring, or a
  documented timing claim.
- These are custom-main benches (`harness = false`). Benchmark plumbing stays
  bench-local; do not expose report helpers as crate public API.

### 2. Signatures

```bash
cargo bench --bench audio_quality_measurements -- \
  --quick --enforce --out <quality.json>

cargo bench --bench audio_callback_chain_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]

cargo bench --bench audio_callback_tail_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>] \
  [--max-p99-regression-pct <non-negative-finite-pct>] \
  [--max-p999-regression-pct <non-negative-finite-pct>] \
  [--pinned] [--pin-core <logical-core>]

cargo bench --bench audio_output_render_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]

cargo bench --bench audio_resampler_streaming_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]

cargo bench --bench audio_fir_eq_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]

cargo bench --bench audio_convolver_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>] \
  [--pinned] [--pin-core <logical-core>]
```

Omitting `--quick` / `--heavy` selects full mode. Quality supports quick/full;
the six performance probes additionally support heavy. Environment overrides
are `AUDIO_BENCH_REVISION`, `AUDIO_BENCH_DIRTY`, `AUDIO_BENCH_RUSTC`,
`AUDIO_BENCH_RUSTC_VERBOSE`, `AUDIO_BENCH_TARGET`, `AUDIO_BENCH_CPU`, and
`AUDIO_BENCH_PROFILE`; `GITHUB_SHA` is a revision fallback.

### 3. Contracts

- Every report has `schema_version`, stable `probe`, `generated_unix_ms`,
  `mode`, `environment`, and explicit measurement `conditions`.
- Environment contains revision, nullable dirty state, rustc, target, OS,
  architecture, CPU, Cargo profile, and compiled feature names. Failed probes
  produce `"unknown"` / `null`; they do not abort a report without a baseline.
- The environment feature list includes the compiled resampler backend as
  `resampler-{RESAMPLER_BACKEND_NAME}` (pushed by `BenchEnvironment::capture()`),
  so soxr and rubato reports are never environment-compatible. Reports recorded
  before backend labeling landed (2026-07-21) are incompatible with new
  baselines by design.
- Any report label that names an algorithm or backend (the resampler bench's
  `conditions.algorithm`, the `case_key` `algorithm=` segment, backend-specific
  scenario descriptions) must derive from
  `audio_engine_core::RESAMPLER_BACKEND_NAME`, never a hard-coded backend
  string. The one schema-stability exception is the render report's
  `native_soxr_bytes` field name. Documented timing rows (docs/quality.md,
  README) must name the backend and run date that produced them.
- Performance cases have unique stable `case_key` values, declared iterations
  and trials, raw trial samples, and min/median/nearest-rank-p95/max. Callback
  utilization uses the device-buffer deadline. Resampler utilization is only a
  source-buffer realtime reference and must be named as such. FIR regeneration
  compares ns/regeneration while FIR apply compares ns/sample; the case key and
  payload must state that primary unit explicitly.
- Keep callback throughput and tail evidence separate.
  `audio_callback_chain_perf` retains its trial-average schema, case keys, and
  baseline meaning. `audio_callback_tail_perf` retains one raw `Instant`
  duration per callback while both probes obtain the three canonical scenarios,
  64/128/256/512-frame matrix, synthetic corpus, and chain configuration from
  the shared bench-local callback fixture.
- The callback-tail timer includes input copy, `DspChain::process`, and output
  `black_box`, but excludes construction, warmup, validation, and report I/O.
  It reports min/median/nearest-rank-p95/p99/p99.9/max, deadline-utilization
  summaries, and an untrimmed missed-deadline count/rate. The deadline is
  `frames / 48_000 Hz`; quick/full/heavy retain 4,000/20,000/100,000 callbacks
  per case.
- Callback-tail timing gates classify only the two active-chain scenarios.
  Their default compatible-baseline limits are 10% median, 20% p99, and 30%
  p99.9. The clock-quantized bypass scenario is report-only, but its raw
  samples, max, and missed-deadline evidence remain present.
- Direct convolver throughput trials must run long enough that short overlap-save
  cases are not dominated by timer quantization; the maintained quick workload
  uses a 2048-frame buffer and 512 base iterations. Callback distributions keep
  every raw sample, including scheduler outliers; never replace max with a
  best-of-N value.
- The convolver's Windows-only `--pinned` mode sets
  `HIGH_PRIORITY_CLASS`, pins the benchmark thread to one logical core, and
  sets `THREAD_PRIORITY_HIGHEST` before collecting samples. Reports add
  `conditions.pinned` and `conditions.pin_core`, making pinned and unpinned
  baselines incompatible. `--pin-core` without `--pinned`, a missing/non-numeric
  core, a core outside the affinity-mask width, or a failed Windows scheduling
  call is a named error.
- Callback-tail baselines are stricter: supplying `--baseline` without
  `--pinned` is rejected before sampling. A pinned report records the requested
  core plus the verified processor group, affinity mask, process priority
  class, and thread priority. Baseline and candidate must contain the same
  effective state. Affinity and priority do not eliminate interrupts, DPCs,
  frequency changes, or scheduler outliers, so every sample stays retained.
- In pinned `--enforce` mode, the 65536-tap, 6-channel, 64-frame callback case
  must be present and pass p99 <= 40% and max <= 50% of its deadline. These are
  machine-local task gates, not portable performance claims. Affinity cannot
  prevent every interrupt: if unrelated cases simultaneously show
  multi-millisecond pauses, retain the failed JSON, identify host load, and
  rerun on a quiet host rather than weakening the gate or deleting outliers.
- Quality keeps `gate` / `report` / `skipped` distinct. Full-output points copy
  `RenderedOutput` rendered frames, algorithmic latency, semantic tail, and
  truncation fields directly. Missing external corpus counts remain visible.
- A requested baseline must match schema, probe, mode, conditions, complete
  case set, rustc, target, OS/architecture, CPU, profile, and features. Unknown
  required environment fields are not comparable. Revision and dirty state may
  differ and are retained for traceability.
- The default median regression limit is 10%: exactly +10% passes and any
  greater regression fails under `--enforce`. Without `--baseline`, absolute
  timing is report-only; `--enforce` still validates work and report integrity.
- Task-critical callback acceptance is stricter than the generic gate: both
  512-frame active-chain medians may regress by at most 3%, their relative p95
  deadline utilization by at most 5%, and every isolated Saturation 4x block
  size must have a strictly lower median than a compatible baseline.
- Output-render reports execute transparent, isolated IIR, isolated Saturation
  4x, finite Convolver, complete equal-rate, and complete resampled cases at
  64 and 4096 frame blocks. They record peak temporary bytes excluding final
  output capacity and require candidate temporary memory to be no larger than
  a compatible baseline. Fixed-stage temporary memory must remain bounded as
  duration grows.
- Shared default-feature CI runners run all nine quick reports and upload JSON;
  the pure-Rust runner additionally executes decoder, component, and lifecycle
  reports under the Rubato-only feature set. Neither runner uses a cross-run
  absolute nanosecond gate without an explicitly supplied compatible baseline.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Unknown CLI option, missing path, negative/non-finite threshold | named argument error |
| `--pin-core` without `--pinned`, missing value, or non-numeric value | named pinned-probe argument error |
| Callback-tail `--baseline` without `--pinned` | reject before collecting samples |
| Pinned logical core exceeds the platform affinity-mask width | reject before shifting the mask |
| Windows priority/affinity call fails or effective state differs | abort before collecting pinned evidence |
| Empty, non-finite, or non-positive trial sample | report construction error |
| Callback-tail raw vector length differs from declared callbacks | report-integrity failure naming declared and retained counts |
| Duplicate/missing case key | baseline comparison rejected with both case sets named |
| Corrupt JSON | deserialization error naming the file and report type |
| Schema/probe/mode/conditions mismatch | comparison rejected before percentages are computed |
| Required environment mismatch or `unknown` | comparison rejected with each incompatible field |
| Baseline and candidate compiled with different resampler backends | comparison rejected via differing `resampler-*` feature entries |
| Candidate median exactly 10% slower | comparison passes |
| Candidate median more than 10% slower | enforced failure names case, baseline, candidate, regression, and threshold |
| Active callback-tail p99 exceeds +20% or p99.9 exceeds +30% | enforced failure names case, metric, values, regression, and threshold |
| Bypass callback-tail changes under a compatible baseline | retain as report-only; do not create timing comparisons for it |
| Pinned callback-tail compared with an unpinned/differently pinned report | reject as a conditions mismatch before percentages are computed |
| 512-frame active callback median exceeds +3% or p95 utilization exceeds +5% | task acceptance failure even when generic +10% would pass |
| Any isolated Saturation 4x candidate median is not lower | strict-improvement failure |
| Render temporary bytes grow with duration for a fixed scenario/block | memory scaling gate failure |
| No baseline on a shared runner | timing remains report-only; work/report gates still run |
| Pinned convolver target case is absent | enforced failure names the required IR/frame/channel tuple |
| Pinned convolver p99 > 40% or max > 50% | enforced failure names the case, measured value, and threshold |
| EBU vectors absent | `skipped` with missing-file count, never pass/conformance |

### 5. Good / Base / Bad Cases

- Good: compare two reports from the same compiler, target, CPU, profile,
  features, mode, conditions, and case set; allow revisions to differ.
- Base: generate a quick CI artifact with `--enforce --out` and no baseline;
  deterministic quality/work checks are enforced while timing is evidence.
- Good: keep 48,000 quick callback-tail samples, classify the eight active
  cases for median/p99/p99.9 comparison, and leave all four bypass cases
  report-only without removing their outliers.
- Good: collect a Windows convolver max/p99 gate with `--pinned --enforce`,
  record the selected core in JSON, and keep a load-contaminated failed report
  separate from the quiet-host acceptance report.
- Bad: compare two `cpu = "unknown"` reports, compare debug with release, or
  call a source-buffer resampler percentage a device callback utilization.
- Bad: hard-code "SoXR" into a bench label that also compiles under the rubato
  backend, or compare a `resampler-soxr` report against a `resampler-rubato`
  baseline.
- Bad: cite min/best-of-N as representative performance or turn a missing EBU
  corpus into a successful conformance claim.
- Bad: run with `--pin-core 2` but omit `--pinned`, compare a pinned report to
  an unpinned baseline, or discard a raw max because it missed the gate.
- Bad: add per-callback timers to `audio_callback_chain_perf`, silently change
  its historical trial-average meaning, or gate timer-quantized bypass tails.
- Good: port the exact benchmark workload into a detached old-code worktree,
  expose an existing private block-size hook only for measurement, and compare
  that report with the candidate. Never generate a baseline from candidate
  code or silently compare incompatible case matrices.

### 6. Tests Required

- Shared support tests assert odd/even median, nearest-rank p95, raw sample
  retention, invalid samples, CLI modes/paths/thresholds, JSON round trip, and
  environment compatibility including unknown-field rejection. Pinned-probe
  tests additionally cover argument removal/default core/error cases and exact
  p99/max threshold boundaries.
- Callback-tail support tests additionally assert nearest-rank p99/p99.9,
  exact raw-order retention, invalid-sample rejection, tail-threshold CLI
  parsing, canonical scenario/case keys, and actual bypass/active fixture work.
- `tests/benchmark_support.rs` asserts the captured environment features
  contain `resampler-{RESAMPLER_BACKEND_NAME}`; run it under both
  `--all-features` and `--no-default-features --features rubato`.
- Regression tests assert exactly +10% passes, greater than +10% fails, and the
  diagnostic contains case, baseline, candidate, measured regression, and
  threshold.
- Each performance quick run asserts unique case keys, trial-vector lengths,
  finite timing, and complete work. Callback/resampler additionally assert
  consumed/produced work and output bounds; FIR asserts IR length/finite
  samples, finite changed apply output, and overlap-save routing.
- A Windows pinned convolver acceptance run must exercise the real scheduling
  calls, write versioned JSON, contain the 65536/6ch/64 target case, and pass
  both absolute gates. Unit tests cover pure parsing/gate logic; they do not
  substitute for this probe.
- A Windows callback-tail acceptance run must write unpinned and pinned quick
  reports, verify 12 unique cases and exactly 48,000 retained samples, record
  the effective scheduling state, pass a compatible 24-comparison
  baseline/candidate gate, and reject an unpinned baseline by name.
- Callback acceptance checks require two 512-frame active cases and four
  isolated Saturation 4x cases. Output-render checks require every
  scenario/duration/block tuple, active-work evidence, exact finite tails,
  unknown-tail early stop, and duration-independent temporary memory.
- Quality quick `--enforce --out` must deserialize and expose environment,
  skipped count, rendered frames, latency, semantic tail, and truncation.
- Before release, run callback/resampler/FIR quick/full/heavy, quality
  quick/full, both feature test matrices, both strict Clippy matrices, rustfmt,
  docs, and package verification.

### 7. Wrong vs Correct

#### Wrong

```text
Candidate is 8 ns/sample, therefore this is the fastest implementation.
GitHub timing regressed, so fail against last run regardless of runner CPU.
Pinned max missed while the host was saturated, so delete that sample and keep
rerunning until one report passes.
Fold raw callback timings into the aggregate callback report and compare the
sub-microsecond bypass p99 against a portable shared-runner threshold.
```

#### Correct

```text
On the recorded compiler/target/CPU/profile/features, the seven-trial quick
median was 8 ns/input-sample; see the JSON for p95 and raw samples. Enforce a
timing regression only against an explicitly compatible same-environment
baseline; shared-runner absolute timing remains report-only.
For a pinned machine-local max gate, retain every raw sample and failed report;
rerun on a documented quiet host only when concurrent system load polluted the
measurement.
Keep aggregate callback throughput and per-callback tail probes separate. Gate
only active callback-tail median/p99/p99.9 against a verified compatible pinned
baseline; shared CI and bypass tails remain report-only timing evidence.
```

## Scenario: Decoder, Public Component, And Lifecycle Performance Coverage

### 1. Scope / Trigger

- Trigger: changing `StreamingDecoder` startup/streaming/seek/staging behavior;
  `SpectrumAnalyzer`, `Downmixer`, loudness/true-peak analysis, AutoMix,
  `RingBuffer`, or `LoudnessDatabase`; processor setup/reset/finish behavior;
  dynamic Convolver ownership; shared allocation instrumentation; or the three
  coverage probes and their CI wiring.
- These probes close crate-owned performance surfaces. CPAL/WASAPI/device and
  user-visible playback latency stay in consuming-application integration
  evidence because this crate owns no device.

### 2. Signatures

```bash
cargo bench --bench audio_decoder_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]

cargo bench --bench audio_component_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]

cargo bench --bench audio_lifecycle_memory_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]
```

All three use the shared `PerfArgs`, schema version, environment capture,
distribution, JSON, case-set comparison, and compatible-baseline helpers in
`benches/support/`. The decoder and AutoMix cases share
`support::audio_fixture`; allocation evidence shares
`support::allocation::AllocationScope`.

### 3. Contracts

- `audio_decoder_perf` generates a byte-stable 12-second stereo PCM16
  RIFF/WAVE file before timing. Conditions record container, codec/sample
  format, rate, channels, frames, duration, byte count, FNV-1a content hash,
  generation identifier, warm-cache state, and the no-network scope.
- Decoder timing separates local source open, probe, decoder build, first
  borrowed PCM, steady borrowed decode, coarse seek command, and coarse
  seek-to-first-PCM. Steady decode's primary lower-is-better value is ns/frame;
  frames/second and realtime factor remain additional distributions.
- Quick decoder reports retain nine ordinary raw samples and 24 seek samples.
  Each open/probe/build/first-PCM raw sample averages 16 repetitions of that
  same isolated phase so Windows timer granularity does not create false 10%
  regressions. Conditions declare both sample and repetition counts; work
  validation counts all 144 phase operations rather than pretending there were
  only nine.
- Decoder work validation requires exact frames, finite PCM, non-empty first
  packet, stable packet count/hash, and seek error no greater than
  `SEEK_COARSE_TOLERANCE_FRAMES`. Fixed `StreamingDecoder` staging bytes are
  exact; global-allocator rows never claim to include opaque Symphonia/system
  allocations.
- `audio_component_perf` has 11 always-present cases: two Spectrum geometries,
  5.1 and 7.1 Downmixer, two LoudnessMeter block sizes, contiguous and strided
  TruePeakDetector, AutoMix Head/Full, and RingBuffer wrap-capable
  write/read/advance. With `loudness-db`, five in-memory SQLite cases add open,
  single upsert, indexed get, batch upsert, and stats. Without the feature,
  conditions say `excluded`; the cases are not silently treated as passes.
- Component cases declare their primary unit and exact work items. AutoMix uses
  the shared local fixture with a five-second bounded window and no live
  network. Database paths use non-requested benchmark URLs only to avoid local
  file metadata lookup; SQLite itself is in memory.
- `audio_lifecycle_memory_perf` has 13 timing cases: equal-rate and active
  resampler setup, active resampler reset, equal-rate and active resampler
  finish/drain, short/long Convolver setup, Convolver reset and finite drain,
  isolated dynamic-Convolver publication/adoption/reclamation, and bounded
  complete Convolver ownership cycles. Its nine allocation rows separately
  cover equal-rate setup/finish, active reset/finish, Convolver reset/finish,
  and dynamic publication/adoption/reclamation; three persistent-memory rows
  retain the active resampler and both Convolver strategies.
- Persistent setup snapshots are captured while the constructed object remains
  alive. Final/caller buffers are outside the scope. SoXR native `malloc` is
  explicitly invisible; `working_buffer_bytes` describes only exact
  adapter-owned PCM scratch. Pure-Rust reports count allocations routed through
  Rust but do not invent estimates for opaque engine ownership.
- Quick soak performs five trials of 128 complete control/processor lifecycles.
  Every trial must finish disabled, reclaimed, quiescent, dropped, and with zero
  retained Rust bytes. This is bounded lifecycle evidence, not an unbounded RSS
  guarantee.
- Report construction validates the exact mode/feature-specific case set even
  when no baseline is supplied. After writing JSON, each probe deserializes it
  through its concrete report type and compares the result with the in-memory
  report; structural/integer/string values are exact and finite floating-point
  fields may differ by at most four machine epsilons after JSON round-trip.
- Lifecycle same-machine baselines gate seven stable cases: active-resampler
  setup/reset/drain, short/long Convolver setup, finite Convolver drain, and the
  bounded soak. Equal-rate setup/finish, timer-quantized Convolver reset, and
  isolated publication/adoption/reclamation remain report-only with full raw
  evidence.
- All absolute timing stays report-only without a compatible same-machine
  baseline. Default-feature and Rubato reports are incompatible through both
  environment features and backend-bearing conditions/case keys.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Fixture bytes/header/hash differ from the versioned contract | support test or decoder report integrity fails before timing claims are accepted |
| Decoder first PCM is empty, output is non-finite, frames/hash vary, or seek exceeds tolerance | named decoder case is invalid; `--enforce` fails |
| Steady decoder has no post-first-packet work | reject the fixture/report rather than emit a fabricated throughput |
| Component output is non-finite/trivial or work counts differ | named component case is invalid |
| `loudness-db` feature is absent | 11 cases plus explicit exclusion text; no database pass claim |
| Resampler process + drain length differs from rounded rate conversion | lifecycle report fails |
| Convolver finish differs from `ir_length - 1` or repeated finish is not `Finished(0)` | lifecycle report fails |
| Dynamic generation/counter/reclamation/quiescence invariant fails | publication/adoption/reclamation and soak cases fail |
| Any complete soak trial retains Rust bytes | bounded soak gate fails and reports maximum retained bytes |
| A mode/feature-specific case is missing or duplicated without a baseline | report integrity fails before JSON is accepted |
| Written JSON cannot deserialize to the concrete report or exceeds the round-trip tolerance | `--enforce --out` fails after writing the named report |
| SoXR allocator row is presented as total native/process memory | invalid claim; preserve native-allocation limitation |
| Baseline schema/mode/conditions/environment/case set differs | reject before calculating percentages |
| Shared CI has no explicit baseline | validate work/schema/JSON only; timing remains report-only |

### 5. Good / Base / Bad Cases

- Good: compare two same-machine decoder reports with identical fixture hash
  and warm-cache conditions, while retaining open/probe/build/first/steady/seek
  as separate cases.
- Good: a Rubato-only component report contains 11 valid cases and a visible
  database feature exclusion; a default report contains all 16.
- Base: a lifecycle report shows zero public Rubato adapter working bytes while
  retaining measured Rust setup allocations for the engine; the two fields
  describe different ownership boundaries.
- Bad: time fixture generation as decoder startup, combine open and first PCM,
  call PCM/WAV throughput a compressed-codec result, or perform live HTTP in
  quick mode.
- Bad: report Convolver publication speed without driving audio adoption and
  control-side reclamation, or call a process-alive allocation snapshot a leak.
- Bad: claim end-to-end/device latency from an in-crate `Instant` interval that
  never opens an audio device.

### 6. Tests Required

- `tests/benchmark_support.rs` asserts the deterministic WAV header, length,
  rate/channels/frames, stable content hash, idempotent file generation, and
  observable Rust allocation activity/peak bytes.
- Each quick probe must run with `--enforce --out`, deserialize its own report,
  contain unique complete case keys and exact raw trial counts, and validate
  every case's work.
- Decoder quick validates exact decoded frames, stable full/steady hashes,
  staging bytes, non-empty first/post-seek packets, finite samples, and coarse
  seek tolerance.
- Component quick runs under default features (16 cases) and Rubato-only (11
  cases plus database exclusion). AutoMix Head and Full must both execute the
  local fixture.
- Lifecycle quick runs under both backends and proves exact resampler duration,
  exact Convolver tail, stable terminal finish, dynamic generation/reclamation,
  authoritative quiescence, 13 unique timing cases, nine allocation rows, three
  persistent-memory rows, and zero retained Rust bytes after every complete
  soak trial.
- Shared support tests round-trip all report types and reject missing or
  duplicate standalone case keys. Every real `--out` run then performs the same
  concrete-type read-back check against the just-written report.
- Baseline tests retain the shared exactly-10%-passes and incompatible
  schema/mode/conditions/environment/case-set rejection behavior.
- CI uploads the three reports from both default-feature and pure-Rust jobs;
  neither supplies an implicit cross-run baseline.

### 7. Wrong vs Correct

#### Wrong

```text
Time StreamingDecoder::open plus fixture creation and call the result first PCM.
Report one component count even when loudness-db was not compiled.
Treat Rust retained bytes as libsoxr/process RSS and run an endless soak in CI.
```

#### Correct

```text
Generate and validate the byte-stable fixture before timing, then report
source-open, probe, build, first borrowed PCM, steady decode, and seek separately.
Record 16 default or 11 Rubato-only component cases with a visible DB exclusion.
Report Rust allocator and exact adapter scratch boundaries separately, and use a
declared 13-case lifecycle matrix and bounded soak whose complete trials return
retained bytes to zero. Validate the exact case set and read the JSON back before
accepting a report, even when no baseline was supplied.
```

## Scenario: Canonical Output Stages And Post-Render Analysis

### 1. Scope / Trigger

- Trigger: adding, removing, renaming, or reordering an output-chain stage;
  changing callback/offline traversal; or changing quality-report analysis
  metadata.
- The private output-stage manifest is the execution source. Do not restore
  independent handwritten stage-order lists.

### 2. Signatures

```rust
canonical_output_stage_descriptors() -> &'static [OutputStageDescriptor]
callback_stage_names() -> Vec<&'static str>
offline_render_stage_names() -> Vec<&'static str>
canonical_post_render_analysis_descriptors()
    -> &'static [PostRenderAnalysisDescriptor]
post_render_analysis_names() -> Vec<&'static str>
```

`OutputStageDescriptor` exposes `callback_stage` and
`offline_render_stage`. Post-render analysis has separate
`PostRenderAnalysisId` / `PostRenderAnalysisDescriptor` types. The removed
`offline_stage_names`, `offline_stage_order_csv`, and `offline_stage` names do
not have compatibility wrappers.

### 3. Contracts

- One private declarative manifest orders source-rate transforms, the optional
  resampler rate boundary, output-rate transforms, and terminal quantization.
  It expands callback construction plus offline process/render/reset/rate
  traversals while preserving concrete `OutputRenderChain` fields.
- Callback retains its preallocated `DspChain` trait-object storage; offline
  traversal remains statically dispatched. Manifest use must not add callback
  allocation, per-block container growth, downcasts, or another virtual layer.
- `LoudnessMeter` is analysis, not a signal transform. It never appears in
  output-stage descriptors or `OutputRenderChain`; it is reported as the
  opt-in `LoudnessMeterTruePeak` post-render analysis and consumes final
  rendered samples without modifying them.
- Quality JSON/report text carries actual render stages and post-render
  analysis in separate fields. It must not append a second Meter label to a
  stage CSV that already claims Meter execution.
- Stage metadata APIs allocate only for setup/reporting. They are not called
  from the callback processing loop.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Callback stage order differs from manifest descriptors | parity test fails with both observed and canonical orders |
| Offline shared stage output differs from callback at equal rates | bit-exact pre-quantize parity test fails |
| Meter appears in render-stage metadata | reject; move it to post-render analysis descriptors |
| Resampler appears in callback traversal | reject; it is an offline rate-boundary role |
| Quantize runs before output-rate noise shaping | reject; terminal transform must remain last |
| New callback traversal allocates after setup | `assert_no_alloc` regression fails |

### 5. Good / Base / Bad Cases

- Good: move one manifest entry and let callback construction, offline
  traversal, descriptors, and parity snapshots change together.
- Base: keep `OutputRenderChain` typed fields for limiter telemetry and
  Convolver reclamation while manifest macros emit direct method calls.
- Bad: add Meter to the offline stage list without adding a Meter field and
  actual signal traversal, or maintain a separate benchmark-only order string.

### 6. Tests Required

- Assert callback processor names equal the descriptor callback subset and a
  deliberately reordered observation fails the parity assertion.
- Assert offline names include Resampler and Quantize but exclude Meter; assert
  post-render descriptors contain `LoudnessMeterTruePeak` exactly once.
- Compare active callback and offline pre-quantize output bit-for-bit at equal
  sample rates.
- Keep callback no-allocation, irregular-chunk equivalence, and reset-isolation
  tests after any manifest change.
- Run `audio_quality_measurements --quick --enforce` and
  `audio_callback_chain_perf --quick --enforce` after traversal/report changes.

### 7. Wrong vs Correct

#### Wrong

```rust
const OFFLINE_NAMES: &[&str] = &["Volume", "Meter"];
// OutputRenderChain never constructs or executes Meter.
```

#### Correct

```rust
let render_stages = offline_render_stage_names();
let analyses = post_render_analysis_names();
// Report the two plans separately; only render_stages transform samples.
```

## Scenario: Rubato Linear Routing and Hybrid Nonlinear Phase Support

### 1. Scope / Trigger

- Trigger: changing the `rubato` version, fixed chunk size, FIFO representation,
  FFT routing bound, sub-chunk count, phase routing, polyphase geometry,
  delay/tail accounting, quality mapping, or resampler benchmark algorithm labels in
  `src/processor/resampler/`.

### 2. Signatures

```rust
should_use_fft(from_rate: u32, to_rate: u32, quality: ResampleQuality) -> bool
nonlinear_uses_spectral(from_rate: u32, to_rate: u32) -> bool
RubatoEngine::new(
    from_rate: u32,
    to_rate: u32,
    phase: PhaseResponse,
    quality: ResampleQuality,
    channels: usize,
)
    -> Result<RubatoEngine, String>
MonoBackend::process(&mut self, input: &[f64], output: &mut [f64])
    -> Result<BackendProgress, &'static str>
MonoBackend::drain(&mut self, output: &mut [f64]) -> Result<usize, &'static str>
MonoBackend::clear(&mut self) -> Result<(), &'static str>
```

Evidence commands:

```text
cargo bench --bench audio_resampler_streaming_perf --no-default-features --features rubato -- --quick --enforce --out <resampler.json>
cargo bench --bench audio_resampler_matrix_perf --no-default-features --features rubato -- --heavy --out <matrix.json>
cargo bench --bench audio_quality_measurements --no-default-features --features rubato -- --quick --enforce --out <quality.json>
```

### 3. Contracts

- `PhaseResponse::Linear` uses a dedicated 127-tap symmetric half-band FIR when
  quality is High and `to_rate == 2 * from_rate`. The engine uses the existing
  1024-frame fixed input chunk, evaluates 32 symmetric coefficient pairs, and
  emits the companion phase as a delayed direct source sample. Other Low,
  Standard, and High common ratios use `Fft<f64>` with two FFT sub-chunks and
  `BlackmanHarris2`; UltraHigh common ratios use `Fft<f64>` with one sub-chunk
  (a 2x longer internal FIR, the tier's quality knob). A rate pair is common
  only when both components after GCD
  reduction are at most 1024. Larger reduced ratios use
  `Async<f64>::new_sinc`; quality selects the sinc parameters for those routes.
- `PhaseResponse::Minimum` and `PhaseResponse::Maximum` use a separate,
  setup-designed causal rational FIR. A real-cepstrum spectral factor of the
  low-pass prototype produces the minimum-phase kernel; reversing that kernel
  produces maximum phase with the same magnitude response. Reduced `up <= 16`
  uses overlap-save spectral execution. Larger valid `up` uses contiguous
  time-domain polyphase execution with planar channel history. Both engines
  share the immutable kernel, latency, finish-extension, and exact cumulative
  rational pacing formulas.
- Nonlinear phase accepts only reduced rate components at most 1024 and a
  bounded coefficient bank. Unsupported geometry returns the named
  initialization error; it must never fall back to the linear Rubato engine or
  merely report a shifted latency as a phase response.
- Contiguous history retains
  `taps_per_phase - 1 + ceil((down - 1) / up)` frames before each chunk so a
  rationally authorized output may refer just before the chunk boundary.
  Window offsets use checked arithmetic and static errors. Construction selects
  a stereo AVX2 function pointer when available; it uses multiply plus add, not
  FMA, so the four-lane reduction stays bit-equal to scalar and feature
  detection never enters the callback.
- Keep two sub-chunks for Low through High unless a same-machine sweep improves
  both core conversions without weakening quality. The 2026-07-22 512-frame
  evidence rejected one sub-chunk for High (35.10 ns/input sample at 44.1 to
  48 kHz) and four sub-chunks (14.59 ns/input sample at 48 to 96 kHz) against
  two sub-chunks (9.86 and 12.57 respectively). UltraHigh deliberately pays the
  one-sub-chunk cost for the 2x longer filter: the 2026-07-25 quality harness
  measured THD+N -204.9 dB, passband deviation 2.0e-11 dB, and alias
  attenuation -290.5 dB, beating both the High two-sub-chunk route and the
  previous UltraHigh sinc engine on passband and alias. Changing the
  precomputed window does not reduce runtime FFT work and still requires fresh
  quality evidence.
- `OutputRenderChain` requests UltraHigh, which under the rubato feature now
  uses the one-sub-chunk FFT route for common ratios. A quality-routing change
  must run `audio_output_render_perf --quick` as well as the focused resampler
  probe.
- Linear Rubato engines carry a real leading delay. The adapter discards
  exactly `output_delay()` produced frames once per stream, then drains or
  truncates to `round(total_input * to_rate / from_rate)`. Nonlinear phase does
  not crop its causal kernel: it reports the actual output-rate latency plus a
  finite tail whose sum covers the full response. Reset restores the engine,
  FIFOs, duration counters, terminal state, delay skip, and polyphase history.
- Input/output staging uses setup-allocated fixed-capacity `SampleRing` queues.
  The input capacity is two fixed chunks so exact one-chunk consumption keeps
  each next front chunk contiguous across wrap. The rings never grow, overwrite
  unread samples, or log; push/pop use at most two bounded copies and preserve
  strict output backpressure. Do not replace them with the public pipeline ring,
  whose overflow and read semantics differ from the resampler contract.
- Low-through-High FFT keeps the 1024-frame/two-sub-chunk production geometry.
  A complete caller chunk bypasses an empty input FIFO. When a staged FIFO
  prefix plus the caller suffix completes the chunk, `SplitInterleavedInput`
  presents both segments as one stack view and Rubato bulk-copies each channel
  directly into its planar scratch; the suffix is not first copied into the
  ring. Output adapters bulk-copy per channel. A short caller output uses the
  bounded split/spill route and preserves unread output in the fixed ring.
- FFT drain with an empty input FIFO uses `Indexing::new().partial_len(0)` so
  Rubato supplies its internal zero padding without an explicit interleaved
  zero block. If one drain step covers every caller-visible frame still
  authorized by exact duration and the caller has enough capacity,
  `TerminalInterleavedOutput` writes only that interval and discards leading
  delay plus the native suffix after the terminal frame. Constrained output,
  non-FFT engines, or a real partial tail retain the split/spill fallback.
- `split_input_enabled`, `partial_zero_drain_enabled`, and
  `terminal_truncate_drain_enabled` exist only under `cfg(test)` as permanent
  bit-exact oracle controls. They are not runtime architecture switches or
  benchmark tuning knobs.
- Audioadapter slice wrappers are stack views. Rubato construction, FIR design,
  and engine boxing happen only during setup; process, drain, and reset remain
  allocation-free.
- Half-band coefficients, per-channel history, and block staging are allocated
  during setup. Construction may select a fixed AVX2+FMA accumulator function
  pointer after feature detection; callback processing must not repeat feature
  detection. The scalar fallback and selected vector kernel must be bit-equal
  for vector and remainder lengths.
- The performance report algorithm text and `case_key` identify exact-2x
  half-band routing, FFT/sinc fallback, and the nonlinear `up = 16` split. Any
  algorithm/routing change must change that identifier so an older spectral,
  sinc, FFT, or differently routed report is baseline-incompatible.
- The retained v17 FFT adapter IDs are
  `streaming_native_interleaved_halfband2x_fft1024_sub2_bulk_io_split_input_terminal_drain_v17`
  and
  `audio_engine_core_rubato_fft1024_subchunk2_bulk_io_split_input_terminal_drain_v17`.
  Reports with earlier adapter IDs may be matched manually only after aligning
  work, quality, caller schedule, and host conditions; they are not compatible
  automatic timing baselines.
- On the recorded 2026-07-26 Windows/rustc 1.93.1 host, four-run pinned heavy
  medians moved 44.1-to-48 High/Minimum from 137.72 to 27.53 ns/input sample
  (5.00x). Retained 48-to-96 nonlinear cases were within +1.01%; the only
  short-matrix Linear result above +5% passed a 6000-iteration, 21-trial
  focused ABBA at -3.35%. These are machine-local evidence, not portable
  absolute claims.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Either sample rate is zero | Public constructor rejects it before engine construction |
| Linear + High + exact 2x upsampling | Select the dedicated half-band engine |
| Linear + Low/Standard, or High non-2x, and both reduced rate components are <= 1024 | Select FFT with two sub-chunks |
| Linear + UltraHigh common ratio | Select FFT with one sub-chunk (2x longer FIR than High) |
| Linear + either reduced rate component > 1024 | Select sinc; never construct a pathological FFT block |
| Minimum/Maximum + reduced `up <= 16` | Select spectral nonlinear execution |
| Minimum/Maximum + reduced `up > 16` within all bounds | Select contiguous polyphase execution |
| Minimum/Maximum + reduced component > 1024 or oversized coefficient bank | Named initialization error; never silently select FFT/sinc |
| Backend consumes other than the fixed input chunk or over-reports output | Static `ProcessError::Backend` path; never slice or panic |
| Input/output ring lacks capacity for a requested push | Static backend error; never overwrite, resize, allocate, or log |
| FIFO prefix plus caller suffix does not form exactly one FFT input chunk | static backend error before native processing |
| Initial delay spans multiple output calls | Continue discarding until the counter reaches zero |
| Terminal direct drain cannot retain every still-authorized frame in caller output | use split/spill fallback; never truncate required output |
| Repeated drain after exact duration is emitted | return zero without invoking more native work |
| Reset after process or finish | Re-arm the original delay and produce the same output as a fresh instance |
| Algorithm label differs from a baseline | Reject comparison before computing timing percentages |

### 5. Good / Base / Bad Cases

- Good: 48 to 96 kHz and 44.1 to 88.2 kHz use half-band at Linear/High;
  Linear/Standard keeps FFT and Linear/UltraHigh uses the one-sub-chunk FFT.
  44.1 to 48 kHz
  reduces to 147:160 and uses FFT at Linear/High, while Minimum/Maximum use the
  contiguous polyphase engine. 48 to 96 Minimum/Maximum stays on the spectral
  engine because its reduced `up` is 2.
  44.1 to 44.101 kHz reduces to 44100:44101 and uses sinc for Linear at every
  quality but rejects nonlinear phase before processing.
- Base: equal-rate streams bypass the backend in `StreamingResampler`.
- Good: wrapped input/output rings preserve exact order and expose every next
  fixed input chunk contiguously without shifting queued prefixes.
- Good: a 512-frame FIFO prefix plus a 512-frame caller suffix completes one
  FFT input chunk through the split view, while a terminal zero-input FFT step
  writes only the exact remaining duration directly to a sufficiently large
  caller buffer.
- Bad: construct FFT for every non-equal rate, creating a 44,101-frame output
  block and 22,050-frame delay for 44.1 to 44.101 kHz.
- Bad: accept `Minimum` or `Maximum`, call the linear engine, and alter only a
  reported latency or output start frame.
- Bad: keep a generic `rubato_streaming_default` case key after changing from
  sinc to FFT, which makes incompatible performance evidence look comparable.
- Bad: broaden the half-band predicate to Standard, UltraHigh, downsampling,
  non-2x ratios, or nonlinear phase because one 48-to-96 benchmark improved.
- Bad: cite a noisy quick-run minimum as representative evidence, or retain a
  FIFO rewrite that does not improve the adjacent heavy median matrix.
- Bad: stage a full interleaved zero chunk merely to drain an empty FFT FIFO,
  or copy a terminal native suffix into a ring only to clear it at the exact
  duration boundary.
- Bad: expose the three test-only oracle switches through an environment
  variable or production API.

### 6. Tests Required

- Routing tests assert exact-2x Linear/High upsampling selects half-band without
  broadening; other common ratios select FFT (UltraHigh with one sub-chunk,
  Low through High with two), and a coprime adjacent rate selects sinc.
- Half-band tests compare block output with full zero-stuffed convolution,
  enforce DC gain and representative passband/image bounds, compare native
  interleaving with independent mono engines, and run process/reset under
  `assert_no_alloc`.
- Nonlinear tests assert a minimum-phase energy centroid before the linear
  prototype, maximum after it, distinct non-shift phase envelopes, and the
  same magnitude response within the documented tolerance. They also cover
  the spectral/contiguous routing boundary, oracle error below `1e-9`, timing
  equality, scalar/AVX2 bit equality, unsupported reduced geometry, and
  actual-ratio passband, THD+N, and alias bounds.
- Shared resampler tests cover duration, impulse alignment within one frame,
  random chunking equivalence, reset isolation, and process/finish no-allocation
  under the pure-Rust feature matrix, including nonlinear finite-tail drain.
- Ring-specific tests cross both input/output wrap, assert exact order and front
  contiguity, and run push/pop under `assert_no_alloc`.
- FFT adapter tests cover both canonical directions with 128/256/512-frame
  callers and a constrained 257-frame output. They prove split-input versus
  forced FIFO, partial-zero versus explicit-zero, and terminal-truncate versus
  split/spill complete streams are bit-exact; they also cover reset/fresh,
  stable terminal drain, and process/drain `assert_no_alloc`.
- An end-to-end pathological-ratio test must exercise the real sinc fallback,
  not only the routing predicate.
- Run all 27 quick quality gates and record THD+N, 20 kHz gain, passband, and
  stopband values. Run the quick resampler report and record both 512-frame
  `process_checked` conversions before changing documented claims.
- Run pinned streaming quick plus at least two heavy confirmations over
  128/256/512/1024 callers and both public API paths after changing the FFT
  adapter. Retain raw median/p95 vectors and do not classify a cross-run p95
  spike as a regression unless it repeats under matched host conditions.
- When UltraHigh routing changes, run the quick output-render report and record
  the active resampled scenario's CPU, realtime factor, and setup memory.

### 7. Wrong vs Correct

#### Wrong

```rust
let engine = Fft::new_custom(from, to, 1024, 2, 1, window, FixedSync::Input)?;
let nonlinear = SpectralNonlinearResampler::new(from, to, phase, quality, channels, 1024)?;
out_fifo.extend_from_slice(&out_stage[..written]);
out_fifo.copy_within(consumed.., 0); // shifts queued samples every chunk
in_fifo.push(&zero_chunk)?; // copies interleaved silence solely to drive FFT drain
out_fifo.push(&terminal_native_output)?; // stages suffix that exact duration discards
```

#### Correct

```rust
let engine = if should_use_fft(from, to, quality) { fft()? } else { sinc()? };
let nonlinear = if nonlinear_uses_spectral(from, to) {
    spectral()?
} else {
    contiguous_polyphase()?
};
let skip = delay_remaining.min(written);
delay_remaining -= skip;
out_fifo.push(&out_stage[skip..written])?; // fixed capacity, strict backpressure

let input = SplitInterleavedInput::new(&[], &[], channels)?;
let indexing = Indexing::new().partial_len(0);
let mut output = TerminalInterleavedOutput::new(
    caller_output,
    channels,
    native_frames,
    delay_to_drop,
    exact_remaining,
)?;
engine.process_fft_adapters(&input, &mut output, Some(&indexing))?;
```

## Code Review Checklist

- [ ] Hot path: no alloc/lock/log/IO/panic/unbounded work.
- [ ] Claims in docs/README backed by a test, current bench output, or a
      limitation note.
- [ ] Feature-gated code builds with each optional feature toggled individually
      on top of a minimal resampler backend (`--no-default-features --features
      rubato[,...]`). Bare `--no-default-features` must still fail the backend
      guard; see the Testing Requirements section.
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

- Feature checks. Every combination carries a resampler backend, because
  enabling neither is a deliberate compile error rather than a supported build:
  - `cargo build`
  - `cargo build --no-default-features --features rubato`
  - `cargo build --no-default-features --features soxr`
  - `cargo build --no-default-features --features rubato,http`
  - `cargo build --no-default-features --features rubato,loudness-db`
  - `cargo build --no-default-features` — must fail on the missing-backend
    guard. A successful build here means the guard regressed.
- Docs/package checks:
  - `cargo doc --no-deps`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`
  - `cargo package --allow-dirty`

### 3. Contracts

- `http` controls the optional `reqwest` dependency and network error surface.
- `loudness-db` controls the optional `rusqlite` dependency and SQLite cache
  types.
- The resampler backend is feature-selected: `soxr` (default) links native
  libsoxr (LGPL-2.1), while `rubato` compiles quality-aware pure-Rust FFT/sinc
  routing under `src/processor/resampler/rubato_backend.rs`. Enabling neither
  backend is a compile error; when both are enabled, `soxr` wins. A
  `default-features = false, features = ["rubato"]` build has no native
  dependency. Both backends must satisfy the same mono streaming contract
  (arbitrary input granularity, duration-aligned drain, `clear` restoring
  initial state) enforced by the shared resampler test suite.
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

- Good: "`default-features = false, features = [\"rubato\"]` removes the HTTP,
  SQLite, and native libsoxr dependencies; a resampler backend is still required
  because resampling is core."
- Base: "`http` and `loudness-db` are default-on and can be disabled
  independently; the resampler backend is chosen, not omitted."
- Bad: "`default-features = false` creates a dependency-free DSP-only build",
  "building without resampling avoids libsoxr", or "SoXR remains required" now
  that `rubato` is a supported pure-Rust backend.

### 6. Tests Required

- Build the default profile and each optional feature independently on top of a
  minimal backend. Assert that bare `--no-default-features` still fails.
- Run `cargo test --all-features` and
  `cargo test --no-default-features --features rubato` before publishing; these
  are the two supported backend paths.
- When changing dependencies, run `cargo rustc --lib -- -D unused-crate-dependencies`
  so dead direct dependencies fail the check instead of remaining in the manifest.
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

## Scenario: Windows MSVC Runtime Deployment for MSYS2 SoXR

### 1. Scope / Trigger

- Trigger: changing `build.rs`, `build/windows_runtime.rs`, Windows native
  dependency setup, `PKG_CONFIG_PATH` handling, or any test/example/benchmark
  that must start with the MSYS2 SoXR DLL under the MSVC Rust target.
- This scenario does not apply when `vcpkg::find_package("soxr")` selects a
  static vcpkg build or on non-Windows targets.

### 2. Signatures

```text
PKG_CONFIG_PATH=<msys2-prefix>/mingw64/lib/pkgconfig
VCPKG_ROOT=<vcpkg-root>  # alternative provider

soxr_dll_candidates_from_pkg_config_dir(pkg_config_dir: &Path) -> Vec<PathBuf>
deploy_runtime_dlls(soxr_dll: &Path, out_dir: &Path) -> Result<(), String>

cargo test
cargo run --example <name>
cargo bench --bench <name> -- <bench arguments>
```

### 3. Contracts

- A pkg-config directory at `<prefix>/lib/pkgconfig` maps to
  `<prefix>/bin/libsoxr.dll` (or `soxr.dll`). `<prefix>/lib/bin` is wrong.
- Explicit pkg-config and the known Scoop/MSYS2 installation are searched
  before generic `PATH`, so a stale DLL already copied under `target/` cannot
  become the source for the next deployment.
- The MSYS2 source directory supplies one ABI-matched set: `libsoxr.dll`,
  `libgomp-1.dll`, `libgcc_s_seh-1.dll`, and `libwinpthread-1.dll`. Never mix a
  `libgomp` or GCC runtime from another MinGW installation merely because it is
  earlier on `PATH`.
- Runtime DLLs are content-checked and deployed beside every ordinary Cargo
  executable location for the active profile: profile root, `deps`, and
  `examples`. This covers binaries, tests/doctests, custom-harness benches, and
  examples without command-specific PATH wrappers.
- Unrelated DLLs from the MSYS2 `bin` directory are not copied. Runtime sources
  and `build/windows_runtime.rs` are Cargo rerun inputs, so an updated installed
  runtime or deployment rule refreshes stale destinations.
- Deployment is build-time filesystem work only; it never enters an audio or
  DSP path.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| `PKG_CONFIG_PATH=<prefix>/lib/pkgconfig` | Probe `<prefix>/bin`, not `<prefix>/lib/bin` |
| Cargo target root already has an isolated `libsoxr.dll` | Prefer the configured MSYS2 source; do not self-copy the isolated target DLL |
| Destination DLL bytes equal source | Leave the file unchanged |
| Destination DLL is absent or stale | Copy/refresh it in root, `deps`, and `examples` |
| Unrelated sibling DLL exists | Do not deploy it |
| Destination directory is absent | Create it during the build script |
| Source/destination read, directory creation, or copy fails | Return a diagnostic containing both relevant paths |
| Supported MSYS2 deployment is complete | Direct Cargo test/example/bench commands start without `STATUS_DLL_NOT_FOUND` or runtime PATH injection |

### 5. Good / Base / Bad Cases

- Good: derive one source `bin` directory from pkg-config, copy its SoXR and
  matching MinGW runtime set into all three Cargo executable directories, then
  run the quality bench directly.
- Base: a vcpkg static triplet links without runtime DLL deployment.
- Bad: copy only `libsoxr.dll` to `target/release`; benchmark executables live
  in `target/release/deps` and the DLL itself imports `libgomp-1.dll`.
- Bad: make every developer prepend an arbitrary MinGW directory to runtime
  `PATH`; another ABI-compatible-looking `libgomp-1.dll` can fail or hang during
  DLL initialization.

### 6. Tests Required

- Path resolution asserts `<prefix>/lib/pkgconfig -> <prefix>/bin` for both
  accepted SoXR DLL names.
- A filesystem fixture asserts exact deployment of the four supported runtime
  DLLs to profile root, `deps`, and `examples`, while an unrelated DLL remains
  absent.
- A stale-destination fixture asserts every executable directory is refreshed
  from the configured source.
- On Windows MSVC with MSYS2 SoXR, run at least one SoXR-using custom bench
  directly with no temporary PATH injection; `--quick --enforce` must reach the
  benchmark report and exit zero.

### 7. Wrong vs Correct

#### Wrong

```rust
let dll = pkg_config_dir.join("..").join("bin").join("libsoxr.dll");
fs::copy(dll, profile_dir.join("libsoxr.dll"))?;
```

#### Correct

```rust
let prefix = pkg_config_dir.ancestors().nth(2).ok_or("missing prefix")?;
let dll = prefix.join("bin").join("libsoxr.dll");
deploy_runtime_dlls(&dll, out_dir)?; // root + deps + examples, same-source closure
```

## Scenario: Cross-Project Resampler Comparison

### 1. Scope / Trigger

- Trigger: adding or changing `audio_resampler_comparison_perf`, its
  benchmark-owned adapters/report schema, a raw-upstream control, or a
  runtime-loaded external resampler.
- This scenario is evidence tooling only. It must not change production
  backend selection, public resampler behavior, or ordinary runtime linkage.

### 2. Signatures

```text
cargo bench --bench audio_resampler_comparison_perf --all-features -- \
  [--quick|--heavy] [--enforce] [--out <report.json>] \
  [--baseline <report.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>] \
  [--libsamplerate <explicit-library-path>] \
  [--engine-library <engine-id>=<explicit-shim-path>] \
  [--require-engine <engine-id>] [--require-complete-matrix] \
  [--raw-rubato-geometry <512/1|1024/2>] \
  [--pinned] [--pin-core <logical-core>]

AUDIO_BENCH_LIBSAMPLERATE_PATH=<explicit-library-path>
AUDIO_BENCH_NATIVE_SHIM_TEST_DIR=<directory-containing-all-seven-shims>

LibSamplerateAdapter::process_final(input: &[f32], output: &mut [f32])
    -> Result<AdapterProgress, String>
LibSamplerateAdapter::drain(output: &mut [f32])
    -> Result<AdapterProgress, String>

build_resampler_shims.ps1
    [-DependencyRoot <ignored-cache>] [-CompilerPrefix <mingw-prefix>]
    [-BuildDirectory <ignored-output>]

ReportConditions.adapter_schema =
    "cross_project_resampler_v5_common_capacity_compensated_exact_work"
ReportConditions.workload_id =
    "quality_latency_throughput_pareto_v3_strict_capacity_delay"
ReportConditions.output_capacity_policy =
    "common_max_per_rate_across_all_measured_factories;identical_for_process_reset_quality_and_drain"
AEB_RESAMPLER_ABI_VERSION = 2

aeb_resampler_process(
    state, input, input_frames, output, output_capacity_frames, end_of_input,
    consumed_frames, produced_frames, finished
) noexcept -> int
aeb_resampler_reset(state) noexcept -> int
```

Known engine IDs are `audio_engine_core`, `raw_libsoxr`, `raw_rubato`,
`libsamplerate`, `ffmpeg_libswresample`, `speexdsp`, `r8brain`,
`zita_resampler`, `webrtc`, `wdl`, and `libresample`. The last seven are
runtime-loaded benchmark shim IDs.

### 3. Contracts

- External libraries load only from `--libsamplerate`,
  `AUDIO_BENCH_LIBSAMPLERATE_PATH`, or one explicit `--engine-library
  <engine-id>=<path>` per native shim. Never search `PATH` or system
  directories. Record the canonical path, upstream version, SHA-256, file
  size, sample format, source revision, build provenance, and linked runtime
  artifacts in the report; keep acquired binaries under ignored benchmark
  cache storage and never commit them.
- `benches/native/build_resampler_shims.ps1` is the reproducible provisioning
  entry point. It validates pinned package/file hashes and exact git revisions,
  rejects dirty pinned git worktrees, and validates fixed source/install inputs
  before compiling. Directory identity uses sorted
  `relative-path SP file-SHA256` records encoded as UTF-8 without BOM and joined
  with LF before hashing. The script requires MinGW-w64 GCC/G++ 15.2.0,
  separates C/C++ compilation where required, copies only runtime dependencies
  into the ignored output directory, and prints final shim hashes.
- Every measured engine records adapter schema
  `cross_project_resampler_v5_common_capacity_compensated_exact_work`, a stable
  algorithm ID, implementation/version, format/layout, quality recipe, phase
  behavior, rate pair, channels, and chunk size. An algorithm or lifecycle
  change needs a new algorithm ID so incompatible reports cannot compare as
  one engine.
- Before measurement, every discovered factory must create successfully for
  both canonical rate pairs. A failed create probe becomes an explicit
  unavailable row before any partial case can be presented as measured.
- Before timing each factory/rate pair, process a deterministic stream, reset
  the same instance, process it again, and compare both complete outputs with a
  separately created fresh instance bit-for-bit. Any length/sample difference
  invalidates that engine; a native reset API is never trusted by itself.
- Time setup, steady process, reset, and drain separately. Validate native
  consumed/produced counts before slicing and retain raw trial samples. For
  every trial, gate exact warm-up and timed input consumption plus
  `warmup_output_frames + steady_output_frames + drain_frames ==
  expected_complete_output_frames`; terminal drain is also mandatory. One
  native output buffer of slack is not acceptable. Cross-engine quality,
  latency, and throughput remain report-only; compatible same-engine baselines
  may gate timing.
- Before setup timing, create every runnable factory once per rate and select
  the maximum advertised output capacity. Record that value in
  `output_capacity_frames_by_rate` and use the identical common capacity for
  process, reset/fresh, quality, and drain across all engines. A factory that
  cannot create for the capacity probe is unavailable; a per-engine timed
  buffer is not comparable v5 evidence.
- Optional `--pinned --pin-core <logical-core>` must record the requested core
  plus the verified effective affinity mask, process priority, and thread
  priority. Pinned and unpinned reports, or reports pinned to different cores
  or scheduling states, are baseline-incompatible. Pinning reduces migration
  noise but does not remove interrupts, DPCs, frequency changes, or scheduler
  outliers from raw trials.
- The f64 project/libsoxr/Rubato/FFmpeg/r8brain/WDL lanes and the f32
  libsamplerate/SpeexDSP/zita/WebRTC/libresample lanes are distinct. Never
  create a strict cross-format speed gate or imply that differently named
  quality recipes are equivalent.
- Preserve native lifecycle semantics. Duration-aligned engines produce the
  rounded rate-converted length unless the upstream API has a stricter
  documented endpoint rule. Raw Rubato records its native `output_delay` but
  compensates it before caller-visible output, reports zero compensated API
  latency, and paces emission to the exact rounded duration. Its 512/1 and
  1024/2 algorithms are respectively
  `raw_rubato_fft512_bh2_subchunk1_compensated_exact_v3` and
  `raw_rubato_fft1024_bh2_subchunk2_compensated_exact_v3`. Drain uses bounded
  partial-zero state advancement and must terminate at the exact target.
- Strict adapter-overhead claims require one compiled build and one report with
  the same lane, recipe, geometry, common capacity, exact work, and trial
  schedule. The all-feature build pairs the selected project SoXR backend with
  raw libsoxr; the Rubato-only build pairs the project Rubato backend with raw
  Rubato 1024/2. Absolute timings across those two separately compiled reports
  are informative but are not a controlled AB comparison. A `faster` claim
  requires at least a 2% median advantage or separated trial distributions;
  smaller median deltas are classified as tied.
- libsamplerate must receive `end_of_input = 1` on the final `src_process`
  call that still contains real input. A later empty call is only for draining
  already-signalled state; first exhausting all input with
  `end_of_input = 0` truncates the filter tail. Its exact completed length is
  `ceil(input_frames * output_rate / input_rate)`, including 2,787 frames for
  2,560 input frames at 44.1-to-48 kHz. This lifecycle is identified as
  `libsamplerate_sinc_best_quality_streaming_f32_v3`.
- Every native shim uses ABI version 2, validates geometry and progress at the
  C boundary, preallocates process/drain staging, and makes terminal drain
  idempotent. All seven `process` and `reset` exports are `noexcept`; C++
  exceptions are translated to a shim error code/string and never cross the C
  ABI. Algorithm IDs use `<engine-id>_benchmark_shim_v2`.
- Its exact complete output follows the adapter's documented
  duration-plus-latency policy. Keep three concepts separate in every quality
  case: `reported_api_buffering_latency_frames`,
  `observed_input_frames_before_first_output`, and
  `measured_impulse_peak_frame`. File-aligned engines may consume lookahead
  before producing output while retaining an impulse at output frame zero.
- FFmpeg process and drain cap output against cumulative exact rational
  duration. A short native flush is not terminal while target frames remain.
  WebRTC accumulates arbitrary 512-frame caller chunks into native 10 ms
  blocks, pads only the final block, and trims output attributable to padding.
- Reset must reproduce a fresh stream sample-for-sample. The tested SpeexDSP
  1.2.1 interleaved build leaves right-channel history after
  `speex_resampler_reset_mem`; its shim constructs a replacement native state,
  swaps only after successful setup, and then destroys the old state. Reset
  timing includes this setup work; process and drain remain preallocated.
- Quality rendering must have exact complete length, terminal drain, finite
  samples, a finite/analyzable impulse peak and RMS, and analyzable 997 Hz and
  18 kHz tone fits. Silent or near-silent output is invalid; do not floor an
  undefined amplitude or THD+N into an apparent quality pass.
- Unavailable engines are explicit `unavailable` entries with a reason. A required
  unavailable engine writes any requested JSON first, then returns non-zero.
  An unavailable row is never counted as a successful measured case.
- Coverage claims are evaluated against the user-approved representative
  project inventory, not only the cases that happened to execute. Every
  required project must end as `measured`, `not-comparable`, or
  `infeasible-with-evidence`; `skipped`, `unavailable`, `deferred`, adapter
  placeholders, and lack of a prebuilt package are non-terminal. A report or
  document with a non-terminal project row must not be titled or described as
  `final`, `complete`, `all`, or universal coverage. Non-project limitations
  belong in `conditions.scope_boundaries`; schema v5 does not serialize the
  ambiguous `excludes` field.
- The report's top-level `coverage` payload contains `all_terminal` and exactly
  11 ordered entries. Each entry records `engine_id`, `state`, `terminal`,
  `measured_rate_pairs`, `case_keys`, and optional `evidence`. `measured`
  requires both canonical rate IDs. `not_comparable` and
  `infeasible_with_evidence` are terminal only with persisted evidence;
  `unavailable` is non-terminal. Reconstruct the table from actual case and
  unavailable rows and reject duplicates, partial rates, measured/unavailable
  conflicts, unknown IDs, omissions, or a mismatched serialized table.
- `--require-complete-matrix` does not suppress partial evidence. Print and
  write requested JSON first, then fail with every non-terminal engine and its
  evidence. The primary all-features evidence report uses this flag and must
  contain 11 measured entries and 22 cases. A Rubato-only supplementary report
  intentionally records raw libsoxr as unavailable and must not be presented
  as the complete representative matrix.
- Formal `--require-complete-matrix` runs require verified provenance for every
  runtime-loaded native identity. A caller-provided payload with only a path or
  self-reported metadata is insufficient: its bytes must match a pinned source
  and build identity. The representative 11-engine report therefore has eight
  unique provenance-verified native engines: libsamplerate plus seven shims.
- Baseline validation, required-engine enforcement, complete-coverage
  enforcement, formal provenance, and `--enforce` work/quality failures are
  accumulated into top-level `run_failures`. Print and write the fully populated
  report, read it back, and only then return non-zero. A pre-write validation
  error must not erase measured or unavailable evidence.
- JSON read-back keeps structure, keys, arrays, strings, integers, and booleans
  exact. Finite floating-point values may differ by at most four machine
  epsilons after serialization; diagnostics name the first differing path.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Unknown `--require-engine` value | named argument error listing known IDs |
| Unknown/duplicate `--raw-rubato-geometry`, or use without the Rubato feature | named argument/feature error before discovery |
| Invalid `--pinned` / `--pin-core`, affinity, or priority state | fail before timing; do not label the report pinned |
| `--engine-library` has an unknown/non-shim ID, empty path, or duplicate ID | named argument error before discovery |
| No explicit libsamplerate path | `unavailable`, with argument/environment guidance |
| Explicit DLL path cannot load, ABI/symbol/engine ID is wrong, or metadata is empty | `unavailable` with the concrete path/ABI reason |
| Factory cannot create either canonical rate pair | mark the engine unavailable; do not retain a partial measured case |
| Common-capacity probe fails, or a timed adapter advertises more than the selected capacity | named engine/rate error before measurement |
| Unavailable engine is required | requested JSON contains `required=true`, then process exits non-zero |
| Complete matrix contains any `unavailable` row | requested JSON contains all 11 coverage rows, then process exits non-zero |
| Complete matrix uses an unverified native payload | append a named provenance failure, persist JSON, then exit non-zero |
| Native consumed/produced count is negative or exceeds capacity | reject before cursor advance or slice |
| libsamplerate final real input is not fully consumed | named final-input error; do not enter drain |
| libsamplerate is drained before real input carried `end_of_input=1` | named lifecycle error |
| libsamplerate exact length uses nearest rounding instead of ceiling | work gate fails; 2,560 frames at 44.1-to-48 must produce 2,787 |
| Complete output differs from the engine's duration-plus-latency contract | work gate fails under `--enforce` |
| Compensated raw Rubato exposes native leading delay or emits beyond rounded duration | strict control/reset oracle fails; do not retain the case |
| Warm-up/timed consumption or warm-up + steady + drain total differs in any trial | named per-trial work gate fails; no output slack is allowed |
| Impulse/tone output is silent, below analyzable energy, non-finite, or has undefined THD+N | quality row is invalid and `--enforce` records a run failure |
| Reset output differs from either the second reset render or a fresh instance | engine/rate reset-fresh oracle fails with the first differing sample |
| Measured engine has only one canonical rate, duplicate rate/case, or is also unavailable | coverage construction fails by engine/rate name |
| SpeexDSP reset output differs from a fresh state | ignored native evidence test reports first sample/frame/channel/value difference |
| Shim `process`/`reset` throws C++ | ABI v2 catches it and returns a diagnostic; no exception crosses C |
| Pinned git source is dirty or a package/file/tree manifest hash differs | provisioning stops before compilation and names the mismatched input |
| Baseline format, algorithm, recipe, conditions, case set, or environment differs | reject before timing percentages |
| Baseline/required/coverage/provenance/enforce check fails | append to `run_failures`, persist/read back the report, then exit non-zero |
| JSON structure or non-float value changes on read-back, or `excludes` reappears | report-integrity failure naming the field path |

### 5. Good / Base / Bad Cases

- Good: build all seven pinned shims, load every native DLL through an explicit
  canonicalizable path, require complete coverage, retain binary/source
  identity, use one common per-rate output capacity, and compare project SoXR
  with raw libsoxr in the same 22-case all-feature report. Run the separate
  Rubato-only 1024/2 project/raw control under identical pinned conditions.
- Base: run without libsamplerate during ordinary development; the report keeps
  project/raw controls plus visible non-required unavailable coverage entries.
- Bad: silently locate a DLL through `PATH`, report an unavailable engine as
  passed, or commit the acquired package/DLL.
- Bad: rank f32 `SRC_SINC_BEST_QUALITY` against f64 engines as a strict speed
  regression without matching format, response, and latency.
- Bad: place project SoXR and project Rubato values from separate feature
  builds in one table and describe their absolute difference as a controlled
  backend win.
- Bad: consume the last real block with `end_of_input=0`, then send an empty
  `end_of_input=1` call and accept the resulting short output.
- Bad: allow one output buffer of work slack, accept exact-length silence, or
  use an impulse peak as a substitute for first-output buffering latency.
- Bad: trust a native reset API without a fresh-stream oracle, or manually mark
  a coverage row measured without two actual case keys.
- Bad: fail baseline validation before writing JSON, or label caller-provided
  native bytes as verified merely because the shim reports a pinned revision.

### 6. Tests Required

- Unit tests cover native count bounds, unique discovered IDs, required-flag
  preservation, explicit argument/environment/shim parsing, unknown and
  duplicate IDs, canonical create-probe failures, exact rational output
  rounding, the libsamplerate 2,560-frame ceiling regression, common-capacity
  selection/serialization, pinned-argument parsing, raw Rubato delay
  compensation and exact-duration drain, 1024/2 bit equality with the
  production Rubato stream, reset/fresh bit equality, exact per-trial output
  totals, silent/non-analyzable quality rejection, separated latency fields,
  complete/non-terminal/partial coverage tables, formal provenance, and
  float-tolerant JSON read-back with a field-path failure.
- Run `cargo test --all-features` and
  `cargo test --no-default-features --features rubato` so both project backend
  identities and available raw controls compile and execute.
- The ignored native evidence test loads all seven shims through
  `AUDIO_BENCH_NATIVE_SHIM_TEST_DIR`, runs both canonical rates with a
  4,097-frame irregular chunk schedule, and asserts ABI v2,
  metadata/hash/runtime identity, bounded progress, exact length, finite output,
  complete/idempotent drain, and reset/fresh bit equality. Run it under both
  feature matrices.
- The formal all-features run uses explicit pinned paths, `--enforce`,
  `--require-complete-matrix`, and `--out`; it must contain 11 terminal measured
  entries, 22 valid cases, exact complete outputs, zero `run_failures`, and
  eight unique provenance-verified native engines. A claim-bearing timing run
  also uses `--pinned --pin-core <core>` and retains the effective scheduling
  state.
- The Rubato-only run requires libsamplerate and all seven shims, selects raw
  Rubato `1024/2` for the strict production control, retains 20 valid cases, and
  marks raw libsoxr unavailable. A deliberately incomplete complete-matrix or
  incompatible-baseline run must write its 11-row JSON and populated
  `run_failures`, then exit non-zero with every failure named. Its conditions
  contain `scope_boundaries` and no `excludes` key.
- Provisioning validation must pass with the pinned clean sources and fixed
  package/import/runtime/tree hashes; a dirty-source or changed-hash fixture
  must stop before producing a formal shim.
- Run both strict Clippy feature matrices and rustfmt after adapter/report
  changes. Regenerate formal JSON after any setup- or timing-affecting change.

### 7. Wrong vs Correct

#### Wrong

```rust
coverage.push(EngineCoverage::measured("speexdsp")); // no actual rate evidence
speex_resampler_reset_mem(state); // assumed fresh without a stereo oracle
let db = db_ratio_with_floor(0.0, reference); // silence becomes a fake number
validate_baseline(&report)?; // exits before requested evidence is written
```

#### Correct

```rust
let coverage = build_coverage(report.cases(), report.unavailable())?;
assert_eq!(coverage.entries.len(), ALL_ENGINE_IDS.len());
assert!(coverage.entries.iter().all(|entry| {
    entry.state != CoverageState::Measured || entry.case_keys.len() == RATE_PAIRS.len()
}));
validate_reset_matches_fresh(factory, rate, inputs)?;
assert_eq!(warmup_output + steady_output + drain_output, expected_output);
report.run_failures.extend(run_all_deferred_checks(&report));
write_and_read_back(&report)?;
```
