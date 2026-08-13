# Journal - Asaiuta (Part 1)

> AI development session journal
> Started: 2026-06-12

---



## Session 1: Trellis bootstrap: PRD review + source-backed backend spec

**Date**: 2026-06-14
**Task**: Trellis bootstrap: PRD review + source-backed backend spec
**Branch**: `main`

### Summary

Reviewed all 11 Trellis PRDs against source; fixed parent roadmap (4->9 child tasks listed, spec-bootstrap sequenced first as hard prerequisite) and corrected decoder PRD's NoAudioTrack wording. Added P1/P2/backlog/release-gate priority tiers, bumping the decoder seek double-trim bug fix ahead of DSP enhancements. Implemented the spec-bootstrap task: rewrote 6 placeholder backend spec files with source-backed content and added realtime-safety.md (hot-path invariant), all verified by trellis-check against live source (1 fix: missing loudness.rs in tree). Committed whole .trellis/ system; gitignored per-developer agent tooling dirs.

### Main Changes

- Made `StreamingResampler` implement the unified streaming contract with exact
  SoXR consumed/produced progress, native drain-to-zero, native clear on reset,
  and allocation-free process/finish paths.
- Added stage-complete offline finalize with compensated and raw-causal
  timelines, explicit latency/tail metadata, and downstream tail propagation.
- Distinguished limiter look-ahead latency from convolution semantic tail and
  added pre-dither unknown-tail energy termination with a hard truncation cap.
- Migrated examples/benches off the removed resampler convenience API and
  captured the resulting contracts in backend Trellis specs.

### Git Commits

| Hash | Message |
|------|---------|
| `e629b07` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 2: Implement quality-gates: enforceable benchmark gates + JSON evidence

**Date**: 2026-06-14
**Task**: Implement quality-gates: enforceable benchmark gates + JSON evidence
**Branch**: `main`

### Summary

Started the P1 quality-gates task (corrected the auto-selected current-task pointer off the backlog channel-layout task first). Implemented report/gate/skipped metric classification in audio_quality_measurements with --enforce (non-zero exit + named measured-vs-threshold diagnostics) and --out JSON evidence; conservative thresholds (tight only for ebur128 bit-parity). Added benchmark-inventory.md for all six benches and recorded the benchmark gate convention in quality-guidelines.md. trellis-check verified all 8 acceptance criteria incl. a fault-injection proof of gate-failure diagnostics; zero src/ impact. The implement sub-agent was rate-limited mid-run but its work was complete and verified post-hoc.

### Main Changes

- Marked 11 completed non-codebase tasks as `completed` and moved them to
  `.trellis/tasks/archive/2026-07/`.
- Kept the codebase maintainability audit and its 11 remediation tasks active.
- Preserved all existing dirty source, spec, benchmark, documentation, and
  unrelated untracked files.

### Git Commits

| Hash | Message |
|------|---------|
| `0842efd` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 3: Unified output render chain

**Date**: 2026-06-20
**Task**: Unified output render chain

### Summary

Unified the canonical output chain builder across offline quality rendering and callback/perf construction; preserved true-peak evidence as measured/report-only where appropriate.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `3ec675c` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 4: Partitioned convolution long-IR routing

**Date**: 2026-06-26
**Task**: Partitioned convolution for long impulse responses
**Branch**: `feat/channel-layout-mixing`

### Summary

Implemented the partitioned convolution task: `FFTConvolver` now keeps the
existing overlap-save engine for short/FIR EQ impulse responses and routes long
IRs to a uniform 1024-frame partitioned tail with an overlap-save head. Added
public strategy/threshold metadata, expanded convolver and FIR EQ benches to
report routing evidence, refreshed README performance/routing notes, and
captured the contract in backend specs.

### Main Changes

- Added `ConvolutionStrategy`, `PARTITIONED_CONVOLUTION_IR_THRESHOLD`, and
  `PARTITIONED_CONVOLUTION_PARTITION_SIZE` to the public convolver surface.
- Added long-IR partitioned processing with precomputed FFT plans/spectra,
  per-channel history, reset handling, and allocation-free steady-state
  `process_into`/`process_inplace` paths.
- Added correctness coverage against the overlap-save reference for stereo,
  mono, and 6-channel IRs, plus cross-buffer continuity, reset, in-place, and
  no-allocation tests.
- Extended `audio_convolver_perf` and `audio_fir_eq_perf` to report selected
  strategy, FFT size, and partition size across short/medium/long IR scenarios.

### Git Commits

| Hash | Message |
|------|---------|
| `0cc5626` | (see git log) |

### Testing

