# Complete Audio Engine Performance Benchmark Coverage

## Goal

Build a staged, versioned benchmark program that covers the crate's owned
realtime, decoder, DSP, analysis, memory, and lifecycle performance surfaces.
The result must distinguish portable work/report integrity from machine-local
timing regression evidence and must not overclaim device-level coverage that
belongs to a consuming application.

## What I already know

* The crate currently registers nine custom-main benchmarks.
* Existing evidence is strongest for steady-state DSP callback throughput,
  convolution, resampling, FIR EQ, offline rendering, and objective audio
  quality.
* `audio_callback_chain_perf` reports median/p95 values over trial averages; it
  does not retain a per-callback full-chain p99/p99.9/max distribution or count
  missed callback deadlines.
* `audio_convolver_perf` is the exception: its callback-burst cases retain
  per-call p95/p99/max values and support a Windows pinned gate.
* Decoder evidence is fixture-dependent and gapless-focused. General codec
  throughput, first-PCM latency, seek distributions, allocation, HTTP, and
  cancellation performance are not covered.
* `SpectrumAnalyzer`, `Downmixer`, AutoMix analysis, `RingBuffer`, isolated
  loudness analysis, and `LoudnessDatabase` have no dedicated performance
  benchmark.
* Offline-render allocation accounting excludes native SoXR allocations and
  does not establish process RSS or long-run memory stability.
* GitHub CI runs five quick report/gate commands. Without a compatible
  same-machine baseline, timing remains report-only.
* The crate owns no audio device. CPAL/WASAPI/device-callback and user-visible
  playback latency belong in the consuming application, not this crate.
* Both `cargo check --benches` and
  `cargo check --benches --no-default-features --features rubato` currently
  pass.
* The user selected the risk-first ordering, so callback-chain tail evidence was
  implemented first. It remains a distinct intermediate milestone; the parent
  program now also contains decoder, component, and lifecycle-memory evidence.

## Delivery Constraints

* Use this task as the parent coverage program and keep the implementation in
  independently reviewable probes, but complete every crate-owned phase before
  marking the parent task done.
* Add new probes to the versioned JSON/report conventions in `benches/support`
  instead of introducing Criterion or a second benchmark framework.
* Keep quick mode local-development friendly and deterministic; external
  corpora and live network services remain optional or excluded.
* A completed phase is only an intermediate milestone. It must not be reported
  as completion while any required crate-owned performance surface below is
  still uncovered.

## Requirements

### Common evidence contract

* Every standardized probe records schema version, stable probe/case identity,
  environment, explicit conditions and exclusions, raw samples, work
  validation, and JSON output.
* Timing comparisons require compatible same-machine baselines. Portable CI
  must not impose universal nanosecond claims from shared runners.
* Existing benchmark case keys remain stable unless a deliberate schema or
  algorithm identity change makes old evidence incompatible.
* Quick/full/heavy workloads must have documented intent and bounded runtime.
* Default SoXR and Rubato-only configurations must compile; backend-specific
  probes must identify the compiled backend in reports and case keys.

### Confirmed phase 1: full-chain callback tail

* Add per-callback raw timing distributions for representative bypass and
  active DSP-chain scenarios without changing the meaning of the existing
  aggregate callback report.
* Add a standalone `audio_callback_tail_perf` probe and preserve
  `audio_callback_chain_perf` report semantics, stable case keys, and
  historical baseline compatibility.
* Share the canonical callback fixture and chain configuration between the two
  probes instead of copying setup logic.
* Retain every measured callback duration and report min, median, p95, p99,
  p99.9, max, deadline-utilization percentiles, missed-deadline count, and
  missed-deadline rate.
* Cover representative callback frame sizes and the active chain both with and
  without convolution.
* Support Windows processor pinning and priority configuration, record the
  effective scheduling mode, and keep pinned evidence baseline-incompatible
  with ordinary runs.
* Gate selected median and tail metrics only against a compatible same-machine
  baseline; reject incompatible environment, scheduling, backend, schema, or
  case-set comparisons rather than silently comparing them.
* Document the probe contract, timer scope, scheduler caveats, deadline model,
  quick/full intent, baseline workflow, and device-level exclusions.
* Add a shared-runner CI quick invocation that validates execution and report
  integrity in report-only mode, without enforcing portable absolute timing
  thresholds.
* Validate finite timing, complete DSP work, finite changed output where
  expected, unique case keys, and exact raw-sample counts.

### Decoder and startup

* Add a standalone `audio_decoder_perf` probe backed by a deterministic local
  PCM WAV fixture generated outside every timed region. Quick mode must not use
  live network access or depend on an external corpus.
* Separate local source open, container probe, decoder build, first borrowed
  PCM, steady borrowed decode, and seek timing. Do not combine these phases into
  one startup number.
