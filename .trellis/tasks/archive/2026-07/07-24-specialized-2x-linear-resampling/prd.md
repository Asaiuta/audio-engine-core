# Specialized 2x Linear Resampling Performance

## Goal

Reduce the realtime CPU cost of the common 48 kHz to 96 kHz linear-phase
resampling path by exploiting its exact 2:1 ratio, while preserving the
streaming, quality, and realtime-safety guarantees of the existing resampler.

## What I Already Know

* The retained Rubato linear path uses a 1024-frame FFT engine with two FFT
  subchunks. Its adapter already uses fixed-capacity FIFO rings and a direct
  caller-output path for duration-stable integer ratios such as 48->96 kHz.
* The completed 07-24 task deliberately retained two FFT subchunks globally:
  the four-subchunk candidate regressed the measured 44.1->48 kHz case.
* A setup-designed polyphase backend now exists, but it currently implements
  only `PhaseResponse::Minimum` and `PhaseResponse::Maximum`; it is not a
  dedicated linear-phase 2x performance route.
* The 48->96 kHz result is still bounded by generic FFT processing after the
  FIFO and direct-output improvements. A dedicated half-band or equivalent
  two-phase linear FIR route could avoid that generality, but must earn its
  inclusion with comparable quality and lifecycle evidence.
* Existing resampler contracts require arbitrary caller chunking, exact
  duration accounting/drain behavior, reset isolation, typed errors, and no
  allocations in `process` or `finish` after setup.

## Scope Decision

* Route every exact 2:1 **upsampling** pair that requests
  `PhaseResponse::Linear` and `ResampleQuality::High` through the dedicated
  engine. The initial performance acceptance target remains 48->96 kHz.
* Low, Standard, and UltraHigh quality presets retain their established
  routing, as do downsampling, non-2x ratios, and Minimum/Maximum phase.
* Existing Rubato FFT/sinc routes remain the fallback for all other ratios and
  for cases where the dedicated route cannot meet the selected quality level.
* Performance decisions will use same-machine, work-validity-gated benchmarks
  at 128/256/512-frame caller blocks rather than a single noisy run.

## Requirements (Evolving)

* Add a measured specialized route for the selected exact 2x linear-phase
  scope, without weakening existing Rubato behavior for other ratios.
* Select the route only for `Linear + High + to_rate == 2 * from_rate`; retain
  the current FFT/sinc and nonlinear-polyphase routing predicates otherwise.
* Preserve interleaved f64 streaming semantics, arbitrary input granularity,
  duration-aligned drain, reset, and allocation-free steady-state processing.
* Keep phase semantics explicit: this task must not change the existing
  nonlinear-phase polyphase contract.
* Record before/after performance and quality evidence with distinct algorithm
  identifiers.

## Acceptance Criteria (Evolving)

* [x] The selected 2x route is demonstrably faster than the retained FFT route
      for 48->96 kHz at 128/256/512 frames in a comparable benchmark matrix,
      without invalid work.
* [x] Quality gates, direct/staged duration parity, arbitrary chunking, reset,
      finish, interleaving, and no-allocation tests pass for the new route.
* [x] Existing non-2x Rubato and nonlinear-phase behavior remains unchanged.
* [x] Documentation names the route, its supported geometry, and its measured
      limits.

## Definition of Done

* Tests and focused benchmarks cover the selected routing and failure cases.
* Both supported feature matrices pass fmt, strict Clippy, and tests.
* Benchmark and quality evidence is persisted under `research/`.
* Relevant Trellis specifications and public documentation reflect durable
  behavior changes.

## Out of Scope (Explicit)

* General `CHUNK_IN` auto-tuning across all ratios.
* Generalizing direct output to non-integer or duration-unstable ratios.
* Replacing SoXR or pursuing SoXR-identical numerical stopband floors.
* Runtime sample-rate-ratio transitions without setup-time allocation.

## Technical Notes

* Likely implementation files: `src/processor/resampler/rubato_backend.rs`,
  `src/processor/resampler/polyphase_backend.rs`,
  `src/processor/resampler/mod.rs`, and
  `benches/audio_resampler_streaming_perf.rs`.
* Prior evidence and lifecycle regressions are recorded in
  `../07-24-optimize-rubato-nonlinear-phase/research/phase-and-optimization.md`.
* Relevant specs: `realtime-safety.md`, `streaming-lifecycle.md`, and the
  resampler scenario in `quality-guidelines.md`.

## Research References

* [`research/two-x-linear-route-options.md`](research/two-x-linear-route-options.md)
  — source-backed comparison of dedicated half-band, generic polyphase, and
  FFT-retuning routes.
* [`research/halfband2x-final-evidence.md`](research/halfband2x-final-evidence.md)
  — retained design, candidate evolution, benchmark comparison, quality, and
  feature-matrix verification.

## Research Notes

**Recommended approach: dedicated symmetric half-band engine.** Add a new
setup-designed engine behind the existing interleaved `MonoBackend`; route
Linear + High + exact 2:1 upsampling to it. It should reuse the existing
fixed-capacity rings, direct-output conditions, delay skipping, duration
accounting, and drain/reset machinery. Every other ratio, quality, and phase
continues to use the established route.

The generic nonlinear polyphase engine is a useful filter-design reference but
not the desired performance implementation: its generic per-output
phase/history work does not exploit half-band sparsity. Earlier FFT subchunk
experiments also rule out treating FFT retuning as this task's primary route.
