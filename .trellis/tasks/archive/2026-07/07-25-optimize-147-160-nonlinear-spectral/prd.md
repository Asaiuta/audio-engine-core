# Optimize 147:160 Nonlinear Phase Spectral Resampling

## Goal

Reduce the pure-Rust 44.1↔48 kHz nonlinear-phase (`Minimum`/`Maximum`)
streaming cost from ~191 ns/input sample (High/Minimum, spectral engine)
toward soxr's 10.4 ns. The 48→96 case is already 15.5 ns; 147:160 is slow
because `nin = down·s` forces huge FFT blocks and the alias fold sums
`down = 160` terms per output bin.

## What I already know

* The spectral engine (task 07-25 nonlinear) is exact and retained; its cost
  model scales badly for large `down` (fold = Σ over `down` alias terms).
* Kernel design and latency/finish formulas are shared with the retired
  polyphase oracle; parity regression (<1e-9) must keep passing.
* Prefix-budget direct output applies; lifecycle/realtime contracts fixed.

## Assumptions (temporary)

* A structural change (e.g. two-stage decomposition, smaller fold, or
  time-domain polyphase with contiguous SIMD-friendly loops for large-down
  ratios) can close most of the gap without quality loss.

## Requirements

* New contiguous time-domain polyphase engine for nonlinear phases with
  reduced `up > 16`: planar per-channel history with `copy_within` shift,
  contiguous taps_per_phase dot products (4-accumulator, coefficient slices
  shared across channels). Exact single-kernel semantics; latency and
  finish-extension formulas unchanged.
* Routing: nonlinear + reduced `up <= 16` stays on the spectral engine
  (1:2 keeps 15.5 ns); `up > 16` routes to the new engine; both keep
  `MAX_REDUCED_RATE = 1024`. Linear paths untouched.
* Parity: FFT/polyphase-vs-oracle max error < 1e-9 regressions extended to
  the new engine; timing-metadata equality asserted.
* Realtime safety (setup-only allocation, bounded per-call work) and
  lifecycle contracts (emitted, prefix budget, drain, reset) preserved.
* Quality-gate acceptance; kernel bits unchanged, so listening gates should
  pass as-is (archive one run as evidence).

## Acceptance Criteria

* [x] Fresh same-revision baseline + candidate matrix evidence persisted
      under `research/` with a bumped compatible algorithm identity.
* [x] 44.1→48 High/Minimum improves ≥3x (target ≤ ~50 ns, expect ~40);
      48→96 nonlinear and all Linear cases regress ≤5%.
* [x] Parity <1e-9, timing metadata, adapter bitwise-chunking, no-alloc,
      drain, reset, and routing unit tests pass.
* [x] 27 quick quality gates, strict clippy, fmt, both feature configs pass.

## Decision (ADR-lite)

**Context**: Probe shows 89% of the 147:160 spectral cost is the exact alias
fold (memory-bandwidth-bound over a 754 KB spectrum table); SoA/gather only
reaches ~265 ns, f32/pruning breaks the 1e-9 parity gate.

**Decision**: Candidate C — contiguous time-domain polyphase engine for
large-`up` nonlinear ratios; spectral engine retained for small `up`.
Two-stage spectral cascade (est. 35–45 ns) rejected: it replaces the single
designed kernel and would require a new oracle, composed latency/finish, and
full quality re-evidence. soxr-style interpolated coefficients rejected as
incompatible with the parity contract.

**Consequences**: Two nonlinear engines with a routing threshold to document
and test. Kernel bits unchanged keeps quality evidence simple. Revert if the
retention gate fails.

## Out of Scope

* Linear-phase routing changes; public API/preset changes.

## Technical Notes

* Files: `src/processor/resampler/spectral_backend.rs`, routing in
  `rubato_backend.rs`.
* Evidence: matrix bench nonlinear cases.

## Research References

* [`research/147-160-nonlinear-speedup.md`](research/147-160-nonlinear-speedup.md)
  — fold cost profile, candidate comparison, routing rule, rejected paths.