* Retain raw trial samples and report min/median/p95/max for aggregate phases;
  seek additionally retains and reports p99. Validate every decoded stream by
  sample rate, channel count, frame count, finite output, and a deterministic
  checksum.
* Report steady decode throughput in frames/second and realtime factor, with
  the input duration and decoded frame count explicit.
* Measure Rust allocation calls, peak live bytes, and retained bytes for probe,
  build, first borrowed PCM, and steady borrowed decode. Report the exact
  crate-owned fixed staging bytes and explicitly disclose that opaque
  Symphonia/native allocator ownership is not separately attributable.
* Record fixture container, codec, PCM format, sample rate, channels, frames,
  duration, byte length, and deterministic content hash in the JSON conditions.

### Public component performance

* Add a standalone `audio_component_perf` probe for `SpectrumAnalyzer`,
  `Downmixer`, `LoudnessMeter`, `TruePeakDetector`, AutoMix analysis, and
  `RingBuffer`.
* Cover representative mono/stereo/surround or geometry variants where cost
  scales materially. Every case must have a stable key, raw trials, a declared
  primary unit, validated work counts, and finite/nontrivial output evidence.
* AutoMix must use the same deterministic local fixture contract as the decoder
  probe and record Head/Full mode behavior without live network access.
* When `loudness-db` is compiled, cover in-memory database open, single upsert,
  indexed get, batch upsert, and stats at bounded row counts. When the feature
  is absent, record an explicit feature exclusion rather than silently omitting
  the surface.

### Memory and lifecycle performance

* Add a standalone `audio_lifecycle_memory_perf` probe for representative DSP
  setup, reset, finish/drain, persistent working storage, and bounded repeated
  lifecycle behavior.
* Measure Rust allocation calls, peak live bytes, and retained bytes around
  setup and lifecycle operations. Keep final caller-owned buffers distinct
  from processor working storage, and explicitly disclose opaque native SoXR
  allocations that the Rust global allocator cannot see.
* Cover equal-rate and active resampler lifecycle, finite Convolver finish
  drain, and dynamic Convolver publication/adoption/retirement/reclamation.
  Validate exact progress, terminal idempotence, generations/counters, and
  authoritative quiescence.
* Include bounded repeated/soak evidence with declared iteration counts and a
  retained-Rust-byte growth result. Quick mode must remain bounded; it is not a
  claim about unbounded process RSS stability.

### Automation and documentation

* Register all new probes as custom-main benches and compile them under default
  SoXR and Rubato-only feature matrices.
* Shared CI runs every new quick probe with `--enforce --out` to validate work
  and report integrity, while absolute timing remains report-only without an
  explicitly compatible same-machine baseline.
* Each probe supports compatible-baseline median comparison using the shared
  environment/schema/mode/conditions/case-set rules.
* Document commands, timer/allocation scopes, quick/full/heavy intent, fixture
  provenance, native-memory limitations, feature exclusions, and the explicit
  boundary that CPAL/WASAPI/device latency belongs to a consuming application.

## Technical Approach

Add a separate `audio_callback_tail_perf` custom-main probe rather than changing
the measurement semantics or report schema of `audio_callback_chain_perf`.
Reuse the existing callback scenarios and canonical `OutputChainBuilder`
configuration through a bench-local shared fixture, and reuse the convolver
probe's per-call distribution and pinned scheduling patterns. Keep aggregate
throughput and per-callback tail evidence as distinct probes with distinct
baseline identities.

The tail probe measures one `Instant` interval per callback invocation. It does
not subtract timer overhead from samples because subtraction can hide real
scheduler outliers; timer/copy scope is declared in report conditions. Quick
mode retains enough callbacks for a meaningful nearest-rank p99.9 while keeping
the JSON and runtime bounded.

Windows pinning and distribution/report helpers should be generalized from the
existing convolver benchmark only where their contracts truly match. Reusing a
helper must not alter `audio_convolver_perf` output or enforcement semantics.
The shared CI command exercises the unpinned quick path with report validation;
strict timing comparison is reserved for an explicitly pinned, compatible
same-machine baseline/candidate pair.

## Decision (ADR-lite)

**Context**: The existing callback benchmark deliberately averages many calls
inside each trial. Adding per-call timers to that path would mix throughput and
tail methodologies and either invalidate historical baselines or make one
report carry two different sampling contracts.

**Decision**: Keep `audio_callback_chain_perf` as the aggregate-throughput
probe and add `audio_callback_tail_perf` for raw per-callback distributions.
Share benchmark fixture/configuration code instead of duplicating the active
chain setup.

**Consequences**: Existing callback case keys and baselines retain their
meaning. The new probe has its own JSON/baseline history and a slightly larger
bench-local support surface. Any shared-fixture refactor must prove the existing
aggregate cases still configure the same stages and work validation.

