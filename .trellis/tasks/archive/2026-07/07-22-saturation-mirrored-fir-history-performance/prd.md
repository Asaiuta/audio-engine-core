# Saturation Mirrored FIR History Performance

## Goal

Reduce the remaining `SaturationQuality::Oversampled2x` and
`Oversampled4x` FIR cost by replacing the per-tap circular-history wrap branch
with a fixed, mirrored history window. Preserve the exact saturation transfer,
FIR coefficients, per-product accumulation order, state lifecycle, output
timing, and all existing quality behavior.

## What I Already Know

* The preceding fixed-ratio task reduced isolated 4x cost to `31.870`,
  `30.461`, `29.536`, and `30.142 ns/sample` for 64/128/256/512-frame blocks.
* `OversamplingChannelState::evaluate` still walks the circular history newest
  to oldest and branches on every tap to wrap its index.
* Both live filters are fixed odd lengths: 17 taps for 2x and 33 taps for 4x.
* A chronological contiguous window would reverse the existing floating-point
  accumulation order. Coefficient symmetry preserves the mathematical FIR but
  is not sufficient for the required bit-for-bit implementation oracle.
* Quality changes rebuild the nonlinear state from source history, while reset,
  sample-rate changes, and high-pass topology changes clear oversampling state.

## Requirements

* Keep all public saturation APIs and Direct/2x/4x behavior unchanged.
* Keep the existing transfer functions, interpolation phases, 17-/33-tap
  coefficient tables, residual-only topology, four-frame timeline, and finite
  tail unchanged.
* Store oversampling residual history in a fixed preallocated mirrored layout
  that exposes a contiguous newest-to-oldest window for the active tap count.
* Preserve the old per-product and accumulator order exactly; do not use
  coefficient symmetry to justify a reversed floating-point reduction.
* Keep reset, initialization, quality-history rebuild, transition-bank copying,
  multichannel setup, and high-pass processing correct for both tap counts.
* Keep processing allocation-free, lock-free, logging-free, panic-free, and
  bounded after setup.
* Add an independent legacy circular-ring oracle for 17- and 33-tap histories.
  Compare each evaluated output bit-for-bit over wraparound, reset, and
  reinitialization sequences rather than comparing only two callers that share
  the new state implementation.
* Retain the candidate only when compatible benchmark evidence shows a strict
  isolated 4x improvement at every measured block size and no callback-budget
  regression.

## Acceptance Criteria

* [x] Legacy circular and mirrored histories produce bit-identical FIR outputs
      for 17 and 33 taps before and after multiple wraps, reset, and initialize.
* [x] Existing dynamic-vs-fixed oversampling parity and all saturation unit,
      adapter, chunking, reset/rate, finite-tail, and no-allocation tests pass.
* [x] Isolated 4x medians are strictly lower than the compatible current-code
      baseline at 64, 128, 256, and 512 frames.
* [x] Both 512-frame active callback medians remain within +3%; p95 deadline
      utilization remains within +5%.
* [x] All 27 quick quality gates pass; saturation alias reduction remains at
      least 6 dB and fundamental loss remains no worse than 0.5 dB.
* [x] Default and Rubato library/clippy matrices, rustfmt, rustdoc, and
      `git diff --check` remain green.
* [x] Benchmark JSON and a short evidence note record the compatible baseline,
      candidate, environment, and keep/revert decision.

## Definition of Done

* Focused legacy-oracle and lifecycle coverage is committed with the state
  representation change.
* Compatible callback, quality, and output-render evidence is retained under
  this task's `research/` directory.
* Documentation is updated only if the retained candidate changes a published
  timing number materially.
* Work commits remain separate from Trellis archive/journal bookkeeping.

## Technical Approach

Use a fixed two-window history array. Maintain an index that identifies the
newest residual, write each new residual into both mirrored positions, and
evaluate a contiguous newest-to-oldest slice with coefficients in their
existing order. Keep the dynamic reference path temporarily available for
quality-transition reconstruction and parity testing.

Benchmark this portable structural change before considering explicit SIMD.
If it does not pass the strict isolated-path gate, restore the current circular
state and preserve the negative result in task evidence.

## Decision (ADR-lite)

**Context**: fixed-ratio dispatch removed dynamic phase/tap control, leaving the
branching 17-/33-tap circular dot product as the clearest portable hotspot.

**Decision**: try mirrored newest-to-oldest storage without reversing the
floating-point reduction. Use a standalone legacy ring as the compatibility
oracle and current-code benchmark output as the performance baseline.

**Consequences**: per-channel state grows by at most 33 f64 values, while each
push performs a second fixed write. The change is retained only if removing
the per-tap wrap branches outweighs that write cost on the full benchmark
matrix.

## Out of Scope

* Approximate waveshapers, coefficient changes, f32 processing, new ratios, or
  latency/tail changes.
* Architecture-specific SIMD or new portable-SIMD dependencies.
* Sparse below-threshold residual skipping.
* Resampler, decoder, Convolver, FIR EQ, or output-render policy changes.

## Technical Notes

* Implementation: `src/processor/saturation.rs`.
* Performance gate: `benches/audio_callback_chain_perf.rs`.
* Quality gate: `benches/audio_quality_measurements.rs`.
* Prior evidence: `.trellis/tasks/archive/2026-07/07-22-saturation-oversampled4x-performance/research/final-evidence.md`.
* Applicable specs: `.trellis/spec/backend/realtime-safety.md`,
  `.trellis/spec/backend/listening-nonlinear-correctness.md`, and
  `.trellis/spec/backend/quality-guidelines.md`.