- [OK] `cargo fmt --check`
- [OK] `cargo test convolver --lib`
- [OK] `cargo test fir_eq --lib`
- [OK] `cargo test processor::adapters --lib`
- [OK] `cargo test --lib`
- [OK] `cargo check --benches`
- [OK] `cargo clippy --all-targets -- -D warnings`
- [OK] `cargo bench --bench audio_convolver_perf -- --quick`
- [OK] `cargo bench --bench audio_fir_eq_perf -- --quick`

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 5: Oversampled saturation validation

**Date**: 2026-06-26
**Task**: Oversampled anti aliasing saturation
**Branch**: `feat/channel-layout-mixing`

### Summary

Verified and closed the oversampled saturation task that was implemented in
`bb715bf`. The implementation exposes explicit Direct/Oversampled2x/Oversampled4x
quality modes through both direct `Saturation` control and lock-free callback
params, preserves pre-sized per-channel state, and backs the aliasing claim with
the `audio_quality_measurements` saturation alias gate.

### Main Changes

- Confirmed `SaturationQuality::Oversampled4x` reduces fitted folded alias
  energy versus the Direct Tube path by 16.56 dB in the quick quality bench.
- Confirmed callback-chain quick bench remains far below a 512-frame/48 kHz
  callback period with Oversampled4x enabled in the active DSP scenarios.
- Re-ran focused saturation/adapters tests, full lib tests, bench compilation,
  clippy, and quick performance/quality benches before archiving the task.

### Git Commits

| Hash | Message |
|------|---------|
| `bb715bf` | (see git log) |

### Testing

- [OK] `cargo fmt --check`
- [OK] `cargo test saturation --lib`
- [OK] `cargo test processor::adapters --lib`
- [OK] `cargo test --lib`
- [OK] `cargo check --benches`
- [OK] `cargo clippy --all-targets -- -D warnings`
- [OK] `cargo bench --bench audio_callback_chain_perf -- --quick`
- [OK] `cargo bench --bench audio_quality_measurements -- --quick`

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 6: EQ perceptual DSP evidence

**Date**: 2026-06-26
**Task**: EQ perceptual DSP evidence

### Summary

Fixed crossfeed adapter mix-change continuity, added listening DSP quality metrics for EQ/crossfeed/dynamic loudness, updated backend quality spec, and archived the EQ perceptual DSP task.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `aebda99` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 7: Audio engine release hardening

**Date**: 2026-06-26
**Task**: Audio engine release hardening

### Summary

Closed the release-hardening gate, updated public docs and release evidence, captured SoXR/package checklist guidance, validated the crate release surface, and archived the completed audio-engine feature roadmap.

### Main Changes

- Completed the release-hardening gate for `audio-engine-core` and archived `06-12-audio-engine-api-release-hardening`.
- Fixed a rustdoc private intra-doc-link warning in `TruePeakDetector` documentation.
- Updated README/CHANGELOG/CONTRIBUTING/NOTICE so public release claims match current benchmark evidence and SoXR/native dependency reality.
- Added a release-readiness audit with current validation results and benchmark evidence, including listening-DSP metrics and the remaining report-only full output-chain true-peak limitation.
- Captured the release documentation checklist in `.trellis/spec/backend/quality-guidelines.md`.
- Synchronized and archived the parent `06-12-audio-engine-feature-upgrade` roadmap after all 10 child tasks were done.

### Testing

- [OK] `cargo build`
- [OK] `cargo build --no-default-features`
- [OK] `cargo build --no-default-features --features http`
- [OK] `cargo build --no-default-features --features loudness-db`
- [OK] `cargo test --all-features` (218 unit tests; doctests 1 passed, 1 ignored)
- [OK] `cargo test --no-default-features` (210 unit tests; doctests 1 passed, 1 ignored)
- [OK] `cargo fmt --check`
- [OK] `cargo clippy --all-targets --all-features -- -D warnings`
- [OK] `cargo doc --no-deps`
- [OK] `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`
- [OK] `cargo run --example resample_sine`
- [OK] `cargo run --example equalizer_curve`
- [OK] `cargo bench --bench audio_quality_measurements -- --quick --enforce`
- [OK] `cargo package --allow-dirty` (passed with non-sandbox permissions after sandbox registry credential failures)
- [OK] `task.py validate` for release-hardening and parent roadmap


### Git Commits

| Hash | Message |
|------|---------|
| `5d1448e` | (see git log) |
| `28f8f1f` | (see git log) |

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 8: Migrate fixed DSP chain to streaming contract

**Date**: 2026-07-17
**Task**: Migrate fixed DSP chain to streaming contract

### Summary

Migrated all fixed DSP adapters and the callback chain to StreamingProcessor, added fixed 1:1 lifecycle/error semantics, preserved realtime no-allocation behavior, documented the breaking API change, and recorded callback performance evidence.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `c4bbf2e` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 9: Finalize variable-I/O streaming and offline rendering

**Date**: 2026-07-17
**Task**: Finalize variable-I/O streaming and offline rendering

