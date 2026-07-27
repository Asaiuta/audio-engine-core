# Optimize Resampler Pareto Frontier

## Goal

Move the production `audio-engine-core` resampler onto a demonstrably better
quality/latency/throughput Pareto frontier than its closest upstream controls.
The work may specialize or replace Rubato/SoXR integration and may absorb
proven techniques from other engines, but benchmark wins must come from the
production path and must not be obtained by weakening an existing quality or
lifecycle contract without exposing that trade-off as a separate profile.

## What I Already Know

* The corrected 2026-07-26 comparison measured project SoXR at 12.638 / 9.294
  ns per input sample for 44.1->48 / 48->44.1 kHz, versus raw libsoxr at
  12.155 / 9.528. The forward gap is about 3.8% of project time; reverse is
  already about 2.5% faster.
* Project SoXR owns one mono native stream per channel, deinterleaves input,
  processes channels sequentially through shared scratch, copies each channel
  output, and reinterleaves it. The raw control owns one native stereo stream
  and processes interleaved buffers directly.
* The Rubato-only comparison measured the project backend at 16.496 / 11.965
  ns per input sample versus same-run raw Rubato at 12.612 / 12.750. Project
  Rubato is already faster in reverse and produces materially stronger measured
  response, but the current raw control uses a different 512-frame FFT geometry
  and does not apply the same duration/delay compensation.
* Project Rubato fixes its backend input geometry at 1024 frames. It already has
  a budget-bounded direct-output path, but all caller input is still copied into
  a FIFO before processing.
* The public callback workload uses 512-frame stereo interleaved buffers. Small
  and irregular caller chunks, constrained output, drain, reset, and fresh-state
  equivalence remain production contracts.
* The repository has unrelated dirty benchmark-coverage, playback-pipeline,
  and earlier resampler work. This task must preserve those edits and retain a
  reviewable task-only change boundary.

## Assumptions

* "Surpass" means same-machine superiority under aligned work and quality
  evidence, not a universal claim across all CPUs or a ranking across unlike
  sample formats and filter recipes.
* Existing `High` and `UltraHigh` quality floors, exact duration, phase
  semantics, deterministic reset, no steady allocation, and bounded realtime
  work are invariants.
* A separate explicitly named performance profile may trade response for speed,
  but it cannot replace or silently weaken an existing preset.
* Stereo 44.1/48 kHz music conversion is the first optimization target because
  it is the measured representative workload; generic channel/rate support must
  retain a correct fallback.

## Open Questions

* None blocking. The default decision is to preserve existing quality and add
  specialized production fast paths with generic fallbacks.

## Requirements

* Add strict raw controls that isolate wrapper cost: identical sample format,
  rate, channels, caller schedule, engine geometry, quality recipe, delay
  policy, exact-work policy, warmups, trials, and lifecycle timing boundary.
* Implement a native interleaved stereo SoXR production path and retain the
  existing per-channel fallback for unsupported channel layouts.
* Eliminate avoidable SoXR deinterleave, per-channel output copy, and
  reinterleave work on the stereo path.
* Evaluate Rubato geometry as a coupled `(input chunk, sub-chunks)` choice;
  retain only candidates that satisfy the unchanged objective quality gates.
* Add a direct-input Rubato path when the FIFO is empty and caller input already
  contains a complete backend chunk. Keep the bounded FIFO path for prefixes,
  irregular chunks, backpressure, and drain.
* Evaluate canonical 147:160 and 160:147 specialization after adapter overhead
  is isolated. A custom SIMD/polyphase/FFT route is allowed when it preserves
  the selected profile's measured response and lifecycle semantics.
* Measure steady median/p95, setup, reset, drain, buffering/impulse latency,
  passband deviation, THD+N, alias rejection, exact work, and allocation
  behavior for every retained candidate.
* Do not connect benchmark-only third-party engines to production selection.
  Production integration of a new algorithm requires normal feature/licensing,
  error handling, realtime-safety, and fallback review.
* Revert candidates that add complexity without a repeatable retained benefit.

## Acceptance Criteria

* [x] Project SoXR is statistically tied with or faster than raw libsoxr in
      both canonical directions under the strict same-recipe stereo control;
      any "faster" claim requires at least a 2% median advantage or separated
      trial distributions, not a single noisy median.