### Delivery grouping

The post-tail implementation was grouped into decoder, component, and
memory/lifecycle probes so each report has one coherent measurement contract.
These were required deliverables of this task, not optional later phases.

## Acceptance Criteria

### Full-program acceptance

* [x] `audio_decoder_perf` covers deterministic local open/probe/build,
      first-borrowed-PCM, steady decode throughput/realtime factor, seek
      distribution, allocation, staging, and fixture metadata.
* [x] `audio_component_perf` covers SpectrumAnalyzer, Downmixer,
      LoudnessMeter, TruePeakDetector, AutoMix, RingBuffer, and either measured
      or explicitly feature-excluded LoudnessDatabase cases.
* [x] `audio_lifecycle_memory_perf` covers setup/reset/finish/drain,
      persistent Rust working storage, native-allocation disclosure, dynamic
      Convolver publication/reclamation, and bounded repeated-lifecycle growth.
* [x] Every new report round-trips JSON, validates unique complete case sets and
      raw-sample lengths, and rejects incompatible baselines before comparison.
* [x] All new quick probes run in shared CI as work/report-integrity gates with
      no portable absolute nanosecond threshold.
* [x] Default SoXR and Rubato-only bench compilation, strict Clippy, focused
      support tests, and real quick report generation pass, subject only to
      explicitly documented unrelated pre-existing failures.
* [x] Documentation and Trellis specs describe every measured surface,
      allocation/timer limitation, feature exclusion, and the crate/device
      ownership boundary.

### Phase 1 acceptance (complete)

* [x] The selected first-slice probe builds under default SoXR and Rubato-only
      feature configurations.
* [x] `audio_callback_tail_perf` is registered as a custom-main benchmark and
      does not replace or rename `audio_callback_chain_perf` cases.
* [x] Quick mode completes with `--enforce --out <json>` and the JSON round
      trips through its report type.
* [x] Every measured case retains the declared number of raw samples and
      exposes median/p95/p99/p99.9/max and deadline fields with finite values.
* [x] Every case records missed-deadline count/rate without deleting scheduler
      outliers.
* [x] Work validation proves the intended processor path executed.
* [x] Windows pinned quick execution records the requested/effective affinity
      and priority state, and fails clearly when requested controls cannot be
      established.
* [x] A compatible same-machine baseline can gate the selected median and tail
      metrics; incompatible environment, pinning mode, or case sets are
      rejected.
* [x] Documentation states exactly what is and is not measured and includes
      reproducible unpinned, pinned, baseline, and candidate commands.
* [x] Shared-runner CI executes the quick probe in report-only mode and checks
      the generated report without treating shared-runner timing as a portable
      regression gate.
* [x] Existing benchmark commands continue to compile and their established
      case meanings are preserved.

## Phase 1 Verification Evidence

* Default and Rubato-only `cargo check --benches` pass; both strict Clippy
  matrices pass with `-D warnings`.
* `tests/benchmark_support.rs` passes 18/18 under both all-features/SoXR and
  Rubato-only builds. The default all-features suite passes 351 unit tests,
  18 benchmark-support tests, 3 Windows runtime tests, and 2 doctests.
* The current unpinned, pinned baseline, and pinned candidate quick reports
  each contain 12 unique cases and exactly 48,000 raw samples. All work is
  valid and all callbacks met the modeled deadline.
* The compatible pinned candidate records processor group 0, affinity mask
  `0x4`, process class `0x80`, and thread priority 2; all 24 active-chain
  median/p99/p99.9 comparisons pass. A current-schema pinned candidate rejects
  an unpinned baseline with a named conditions mismatch.
* `audio_callback_chain_perf --quick --enforce` still passes after fixture
  extraction with its existing keys and aggregate trial semantics.
* Repository-wide `cargo fmt --all -- --check` remains blocked only by
  unrelated resampler WIP in `contiguous_polyphase_backend.rs` and
  `rubato_backend.rs`; every Rust file owned by this task passes rustfmt.
* The Rubato-only full suite is blocked by eight unrelated nonlinear resampler
  WIP cases. Six hit the history-window assertion in
  `contiguous_polyphase_backend.rs:267`; two no-allocation cases attempt a
  30-byte allocation and abort. With those eight named cases skipped, the
  remaining 378 library tests, 18 benchmark-support tests, 3 Windows runtime
  tests, and 2 doctests pass.

## Full-Program Verification Evidence

* Current quick reports pass for callback-tail (12 cases/48,000 callbacks),
  decoder (7 SoXR + 7 Rubato cases), components (16 SoXR + 11 Rubato cases),
  and lifecycle-memory (13 SoXR + 13 Rubato cases). Every case key is unique,
  every raw-sample length matches its declaration, and every work validation
  is true.