### Summary

Unified StreamingResampler with exact SoXR progress, native drain/reset, stage-complete offline finalize, latency compensation, semantic-tail preservation, and pre-dither energy termination with a hard cap.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `987c450` | feat(processor): finalize variable-I/O streaming offline |

### Testing

- [OK] 250 all-feature unit tests + 2 doctests
- [OK] 242 no-default-feature unit tests + 2 doctests
- [OK] Strict all-target Clippy for both feature configurations
- [OK] Strict rustdoc, resample example, objective audio-quality gate, and
  offline package verification

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 10: P0 DSP state and math correctness

**Date**: 2026-07-17
**Task**: P0 DSP state and math correctness

### Summary

Corrected EQ branch state adoption, loudness config publication, RBJ shelf math, and dynamic-loudness sample-rate state preservation; all test, Clippy, audio-quality, callback-performance, documentation, and package gates passed.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `a476e9a` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 11: Standardize audio quality and performance gates

**Date**: 2026-07-17
**Task**: Standardize audio quality and performance gates

### Summary

Standardized quality, callback, and streaming-resampler reports with versioned environment metadata, trial distributions, compatible 10% median baselines, CI quick artifacts, latency/tail evidence, documentation, and full release verification.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `5a10309` | (see git log) |
| `3c2f3a9` | (see git log) |
| `9eb1f5e` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 12: Fix AutoMix and FIR algorithms

**Date**: 2026-07-17
**Task**: Fix AutoMix and FIR algorithms

### Summary

Corrected AutoMix spectral cadence and explicit unsupported key status; fixed FIR one-tap gain, absolute magnitude, and minimum-phase taper; standardized FIR JSON/baseline performance gates with CI, docs, specs, and complete verification evidence.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `456e54b` | (see git log) |
| `0345b59` | (see git log) |
| `4503f37` | (see git log) |
| `87a8acc` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 13: Listening and nonlinear DSP correctness

**Date**: 2026-07-17
**Task**: Listening and nonlinear DSP correctness

### Summary

Corrected saturation continuity and gain order, continuous signed noise shaping, and Bauer crossfeed; expanded objective quality/performance gates; permanently deployed the Windows MSYS2 SoXR runtime closure for direct Cargo test/example/bench execution; documented and verified all contracts.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `e2cdf15` | (see git log) |
| `177a940` | (see git log) |
| `07b784a` | (see git log) |
| `c28444a` | (see git log) |
| `3864fc7` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 14: Complete convolver lifecycle and EBU quality verification

**Date**: 2026-07-18
**Task**: Complete convolver lifecycle and EBU quality verification

### Summary

Unified convolver control and reclamation across direct, callback, and offline paths; verified lifecycle, latency, tail, quality, and performance contracts; ran quick and full EBU Tech 3341/3342 corpus gates at 25/25 with zero skips; ignored the local corpus and archived the completed child and 8/8 parent tasks.

### Main Changes

- Replaced the hidden convolver disposal slot with explicit `ConvolverControl`
  publication, status, backpressure, and control-thread reclamation shared by
  direct, callback, and offline entry points.
- Preserved zero algorithmic latency, exact `IR length - 1` finite tail,
  idempotent finish/reset behavior, bounded audio-side ownership, and
  allocation-free callback adoption under concurrent publication stress.
- Added the local EBU corpus ignore rule and recorded the supplied v05 archive
  hash plus quick/full EBU Tech 3341/3342 conformance-gate evidence.

### Git Commits

| Hash | Message |
|------|---------|
| `ebea9be` | fix(processor): unify convolver control and reclamation |
| `a528296` | docs(task): record convolver lifecycle contract and evidence |
| `d732b72` | chore: ignore local EBU reference corpus |
| `72ee161` | docs(task): record EBU corpus verification |

### Testing

- [OK] `cargo test --all-features` and `cargo test --no-default-features`
- [OK] Strict Clippy for both feature matrices, rustfmt, and rustdoc
- [OK] Callback, FIR, and convolver quick performance gates
- [OK] Quality quick/full `--enforce` with EBU corpus: 25/25 gates, 0 skipped
- [OK] 10,000-publication stress, direct-convolution oracle, destructor-thread,
  and `assert_no_alloc` lifecycle coverage

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 15: Convolver realtime ownership and output-stage hardening

**Date**: 2026-07-18
**Task**: Convolver realtime ownership and output-stage hardening

### Summary

Replaced ArcSwap kernel ownership with fixed AtomicPtr handoff, enforced a single consumer lease, preserved finish tails across disable, added versioned quiescence checks, unified output-stage traversal, separated Meter analysis, and passed the full quality matrix.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `7eefc99` | (see git log) |
| `994e88b` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 16: DSP lifecycle performance and correctness