* [x] SoXR setup cost is reduced materially from the current roughly 2x raw
      stereo setup gap, with no reset/drain regression above 5% unless steady
      throughput improves enough to justify and document the trade-off.
* [x] A four-way Rubato control (`raw/project` x selected `512/1024` geometry)
      separates engine/filter cost from adapter/lifecycle cost.
* [x] Retained Rubato changes improve 44.1->48 steady median by at least 5%
      and do not regress 48->44.1, 128/256/512/1024 caller schedules, or p95 by
      more than 3% under compatible evidence.
* [x] Existing High/UltraHigh passband, THD+N, alias, latency, exact-duration,
      chunk-invariance, reset/fresh, and terminal-drain gates all pass.
* [x] Processing and drain allocate nothing, perform no I/O/logging/locking,
      and remain bounded after setup.
* [x] Benchmark reports identify production algorithm/geometry revisions and
      retain raw trial vectors and machine/build provenance.
* [x] Formatting, strict Clippy, both SoXR and Rubato feature matrices, focused
      lifecycle tests, comparison tests, and full relevant benchmark gates pass.

## Definition of Done

* Every retained optimization has before/after compatible JSON evidence.
* Rejected experiments and their measurements are recorded so they are not
  repeated later.
* Production behavior, fallback rules, and quality/lifecycle invariants are
  covered by regression tests.
* Durable routing and benchmark-comparability decisions are synchronized into
  the Trellis backend specifications and public performance documentation.
* Task-owned changes are committed separately from unrelated dirty work.

## Expansion Decisions

* Preserve future extension points for more channel layouts and additional
  canonical ratios, but implement stereo 44.1/48 kHz first.
* Include short-buffer and irregular-buffer behavior in the first iteration;
  a fast path that only works in a synthetic full-buffer benchmark is not
  acceptable.
* Treat numerical drift, output-length drift, reset mismatch, allocation, or
  unbounded drain as hard rollback conditions.

## Decision (ADR-lite)

**Context**: The latest matrix exposes both small wrapper overhead and large
algorithm/quality differences. Optimizing all rows as if they represented the
same work would encourage benchmark-specific or lower-quality changes.

**Decision**: Use a staged Pareto program. First eliminate provable adapter
overhead against strict upstream controls; then tune coupled engine geometry;
only then develop canonical-ratio DSP specialization. Preserve quality and
lifecycle semantics by default, and retain generic production fallbacks.

**Consequences**: SoXR has a high-confidence short path to parity or a narrow
win. Rubato and any custom 147:160 engine require more experiments and may
produce rejected candidates. Universal fastest-engine language remains out of
scope; claims are tied to explicit lanes and evidence.

## Technical Approach

1. Extend the comparison/control harness so raw SoXR and raw Rubato can run the
   exact production geometry and policy needed for attribution.
2. Add a stereo SoXR backend variant over one native interleaved stream and
   route two-channel production streams through it.
3. Add focused bit/output/lifecycle/no-allocation coverage and collect a full
   same-machine candidate report.
4. Refactor Rubato's input adaptation so complete contiguous chunks can bypass
   the FIFO, then benchmark unchanged geometry.
5. Sweep quality-constrained Rubato FFT geometries and retain the best stable
   canonical-rate choice.
6. Profile the remaining 147:160 / 160:147 cost. If the general FFT route is
   still behind, prototype a specialized SIMD-capable linear-phase route and
   retain it only when the full Pareto gate improves.

## Out of Scope

* Benchmark-only dispatch that differs from the public production path.
* Silent quality downgrades or comparisons across f32/f64 as strict wins.
* Claiming universal superiority from one Windows/Intel host.
* Integrating every comparison engine into production merely because its shim
  exists in the benchmark harness.

## Technical Notes

* Primary production files: `src/processor/resampler/mod.rs`,
  `src/processor/resampler/soxr_backend.rs`, and
  `src/processor/resampler/rubato_backend.rs`.
* Strict controls: `benches/resampler_comparison_support/adapters.rs` and
  `benches/audio_resampler_comparison_perf.rs`.
* Existing focused performance probe:
  `benches/audio_resampler_streaming_perf.rs`.
* Required specs: `.trellis/spec/backend/realtime-safety.md`,
  `streaming-lifecycle.md`, `quality-guidelines.md`, `error-handling.md`, and
  `performance-regression.md` where present.
* Source evidence and experiment results belong under this task's `research/`
  directory.
