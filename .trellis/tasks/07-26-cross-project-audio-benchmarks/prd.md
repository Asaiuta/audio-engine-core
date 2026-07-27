# Cross-Project Audio Benchmarks

## Goal

Add reproducible cross-project sample-rate-conversion comparisons covering the
full representative project matrix agreed with the user. Every named project
must end in either a measured result or a concrete, reviewable determination
that its public implementation cannot provide a technically comparable lane.

## What I Already Know

* The current benchmark suite compares candidate reports with historical
  baselines and compares the crate's default SoXR configuration with its
  Rubato-only configuration.
* SoXR and Rubato are already mature upstream projects, but current probes run
  through `audio-engine-core`; they do not provide raw-upstream controls.
* Phase 1 now runs the project backend, raw libsoxr, raw Rubato, and
  libsamplerate. Those results validate the harness but are not comprehensive
  cross-project coverage.
* FFmpeg `libswresample`, SpeexDSP, r8brain-free-src, zita-resampler, WebRTC,
  WDL, and libresample do not yet have measured rows in the current reports.
* The existing benchmark-coverage worktree changes belong to the separate
  `07-25-complete-audio-engine-benchmark-coverage` task and must remain
  separable at commit time.

## Assumptions (Temporary)

* Raw libsoxr and raw Rubato controls are required before claiming that custom
  routing or the crate wrapper outperforms upstream behavior.
* External native adapters must be optional so the normal crate build and CI do
  not require every comparison library.
* Performance will be interpreted as a quality/latency/throughput trade-off,
  not as a single `ns/sample` leaderboard.

## Open Questions

* None.

## Requirements

* The required representative matrix consists of the active
  `audio-engine-core` backend, raw libsoxr, raw Rubato, libsamplerate, FFmpeg
  libswresample, SpeexDSP, r8brain-free-src, zita-resampler, WebRTC, WDL, and
  libresample.
* FFmpeg libswresample, SpeexDSP, r8brain-free-src, and zita-resampler are
  presumed technically comparable and require measured rows for both canonical
  rate directions unless implementation research proves otherwise.
* WebRTC, WDL, and libresample require source/API evaluation. A project that
  exposes a reusable streaming sample-rate converter capable of the canonical
  stereo rate pairs must be measured; age, inconvenient build plumbing, or a
  missing prebuilt package is not sufficient reason to exclude it.
* Each matrix entry must end in exactly one terminal coverage state:
  `measured`, `not-comparable`, or `infeasible-with-evidence`. `skipped`,
  `unavailable`, `deferred`, and an adapter placeholder are non-terminal and do
  not satisfy completion.
* Reuse the existing resampler workload definitions, case identities, report
  metadata, warmup, trial, validation, and raw-sample conventions.
* Compare identical input, channel count, rate ratio, chunk schedule, sample
  format, and drain semantics.
* Record adapter/library identity and version, compiler/build mode, CPU
  conditions, latency, produced-frame count, and quality evidence.
* Keep external comparison dependencies behind explicit benchmark-only build
  controls.
* Load libsamplerate only from an explicit `--libsamplerate <path>` argument or
  `AUDIO_BENCH_LIBSAMPLERATE_PATH`; do not silently search `PATH` or system
  directories.
* Record the canonical library path, upstream version string, binary SHA-256,
  file size, and sample-format lane in every libsamplerate report.
* A missing/unloadable/incomplete libsamplerate ABI produces an explicit
  unavailable result. `--require-engine libsamplerate` converts that status to
  a non-zero benchmark failure.
* Store any locally acquired comparison binary under an ignored benchmark
  cache such as `target/benchmark-deps/`; never commit the DLL.
* Do not change production backend selection or audio behavior.
* Do not mix unrelated benchmark-coverage WIP into this task's commits.
* Do not use a subprocess/file-I/O timing result as a substitute for native
  streaming engine throughput.

## Acceptance Criteria

* [x] The final coverage table contains every required representative project
      and no entry remains `skipped`, `unavailable`, `deferred`, or unspecified.
* [x] Every technically comparable project has valid 44.1-to-48 kHz and
      48-to-44.1 kHz streaming measurements on the same host.
* [x] Every non-measured project has persisted source/API/build evidence proving
      `not-comparable` or `infeasible-with-evidence`; lack of an installed
      binary alone does not qualify.
* [x] All compared cases verify exact input consumption, bounded native progress,
      finite output, documented latency semantics, and complete drain output.
* [x] Reports reject incompatible quality, latency, feature, fixture, format,
      or build identities rather than silently comparing them.
* [x] Performance output includes steady-state throughput plus setup/reset/drain
      costs and objective quality/latency measurements.
* [x] Normal default and Rubato-only builds remain green without external
      benchmark dependencies installed.
* [x] Documentation states what can and cannot be concluded, calls the existing
      libsamplerate report phase 1, and does not make universal completion claims
      while the matrix has non-terminal entries.
* [x] Formal evidence runs load version-pinned external binaries through
      explicit paths and record upstream version, binary/source identity, build
      provenance, and sample-format lane.