**Date**: 2026-07-19
**Task**: DSP lifecycle performance and correctness

### Summary

Unified realtime DSP lifecycle and Convolver ownership, fixed saturation/tail/rate-domain correctness, bounded offline rendering, added performance and true-peak gates, updated backend specs, and archived the task. Verification: 344/344 and 336/336 tests, strict Clippy, rustdoc, four build matrices, and offline package verification passed.

### Main Changes

- Unified fixed-stage streaming lifecycle, latency/tail composition, bounded
  finish driving, and callback/offline stage ordering.
- Replaced realtime Convolver `Arc` ownership with fixed atomic hand-off,
  enforced one live consumer, preserved locked finish tails, and added
  sample-rate-stamped kernel adoption plus off-RT reclamation.
- Corrected Saturation residual oversampling, fixed four-frame timing, sparse
  automation, final output-domain limiting, and bounded energy-based tail stop.
- Added callback/output-render CPU and memory baselines, true-peak and
  fundamental-preservation gates, CI quick reports, and executable backend
  specs.

### Git Commits

| Hash | Message |
|------|---------|
| `c973de3` | fix(processor): enforce DSP lifecycle and realtime ownership |
| `d3ea3c8` | perf(bench): add lifecycle CPU memory and quality gates |
| `d341514` | docs(task): record DSP lifecycle correctness and performance evidence |

### Testing

- [OK] `cargo test --lib`: 344 passed; no-default-features: 336 passed.
- [OK] Strict Clippy feature matrices, `cargo check --all-targets`, rustfmt,
  rustdoc warnings, and four release build configurations passed.
- [OK] Quick/full audio quality: 27/27 gates, 55 EBU loudness files, and 9 EBU
  true-peak files passed; final output remained at or below `-1.0 dBTP`.
- [OK] Callback and offline performance comparisons passed; offline temporary
  memory fell by 96.6% to over 99.6% in measured long-render cases.
- [OK] `cargo package --allow-dirty --offline` packaged and compiled 253 files;
  the online index attempt was blocked only by Schannel credentials.

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 17: Rubato 4 quality-aware resampler routing

**Date**: 2026-07-22
**Task**: Rubato 4 quality-aware resampler routing

### Summary

Upgraded the pure-Rust Rubato backend to quality-aware routing: common Low-through-High ratios use FFT, UltraHigh and pathological ratios retain sinc. Recorded same-machine performance and quality evidence, validated sub_chunks=2, and passed the full test, lint, docs, packaging, and benchmark gates.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `d550d57` | (see git log) |
| `90b6dab` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 18: Optimize Oversampled4x saturation performance

**Date**: 2026-07-22
**Task**: Optimize Oversampled4x saturation performance

### Summary

Specialized saturation oversampling into fixed 2x/4x block kernels, added bit-for-bit parity coverage, and recorded compatible callback, quality, and output-render evidence.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `f3b8a88` | (see git log) |
| `c7f0ba8` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 19: Optimize Saturation mirrored FIR history

**Date**: 2026-07-22
**Task**: Optimize Saturation mirrored FIR history

### Summary

Replaced circular oversampling FIR traversal with mirrored newest-to-oldest history, added an independent bit-for-bit legacy oracle, and validated stable callback, quality, output-render, Clippy, rustdoc, and packaging gates.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `f1f3c87` | (see git log) |
| `287b161` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 20: Long IR convolver RealFFT and layout optimization

**Date**: 2026-07-22
**Task**: Long IR convolver RealFFT and layout optimization

### Summary

Replaced long-IR complex tail FFTs with realfft half-spectra, flattened IR/history spectra with direct two-range ring traversal, expanded convolver throughput/callback evidence, selected 1024-frame partitions, and verified both feature matrices, convolver/FIR/callback benchmarks, fmt, check, Clippy, and unused-dependency checks.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `8fbcfb9` | (see git log) |
| `3fac4c2` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 21: Optimize 147:160 nonlinear resampling

**Date**: 2026-07-26
**Task**: Optimize 147:160 nonlinear resampling
**Branch**: `main`

### Summary

Added hybrid nonlinear routing with a contiguous polyphase backend for reduced up greater than 16; achieved a 5.00x 44.1-to-48 High/Minimum speedup while preserving quality, lifecycle, and Linear retention gates.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `d9f43dc` | (see git log) |
| `8cf09f3` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 22: Archive completed non-codebase tasks

**Date**: 2026-07-31
**Task**: Archive completed non-codebase tasks

### Summary

