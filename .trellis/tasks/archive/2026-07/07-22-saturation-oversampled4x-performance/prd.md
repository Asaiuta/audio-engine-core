# Saturation Oversampled4x Performance

## Goal

Reduce the realtime CPU cost of `SaturationQuality::Oversampled4x` while
preserving the current transfer function, nonlinear-residual topology, timing,
finite tail, chunking invariance, and objective quality gates. The first target
is the isolated saturation path; the full callback chain must not regress.

## What I Already Know

* The current implementation already evaluates the 33-tap FIR once per source
  sample. The older 132-MAC-per-sample observation is no longer an accurate
  description of the live code.
* `advance_oversampled_state` still dispatches a runtime ratio and performs a
  four-iteration interpolation loop for every source sample. `evaluate` walks a
  circular history with a branch on every tap, which inhibits straightforward
  vectorization.
* Same-machine release baseline from
  `research/callback-baseline.json` (Windows MSVC, Intel Alder Lake,
  `x86_64-pc-windows-msvc`, rustc 1.93.1, seven-trial quick median):
  isolated 4x is `43.026`, `36.311`, `36.252`, and `36.942 ns/sample` at
  64/128/256/512 frames. The active no-convolver callback is `66.226`,
  `63.648`, `63.382`, and `64.800 ns/sample` at those sizes.
* The quality contract requires at least 6 dB folded-alias improvement and no
  more than 0.5 dB wanted-fundamental loss versus Direct. It also requires
  threshold continuity, exact below-threshold delayed-dry behavior, finite
  support, reset/rate isolation, chunking invariance, and no steady-state
  allocation.

## Requirements

* Keep the public saturation API and all three quality modes unchanged.
* Preserve the existing f64 transfer functions and 33-tap/17-tap coefficient
  tables; no approximate waveshaper is permitted in this task.
* Keep the 4-frame enabled timeline, residual-only filtering, high-pass path,
  quality transitions, reset, and finite-tail behavior unchanged.
* Optimize the 4x hot path using setup-time/static structure where practical:
  dispatch quality outside the per-sample loop, expose a fixed-ratio four-phase
  kernel, and make the FIR history contiguous or otherwise branch-free for the
  dot product.
* Keep processing allocation-free, lock-free, panic-free, and bounded after
  setup.
* Add focused tests for any new state representation or arithmetic kernel and
  retain the existing numerical oracles.
* Regenerate compatible callback benchmark evidence and update documented
  timing only if the measured result supports a claim.

## Acceptance Criteria

* [x] Isolated 4x median is strictly lower than the baseline at 64, 128, 256,
      and 512 frame blocks under the compatible benchmark gate.
* [x] Both 512-frame active callback medians remain within the 3% task limit;
      p95 deadline utilization does not regress by more than 5%.
* [x] All saturation quality gates pass, including alias reduction and
      fundamental delta, for Tape/Tube/Transistor coverage.
* [x] Existing saturation unit, adapter, reset, rate-change, chunking, finite
      tail, and no-allocation tests pass.
* [x] Default and `--no-default-features` library test/clippy matrices remain
      green; rustfmt and rustdoc remain clean.
* [x] A benchmark JSON and a short evidence note record the before/after
      environment and the decision to keep or reject the candidate.

## Definition of Done

* Tests added or updated for changed state/kernel behavior.
* Compatible quick callback benchmark passes with enforced work and timing
  rules.
* Quality evidence is regenerated when signal arithmetic changes.
* Relevant backend spec or task research is updated with any new invariant.
* Work is committed separately from Trellis archive/journal bookkeeping.

## Technical Approach

### Approach A: Fixed-ratio, contiguous-history kernel (recommended)

Keep the exact algorithm but specialize the 4x path at the buffer boundary.
Use a fixed four-phase interpolation kernel and a mirrored/contiguous ring so
the 33-tap dot product has no per-tap wrap branch and can be optimized by the
compiler. Compare output and quality evidence before retaining each change.

### Approach B: Explicit architecture SIMD

Add AVX2/FMA or portable SIMD dot-product kernels behind target cfgs. This may
win more on the reference CPU but increases portability, dispatch, and
maintenance risk. It is deferred unless Approach A leaves a material gap.

### Approach C: Sparse residual fast path

Track whether nonlinear residual history is inactive and skip waveshaper/FIR
work for provably below-threshold regions. This is potentially useful for
quiet material but requires careful state advancement and an oracle for
crossing/transition cases. It is deferred until the fixed-ratio kernel is
measured.

## Decision (ADR-lite)

**Context**: the live code already contains the residual-only FIR optimization,
so the remaining obvious costs are dynamic ratio/phase control and circular
FIR traversal. Approximate `tanh` or coefficient changes would undermine the
existing Hi-Fi quality contract.

**Decision**: implement the fixed-ratio portion of Approach A, with
arithmetic-preserving behavior as the compatibility goal. The dynamic and
fixed kernels are compared bit-for-bit before retaining the specialization.
Mirrored-history, architecture-specific SIMD, and sparse-residual variants
remain deferred because the fixed dispatch already passes the strict callback
and isolated-path gates.

**Consequences**: the public API and quality-mode semantics remain unchanged,
and the current state representation is retained. The fixed kernels reduce
dispatch overhead without changing operation order or numerical output; later
history/SIMD changes still require the same parity oracle and compatible
benchmark evidence.

## Out of Scope

* Approximate `tanh`, new waveshaper curves, or f32 processing.
* New FIR coefficients, a different oversampling ratio, or a new latency/tail
  contract.
* Public parameter/API changes.
* Output-render policy changes, Convolver/FIR EQ work, and Rubato routing.
* Architecture-specific unsafe SIMD unless the portable structural pass is
  measured and found insufficient.

## Technical Notes

* Implementation: `src/processor/saturation.rs`.
* Callback evidence: `benches/audio_callback_chain_perf.rs`.
* Objective quality: `benches/audio_quality_measurements.rs`.
* Contracts: `.trellis/spec/backend/realtime-safety.md`,
  `.trellis/spec/backend/listening-nonlinear-correctness.md`, and
  `.trellis/spec/backend/quality-guidelines.md`.
* Prior design analysis: `.trellis/tasks/archive/2026-07/07-18-dsp-lifecycle-performance-correctness/research/saturation-timing-and-cpu.md`.