* [x] Missing required engines remain visible in JSON/text output, requested
      JSON is written, and the benchmark then exits non-zero.

## Definition of Done

* Tests added or updated for adapters, report compatibility, and work evidence.
* Formatting, Clippy, default/all-feature tests, and relevant benchmarks pass.
* A same-machine report is saved with revision and dirty-state metadata.
* Benchmark documentation and task research record methodology and limitations.
* Rollback is removal/disablement of optional benchmark adapters; production
  audio code remains unaffected.

## Out of Scope (Explicit)

* Claiming a universal industry ranking from one Windows machine.
* Device/driver/DAC round-trip latency.
* Full-player comparisons against mpv or GStreamer; this task compares reusable
  sample-rate-conversion engines, not media-player pipelines.
* Changing decoder, DSP, resampler, or callback production behavior.

## Technical Notes

* Existing report:
  `.trellis/tasks/07-25-complete-audio-engine-benchmark-coverage/research/final-coverage-and-results.md`.
* Existing probes: `benches/audio_resampler_streaming_perf.rs` and
  `benches/audio_resampler_matrix_perf.rs`.
* Benchmark support utilities live under `benches/support/` and
  `tests/benchmark_support.rs`.
* `Cargo.toml` currently exposes optional `soxr` and `rubato` production
  features; benchmark-only third-party integration must not alter their
  precedence or defaults.

## Research References

* [`research/resampler-comparator-strategy.md`](research/resampler-comparator-strategy.md)
  — recommends a benchmark-owned adapter harness, raw upstream controls, and
  runtime-loaded independent native libraries with explicit availability
  enforcement.

## Feasible Approaches

### A. Complete Representative Matrix (Selected)

Keep the phase-1 harness and add every technically comparable representative
engine. Close a matrix row only with measurements or persisted technical
evidence that the public project does not expose a comparable engine.

### B. Stop at the Native P0 Matrix

Require raw controls, libsamplerate, FFmpeg libswresample, and SpeexDSP, while
leaving r8brain, zita, WebRTC, WDL, and libresample for later. This is no longer
acceptable because it would repeat the scope mismatch the user identified.

### C. Phase-1 Harness Only

Retain only project/raw controls plus libsamplerate. This is the implemented
phase-1 state and is useful evidence, but it is not task completion.

## Decision (ADR-lite)

**Context**: Cross-project evidence needs an independent implementation, but
adding several native ABIs before validating the workload/report contract would
make failures difficult to attribute.

**Decision**: Use Approach A. Preserve the validated phase-1 contract, expand it
to every named representative project, and require a terminal coverage state
for every row before closing the task.

**Consequences**: The task remains open longer and needs multiple native/C++ ABI
adapters and reproducible provisioning records. In return, completion has one
unambiguous meaning and cannot be inferred from a smaller MVP checklist.

**Provisioning decision**: The benchmark consumes a caller-provided fixed
libsamplerate DLL through an explicit path or environment variable. The
repository and ordinary CI do not install or vendor it; formal evidence uses a
pinned binary in an ignored benchmark cache and requires the engine explicitly.

## Technical Approach

* Add a self-contained `audio_resampler_comparison_perf` custom-main probe and
  benchmark-owned adapter module instead of changing production resampler APIs.
* Reuse common environment, JSON, trial-distribution, report-integrity, and
  baseline utilities from `benches/support` without folding this task into the
  active coverage WIP.
* Implement adapters for the selected public `StreamingResampler`, direct
  interleaved libsoxr, direct Rubato FFT, and explicitly loaded external native
  comparators. Small benchmark-only C/C++ shims are allowed when a project has
  no stable C ABI; the shim source, compiler command, upstream revision, and
  resulting binary hash become evidence.
* Separate `f64` and `f32` sample-format lanes and prohibit strict cross-format
  regression gates. Cross-engine output is a quality/latency/throughput Pareto
  report; historical gates remain engine-specific.
* Validate exact input consumption, rate-aligned output, finite samples,
  complete drain, measured/reported latency, passband, alias/stopband, and
  THD+N before accepting timing evidence.
* Keep setup, steady process, reset, and drain timing separate.

## Implementation Plan

1. Treat the existing project/raw/libsamplerate results as phase 1 and preserve
   their validated adapter/report contract.
2. Research and persist API, lifecycle, quality, licensing, and reproducible
   provisioning evidence for FFmpeg, SpeexDSP, r8brain, zita, WebRTC, WDL, and
   libresample.
3. Add and validate FFmpeg libswresample and SpeexDSP adapters, then collect
   same-host results.
4. Add and validate r8brain-free-src and zita-resampler adapters, then collect
   same-host results.
5. Implement each comparable WebRTC/WDL/libresample lane; otherwise record the
   exact terminal non-comparability evidence.
6. Run both Rust feature matrices and all external required engines, regenerate
   JSON/results/docs, verify the coverage table has no non-terminal states, and
   only then prepare task-only commits.