Archived 11 completed convolver, resampler, benchmark, and playback tasks; preserved the 12 codebase-audit tasks and all dirty source work.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `9d1eecc` | perf(convolver): spread long IR tail work |
| `abc05e4` | feat(resampler): implement nonlinear Rubato phases |
| `6e619ea` | perf(resampler): specialize exact 2x linear high upsampling |
| `705f8d7` | docs(resampler): record half-band routing and evidence |
| `0760265` | feat(bench): add resampler configuration matrix benchmark |
| `1d889a1` | perf(resampler): direct prefix-budget output for noninteger ratios |
| `80ee693` | perf(resampler): spectral FFT engine for nonlinear phases |
| `1798833` | perf(resampler): route UltraHigh Linear to single-subchunk FFT |
| `3da9a94` | bench(audio): complete performance coverage and resampler comparisons |
| `f0c0445` | perf(resampler): optimize stereo SoXR and Rubato adapters |
| `83753ce` | feat(playback): expose high-level playback pipeline |
| `0c62feb` | docs(audio): record benchmark, playback, and resampler evidence |

### Testing

- [OK] `cargo fmt --all -- --check`
- [OK] `git diff --check` (line-ending warnings only)
- [OK] Both supported strict Clippy matrices
- [OK] `cargo test --all-features`: 450 library tests plus support and doctests
- [OK] Rubato-only tests: 484 library tests plus support and doctests
- [OK] All 11 archive records report `status=completed` and
  `completedAt=2026-07-31`; only the 12 codebase-audit tasks remain active

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 23: Gate 2: decide the legacy public surface lifecycle

**Date**: 2026-08-03
**Task**: Gate 2: decide the legacy public surface lifecycle
**Branch**: `chore/gate2-legacy-public-surface`

### Summary