* Both lifecycle reports contain nine allocation rows, three persistent-memory
  rows, and five 128-cycle soak trials. Every complete soak trial retains zero
  Rust bytes. The Rubato component report explicitly excludes `loudness-db`.
* Compatible pinned callback-tail plus SoXR/Rubato decoder, component, and
  lifecycle candidates contain 79 passing comparisons and zero failures.
  Lifecycle compares the seven stable cases while retaining all 13 cases in
  compatibility and report-integrity checks.
* Both `cargo check --benches` feature matrices and both strict
  `cargo clippy --all-targets ... -- -D warnings` matrices pass. Every
  task-owned Rust file passes `rustfmt --check`.
* After the final callback-fixture parameter-group review, both callback probes,
  both support-test matrices, both bench-compilation matrices, and both strict
  Clippy matrices were rerun successfully. The refreshed unpinned tail report
  has SHA-256
  `144ADC36B33782D12AE859538B1C0F2E8203973E97CFCC723C2CC6F188505363`.

## Definition of Done

* Focused unit/support tests cover distribution boundaries, allocation
  accounting, report validation, baseline compatibility, CLI parsing, fixture
  determinism, feature exclusions, and lifecycle work validation.
* Every task-owned Rust file passes rustfmt; repository-wide formatting status
  is reported honestly if unrelated dirty work blocks the global check.
* Default and Rubato-only bench compilation and strict Clippy pass.
* All four task-owned quick probes (callback-tail plus the three remaining
  probes) execute with `--enforce --out`, and their inspected JSON evidence is
  stored under this task's `research/` directory.
* Callback-tail unpinned/pinned/baseline evidence and compatible baseline
  comparison remain valid.
* Relevant test matrices pass, with unrelated pre-existing failures separated
  by file and test name rather than hidden.
* `docs/quality.md`, benchmark registration, report-only CI wiring, and Trellis
  specs contain the full-program contracts.
* No required crate-owned benchmark phase remains described only as future or
  follow-up work.

## Implemented Plan

1. Preserved the callback fixture/tail probe and its evidence.
2. Extracted shared allocation and deterministic fixture helpers while
   preserving existing output-render semantics.
3. Added `audio_decoder_perf` with phase-separated timing, allocation, fixture,
   seek, work-validation, and compatible-baseline contracts.
4. Added `audio_component_perf` with the complete public component matrix and
   feature-gated LoudnessDatabase disclosure.
5. Added `audio_lifecycle_memory_perf` with setup/reset/finish/drain, working
   memory, Convolver handoff/reclamation, and bounded soak evidence.
6. Added support tests, Cargo registration, report-only CI, user docs, and
   executable Trellis contracts.
7. Ran both feature matrices and real quick/baseline reports and persisted the
   inspected evidence under `research/`.

## Out of Scope (explicit)

* CPAL/WASAPI/device output and end-to-end user-visible playback latency inside
  this crate; those require a consuming-application integration benchmark.
* Live public-network traffic in deterministic quick benchmarks.
* Universal absolute timing claims across unrelated machines.
* Rewriting all existing benchmark harnesses or replacing their evidence format
  in one change.

## Research References

* [`research/current-coverage-and-phasing.md`](research/current-coverage-and-phasing.md)
  - historical pre-implementation inventory and recommended delivery order.
* [`research/final-coverage-and-results.md`](research/final-coverage-and-results.md)
  - final crate-owned coverage matrix, benchmark results, exclusions, baseline
    evidence, and quality-gate status.
* [`research/callback-tail-unpinned-quick-20260725.json`](research/callback-tail-unpinned-quick-20260725.json)
  — current-schema report-only quick evidence.
* [`research/callback-tail-pinned-baseline-quick-20260725.json`](research/callback-tail-pinned-baseline-quick-20260725.json)
  and
  [`research/callback-tail-pinned-candidate-quick-20260725.json`](research/callback-tail-pinned-candidate-quick-20260725.json)
  — compatible Windows pinned baseline/candidate evidence with 24 passing
  active-chain comparisons.
* [`research/callback-chain-post-fixture-quick-20260725.json`](research/callback-chain-post-fixture-quick-20260725.json)
  — aggregate callback regression evidence after shared-fixture extraction.

## Technical Notes

* Existing patterns: `benches/support/mod.rs`,
  `benches/audio_callback_chain_perf.rs`, and
  `benches/audio_convolver_perf.rs`.
* Coverage boundaries: `src/lib.rs`, `src/processor/mod.rs`, and
  `.trellis/spec/backend/directory-structure.md`.
* Evidence rules: `.trellis/spec/backend/quality-guidelines.md` and
  `.trellis/spec/backend/realtime-safety.md`.
* CI entry point: `.github/workflows/ci.yml`.
* Related completed/in-progress work:
  `.trellis/tasks/07-24-expand-benchmark-matrix/`.
