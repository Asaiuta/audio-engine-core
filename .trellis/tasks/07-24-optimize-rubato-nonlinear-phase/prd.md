# Optimize Rubato Resampling and Implement Nonlinear Phase Response

## Goal

Improve the pure-Rust Rubato resampler for common realtime ratios and make
`PhaseResponse::Minimum` and `PhaseResponse::Maximum` real filter behaviors.
Linear phase keeps the existing Rubato FFT/sinc route. Nonlinear phase uses a
precomputed causal polyphase FIR bank whose prototype is converted with real
cepstrum spectral factorization, so phase selection changes the filter kernel,
not only the reported latency or impulse position.

## What I Already Know

* Rubato 4 is the current pure-Rust backend. Common ratios use `Fft<f64>`;
  UltraHigh and pathological ratios use `Async<f64>` sinc.
* Before this task, the Rubato adapter accepted `PhaseResponse` but ignored it
  because both built-in Rubato engines are linear phase.
* Existing `FirEq` code has a cepstral minimum-phase generator, but it applies
  an EQ-specific tail window and is not a resampler/polyphase implementation.
* The streaming contract requires arbitrary input granularity, duration-aligned
  output, reset isolation, typed progress validation, and no allocation in
  `process`/`finish` after setup.
* The worktree contains unrelated uncommitted long-IR convolution changes;
  this task must not modify or stage them.

## Requirements

* Keep the current Rubato linear-phase behavior bitwise/stability compatible for
  `PhaseResponse::Linear`.
* Implement true nonlinear phase for the Rubato feature:
  * `Minimum` uses a causal minimum-phase prototype generated from the same
    low-pass magnitude target by real cepstrum spectral factorization.
  * `Maximum` uses the corresponding maximum-phase kernel, derived by reversing
    the minimum-phase prototype while preserving its magnitude response.
* Use a rational polyphase FIR path for nonlinear phase with setup-time kernel
  generation and preallocated streaming state.
* Preserve f64 interleaved processing and support the common audio-rate pairs
  covered by the existing resampler tests. Unsupported/pathological geometry
  must return a typed initialization error, never silently fall back to linear
  phase.
* Improve common-ratio throughput without adding callback allocations:
  * evaluate ratio-specific FFT sub-chunk selection;
  * remove avoidable FIFO shifting/copying where the implementation can retain
    bounded work and exact chunking semantics;
  * keep any optimization only when same-machine benchmark evidence proves it.
* Make phase capability explicit in docs and errors. Nonlinear requests must
  never be accepted and ignored.

## Acceptance Criteria

* [x] `PhaseResponse::Minimum` and `Maximum` produce different phase/group-delay
  distributions from Linear for an impulse, while preserving the designed
  magnitude response within documented tolerance.
* [x] Minimum-phase energy centroid precedes Linear and Maximum follows Linear;
  tests reject a pure sample shift masquerading as nonlinear phase.
* [x] Arbitrary input chunking, drain duration, finish terminal behavior, reset,
  channel interleaving, and pathological-rate rejection are covered for the
  nonlinear backend.
* [x] `process` and `finish` remain allocation-free after setup under
  `assert_no_alloc`.
* [x] Existing 27 quality gates and linear-phase Rubato tests remain green.
* [x] The focused benchmark reports the ratio-specific optimization result for
  44.1->48 and 48->96 at 128/256/512 frame blocks, with algorithm labels that
  prevent incompatible baselines from being compared.
* [x] `cargo fmt --all -- --check`, Rubato/default tests, and clippy with
  `-D warnings` pass.
* [x] Documentation states the actual phase support and any intentional
  limits of the nonlinear polyphase path.

## Definition of Done

* Tests cover phase behavior, quality/magnitude, continuity, reset, finish,
  geometry, and no-allocation behavior.
* Focused performance and quality evidence is recorded in this task's
  `research/` directory.
* No unrelated long-IR files are included in the eventual commit.
* Specs/docs are updated if a durable resampler contract is discovered.

## Technical Approach

1. Extract or add a setup-only shared real-cepstrum minimum-phase helper based
   on the proven `FirEq` implementation, without changing existing EQ output.
2. Design a normalized low-pass prototype for the reduced rational ratio and
   convert it to minimum phase. Generate per-phase coefficient slices for a
   bounded polyphase bank. Maximum phase is the reversed minimum-phase kernel.
3. Add a preallocated interleaved polyphase streaming engine behind the existing
   `MonoBackend` adapter. Keep output duration accounting explicit and report
   the actual causal latency for maximum phase if required by the lifecycle
   contract.
4. Add ratio-specific Rubato FFT tuning and benchmark before/after; retain only
   changes that improve the target matrix without weakening quality.
5. Update phase/error documentation and run the full quality gate.

## Decision (ADR-lite)

**Context**: Rubato's built-in FFT and sinc engines are linear phase only, while
the public API already exposes minimum and maximum phase. A latency offset or
all-pass post-filter would not produce the requested filter phase response.

**Decision**: Keep Rubato for the existing linear path and implement a separate
setup-designed, real-cepstrum polyphase FIR path for nonlinear requests. Use
ratio-specific FFT tuning as a separate measured optimization.

**Consequences**: This adds a bounded DSP kernel and setup memory for nonlinear
phase, but gives honest phase semantics without a native dependency. The path
needs explicit limits for extreme rate ratios and separate quality evidence.

## Out of Scope

* Removing SoXR in this task.
* Replacing the existing linear Rubato algorithm without benchmark evidence.
* Making arbitrary runtime sample-rate-ratio transitions allocation-free.
* Claiming SoXR-identical numerical stopband floors without matching evidence.

## Technical Notes

* Primary implementation files: `src/processor/resampler/mod.rs`,
  `src/processor/resampler/rubato_backend.rs`, and a new phase/polyphase
  backend module if needed.
* Existing minimum-phase reference: `src/processor/fir_eq.rs`.
* Relevant specs: `.trellis/spec/backend/realtime-safety.md`,
  `.trellis/spec/backend/streaming-lifecycle.md`, and the resampler scenario in
  `.trellis/spec/backend/quality-guidelines.md`.
* Existing performance/quality evidence is under the archived 07-21 and 07-22
  Rubato tasks; new incompatible reports must use new algorithm labels.