Audited the legacy public surface ahead of the 1.0 freeze (07-28 audit P2 #9): verified each item's disposition in code — removed VolumeController, GainRamp, AtomicDynamicLoudnessState, DEFAULT_BROADCAST_TARGET_LUFS, ConvolverControl::publish and DEFAULT_CONVOLVER_SAMPLE_RATE_HZ; narrowed BiquadSection to pub(crate); gated the rubato-only PolyphaseResampler under cfg(test); kept RingBuffer and the 4 downstream-used Group A items with support statements. Regenerated both public-API baselines (229-line pure-deletion diff). Updated 4 specs and CHANGELOG. Verified the full matrix: tests (all-features 454+20+2+25+3+6; rubato 485+...), clippy -D warnings, fmt, doc warning-free on both feature sets. A/B benchmark check (convolver/callback-chain/component/fir_eq, interleaved runs) showed no measurable change: cross-tree deltas overlap the same-binary run-to-run noise band. Downstream AudioPlayer follow-up recorded in the PRD.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `64f8de4` | (see git log) |
| `78afe04` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 24: Close out gates 7-8: semver/doc gate green, archive tasks

**Date**: 2026-08-11
**Task**: Close out gates 7-8: semver/doc gate green, archive tasks
**Branch**: `chore/gate2-legacy-public-surface`

### Summary

Gate 8 (enforce documented and semver-checked public API) implementation was committed earlier as 7c942ba: crate-level deny(missing_docs), 270 public items documented, rustdoc JSON baselines for both feature matrices committed under tests/semver-baseline/, CI semver gate wired with pinned cargo-semver-checks 0.50.0, negative control proven to fail. This session recorded the gate-8 evidence docs (prd, validation-2026-08-11, semver-baseline-runbook) and the quality-guidelines Release Documentation Checklist updates as a380e92, then archived gate 8 (aa40b07) and gate 7 tighten-resampler-facade-geometry-contract (3eef9e2). Working tree clean.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `7c942ba` | (see git log) |
| `a380e92` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 25: Release gate 9: cut 1.0.0, verify release, tag and push

**Date**: 2026-08-11
**Task**: Release gate 9: cut 1.0.0, verify release, tag and push
**Branch**: `chore/gate2-legacy-public-surface`

### Summary

Cut the stable 1.0.0 release (gate 9 of 9): Cargo.toml 0.1.0 -> 1.0.0, CHANGELOG [1.0.0] - 2026-08-11 entry from the 484-line Unreleased content, README stable-status wording (banner, Quick Start dep '1', Project Status), CONTRIBUTING 1.x SemVer policy + publish runbook. Verified the full Release Documentation Checklist locally: both feature matrices check/clippy(fmt)/doc -D warnings/tests (480 + 500 lib suites), public_api 2/2, cargo semver-checks 223/223 both matrices, cargo package --allow-dirty (776 files) with in-package verification. Decided immediate release (gate sequence + Lyne usage is the soak) and prepare+runbook publish handoff (no token on machine; 0.1.0 was already on crates.io since 2026-06-12 with no git tag — v0.1.0 backfilled at bf9addb). Tagged v1.0.0 at 57d59be, archived gates 3-6, and pushed branch + both tags to origin.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `57d59be` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 26: Publish audio-engine-core 1.0.0 to crates.io

**Date**: 2026-08-11
**Task**: Publish audio-engine-core 1.0.0 to crates.io
**Branch**: `chore/gate2-legacy-public-surface`

### Summary

Found the pre-existing cargo token (CARGO_HOME=D:\Rust\.cargo\credentials.toml, created 2026-06-12), verified it via cargo owner --list (Asaiuta, owner), and published audio-engine-core 1.0.0 live: cargo publish --dry-run then cargo publish both succeeded. crates.io API shows max_version/default_version 1.0.0 (num_versions 2); docs.rs build finished (1.0.0 + latest both 200). Updated the release evidence file (publish section now records the executed result instead of the handoff).

### Main Changes

(Add details)

### Git Commits

(No commits - planning session)

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 27: Restore dynamic-loudness curve tuning through the parameter layer

**Date**: 2026-08-13
**Task**: Restore dynamic-loudness curve tuning through the parameter layer
**Branch**: `main`

### Summary

Found three dynamic-loudness tuning values (pre_gain_db, transition_db, compensation onset) with no path through the parameter layer: pre_gain had no setter and was hardcoded -3dB; the other two had DSP-core setters the adapter never called. Published them via a second SharedParams on an independent generation counter, added DynamicLoudnessTuningSnapshot (non_exhaustive), facade config/getter/setter, and range constants. Purely additive - semver-checks 223/223 under --release-type patch. Named the new onset control compensation_ref_db to avoid collision with the existing ref_volume_db (listening volume). Also split out a pre-existing uncommitted NoiseShaper::process_sample change into its own commit first (2e992fe).

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `b74206e` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 28: Reclaim measured performance from existing dependencies

**Date**: 2026-08-13
**Task**: Reclaim measured performance from existing dependencies
**Branch**: `main`

### Summary

Narrowed ebur128 to I|LRA|HISTOGRAM (bit-exact, drops unread TRUE_PEAK work) and moved gating reads out of process (-92% at 512-frame blocks); migrated OverlapSaveConvolver to realfft (28/28 pinned cases faster, FIR EQ apply -29..-57%); removed rayon, arc-swap, atomic_float (151->142 default deps, 89->80 pure-Rust). Found that Cell breaks LoudnessMeter: Sync, so readers recompute instead of caching. Public auto-trait surface widened (8 types gained UnwindSafe); baselines regenerated with 0 non-unwind changes.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `pending` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 29: Reclaim measured performance from existing dependencies

**Date**: 2026-08-13
**Task**: Reclaim measured performance from existing dependencies
**Branch**: `main`

### Summary

See prd.md outcome section.

### Main Changes

(Add details)

### Git Commits

| Hash | Message |
|------|---------|
| `80d6b07` | (see git log) |

### Testing

- [OK] (Add test results)

### Status

[OK] **Completed**

### Next Steps

- None - task complete


## Session 30: Migrate remaining real-valued FFT call sites to realfft

**Date**: 2026-08-13
**Task**: Migrate remaining real-valued FFT call sites to realfft
**Branch**: `main`

### Summary

Finished the realfft migration begun for the convolvers. Three call sites fed
real audio through complex `rustfft` transforms and read only half the result.
`realfft` was already a dependency, so the dependency count is unchanged.

Surveyed every remaining `rustfft` site rather than only the ones flagged
earlier, which is how `fir_design` got correctly excluded.

### Main Changes

- `spectrum.rs` - `SpectrumAnalyzer` to realfft. It already read only bins
  `1..n/2`, so the complex upper half was pure waste. Storage halved.
- `fir_eq.rs` - `generate_linear_phase_ir` to a real inverse transform. It had
  been writing the mirrored Hermitian half by hand only to satisfy a complex
  IFFT; a real inverse implies that symmetry, so that half is gone.
- `automix_analysis.rs` - `SpectralFluxAccumulator` to realfft plus
  `process_with_scratch`, which also removed a per-hop scratch allocation.
- `fir_design.rs` - comment only. Excluded on purpose: the real-cepstrum
  factorization exponentiates a *complex* spectrum between the transforms, so
  the intermediate genuinely has a non-zero imaginary part. Only the endpoints
  are real.
- `docs/quality.md`, `.trellis/spec/backend/quality-guidelines.md` - measured
  results and the two reusable lessons.

### Git Commits

| Hash | Message |
|------|---------|
| `3703c9c` | perf: use realfft for the remaining real-valued FFT call sites |

### Testing

- [OK] `cargo test --all-features` - 513 + 20 pass
- [OK] `cargo test --no-default-features --features rubato` - 532 pass
- [OK] clippy `--all-features --all-targets` clean, `cargo fmt --check` clean
- [OK] `assert_no_alloc` steady-state tests pass
- [OK] Public API content byte-identical to the committed baseline
- [OK] Both new oracles verified to FAIL under deliberate mutation (off-by-one
  bin index; doubled spectrum), so they are not vacuous

Measured, interleaved, each with an unchanged in-run control:

| Case | Change | Role |
|---|---:|---|
| Spectrum 1,024 / 4,096-point | -12.4% / -23.9% | changed |
| Downmixer 5.1 / 7.1 | +2.2% / +0.7% | control |
| FIR EQ linear phase 511/1,023/2,047 taps | -7.1% / -14.2% / -16.7% | changed |
| FIR EQ minimum phase (same runs) | +2.3% / +5.6% / +4.9% | control |

### Notes / Judgement Calls

Two things I got wrong first and corrected:

1. **A sequential A/B reported a 4.7% AutoMix regression.** Rather than accept
   or hand-wave it, I isolated the accumulator (1.02-1.08x *faster*) and then
   ran interleaved end-to-end (-4.1%, +1.0%, +1.0%). It was host drift; decode
   dominates that case. AutoMix is reported as **neutral**, not improved.
2. **The unchanged minimum-phase FIR path drifted +5%** during runs where the
   changed linear-phase path improved 7-17%. That control is what makes the
   linear-phase claim credible, so it is published alongside it.

Both lessons went into the quality spec: sequential A/B is not sufficient below
~10%, and every claim needs an unchanged in-run control.

Also: I briefly ran `git checkout --` on `automix_analysis.rs` to clean up a
temporary probe and destroyed the real work in that file. Recovered it intact
from a backup copy and re-verified. Lesson: never use `git checkout --` on a
file holding uncommitted work; remove the probe surgically instead.

### Status

[OK] **Completed**

### Next Steps

- `polyphase_backend.rs` (2 rustfft sites) was left unsurveyed and is the
  obvious follow-up if this class of win is worth continuing.


## Session 31: Reuse FFT plans instead of rebuilding planners per call

**Date**: 2026-08-13
**Task**: Reuse FFT plans instead of rebuilding planners per call
**Branch**: `main`

### Summary

Answered "can the minimum-phase cepstral factorization go faster?" by measuring
instead of assuming, and the answer relocated the bottleneck entirely: it was
never the FFTs. `FftPlanner::new()` starts with an empty cache, so three call
sites recomputed a factorization + algorithm selection + twiddle tables on every
call and discarded them. 171 us cold vs 0.072 us warm at 8192 points.

### Main Changes

- New `FirFftPlans` cache in `fir_design.rs`, owned by its user.
- `FirEq` holds one as a field; resampler backends create one per construction
  and share it across `minimum_phase_prototype` and the inner factorization,
  collapsing two cold planners into one.
- Two new tests pinning bit-identical output and per-size cache correctness.
- `docs/quality.md` + quality spec updated.

### Git Commits

| Hash | Message |
|------|---------|
| `8e9b7fc` | perf: reuse FFT plans instead of rebuilding planners per call |

### Testing

- [OK] `--all-features` 515 lib + all integration suites pass
- [OK] `--no-default-features --features rubato` 534 pass; `soxr`-only 482 pass
- [OK] clippy (both feature sets, all targets) + fmt clean
- [OK] `assert_no_alloc` steady-state pass
- [OK] public API diff reviewed line by line: exactly 2 lines per baseline
- [OK] `interleaved_tap_counts...` verified to FAIL when the cache ignores its key

| Case | Change | Role |
|---|---:|---|
| `set_band` 255 taps linear/minimum | -42% / -56% | changed |
| `set_band` 511 taps linear/minimum | -47% / -40% | changed |
| `set_band` 1023 taps linear/minimum | -25% / -51% | changed |
| linear-phase resampler setup | -2% | control |

### Notes / Judgement Calls

1. **I had to retract earlier numbers.** All the setup figures I reported in the
   previous exchange (1149-4648 us, "86-94% is minimum-phase") were measured
   under `--all-features`, which enables the default `soxr` feature and never
   enters the rubato path. My instrumentation printed nothing, which is how I
   caught it. Correct rubato figures are ~3x smaller. Lesson recorded in the
   spec: state the feature set next to any resampler number.

2. **I also retracted my own prior conclusion** that this chain "cannot use a
   real transform." Measurement showed the folded cepstrum is still real (1e-16)
   and the post-`exp()` spectrum is conjugate-symmetric, so two of three
   transforms *can* be real (1.30-1.99x). I did not implement it, because the
   same profiling showed FFTs are only 28-38% of the chain while planner
   construction + `Complex::exp()` are 59-74%. Planner reuse was the better buy.

3. **Refused to publish the resampler-setup win.** Per-rep deltas were -24%,
   -20%, +9%. The mechanism is real but the host can't resolve it, so it is
   documented as not-claimed rather than averaged into a headline number.

4. **`UnwindSafe` narrowing was a real API break, not a nuisance.** I checked the
   diff direction against the spec's warning before reacting, confirmed the
   committed baseline really was `UnwindSafe`, then found `AssertUnwindSafe`
   would have silently hidden it. Asked rather than decided unilaterally; user
   chose to accept the narrowing, which also matches `SpectrumAnalyzer` /
   `FFTConvolver` already being `!UnwindSafe` for the same reason.

5. **Mid-task I ran `git checkout --` on a file with uncommitted work** (again,
   cleaning up a probe) and destroyed the automix migration. Recovered from a
   backup. Second time this session; the rule is now firm: never
   `git checkout --` a file holding uncommitted work.

### Status

[OK] **Completed**

### Next Steps

- `Complex::exp()` is 23-40% of the minimum-phase chain and untouched.
- Migrating T1/T3 of the factorization to `realfft` (1.30-1.99x on the transform
  portion) is verified feasible and now the largest remaining FFT-side item.


## Session 32: Make the pure-Rust rubato backend the default

**Date**: 2026-08-13
**Task**: Make the pure-Rust rubato backend the default
**Branch**: `main`

### Summary

Switched `default` from `soxr` to `rubato`, keeping `soxr` as opt-in. The payoff
is licensing and setup: a default build links no native library, runs no
vcpkg/pkg-config probe, and carries no LGPL-2.1 obligation, so a plain
`cargo add` now works on a machine with no libsoxr.

### Main Changes

- `default = ["http", "loudness-db", "rubato"]`; `soxr` opt-in, priority
  unchanged so `features = ["soxr"]` restores the old backend exactly.
- Third public-API matrix + snapshot + SemVer baseline for the default set.
- CI: default-set test/check/clippy, plus a no-libsoxr default build.
- Docs synced everywhere (both READMEs, NOTICE, 3 docs/, module docs,
  `compile_error!` text); 2 lessons into the quality spec.

### Git Commits

| Hash | Message |
|------|---------|
| `291bcd7` | feat!: make the pure-Rust rubato backend the default |

### Testing

- [OK] default 567 lib + all integration suites; all-features 515; rubato-only
  534; soxr-only 482
- [OK] clippy + fmt clean on all three feature sets; `cargo doc` no warnings
- [OK] public_api 3/3 matrices; the two pre-existing baselines byte-unchanged
- [OK] missing-backend `compile_error!` still fires on `--no-default-features`
- [OK] `RESAMPLER_BACKEND_NAME` probe: default -> rubato, `+soxr` -> soxr
- [OK] `Arc<Mutex<StreamingResampler>>` is Send+Sync on **both** backends

### Notes / Judgement Calls

1. **I overstated the damage, then corrected it.** I first called the `Sync` loss
   high-impact. A set difference between the two rendered baselines showed
   `StreamingResampler` is the *only* newly-`!Sync` public type — every public
   holder (`OutputRenderChain`, `PlaybackPipeline`, `DspChain`,
   `ConvolverProcessor`) was already `!Sync` under the old default. The narrowing
   does not propagate at all.

2. **I also understated it in the other direction.** My manual diff review caught
   only `Sync`. `cargo-semver-checks` named `Sync`, `UnwindSafe`, *and*
   `RefUnwindSafe`. Lesson: let the tool classify auto-trait changes. I isolated
   this change's contribution by re-running against a second baseline that
   already contained the earlier `FirEq` work.

3. **Root cause is upstream and unfixable here.** A compiler probe (not a guess)
   traced it to rubato's `Box<dyn InnerResampler<f64>>` inside `Async<f64>`.

4. **I proved the claim I put in the docs.** Rather than assert
   `Arc<Mutex<_>>` still works, I compiled it under both backends.

5. **Found a real coverage hole while doing this.** I assumed `RUBATO_ONLY`
   covered the new default; it does not — it is `--no-default-features`, so no
   `http`/`loudness-db`. And `ALL_FEATURES` enables `soxr`, which wins priority
   and hides the rubato auto traits. **That is why both existing snapshots stayed
   byte-identical across a breaking change.** Added a default matrix.

6. **Deliberately left the version alone.** User's call. All three
   `--release-type patch` gates now fail with `auto_trait_impl_removed`; that is
   the gate working. CHANGELOG says so explicitly so nobody "fixes" it by
   refreshing baselines.

7. **NOTICE needed care** — it is a licensing document, and the LGPL sentence
   inverted meaning with this change.

### Status

[OK] **Completed** (version number pending release decision)

### Next Steps

- Decide the release version; `cargo-semver-checks` says major.
- Consider an upstream rubato PR adding `+ Sync` to `Box<dyn InnerResampler>`.
- Still open from last session: `Complex::exp()` (23-40% of the minimum-phase
  chain) and migrating T1/T3 of the factorization to `realfft`.
