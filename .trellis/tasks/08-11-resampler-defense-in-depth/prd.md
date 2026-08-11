# Resampler Defense-in-Depth

2026-08-11 full-code-review follow-up, batch 5 of 8. The resampler was the
only subsystem where the review found **zero triggerable correctness defects
in shipped paths** — half-band polyphase, spectral alias folding, cepstral
minimum phase, and cursor arithmetic all verified against independent
derivation. Every item below converts an implicit invariant into an explicit
assertion, contract, or document.

## Goal

Make the resampler's currently-implicit safety and behavior assumptions
explicit, so dependency upgrades or future refactors fail loudly instead of
silently.

## What I Already Know

- **AVX2 kernel preconditions are `debug_assert_eq!` only**
  (`halfband_backend.rs:183-219`,
  `contiguous_polyphase_backend.rs:136-186`): in release, `_mm256_loadu_pd`
  memory safety rests entirely on call-site discipline (all four current
  call sites are constructively equal-length — verified). A hard `assert!`
  or `len().min()` costs one comparison per block and deletes the latent UB
  surface.
- **`drain_into_interleaved` infers flush completion from a partial fill**
  (`mod.rs:1288-1300`), while `drain_into_mono` (`mod.rs:1169-1259`) uses an
  explicit zero-return confirmation round. Both backends were verified safe
  today (rubato emits partial only at `emitted == expected_total`; libsoxr's
  flush front-loads its FIFO), but the interleaved inference relies on
  undocumented backend behavior. Unify on zero-return confirmation.
- **SoXR latency reporting is hardcoded zero**
  (`soxr_backend.rs:159-165`): relies on libsoxr's "duration-aligned, no
  leading delay after drain" behavior rather than querying `soxr_delay()`.
  The behavior is pinned by
  `native_drain_returns_a_duration_aligned_impulse_sequence`, so a changed
  libsoxr build fails a test rather than shipping wrong timing — but the
  reliance deserves a documented decision (keep + comment, or query and
  reconcile).
- **libsoxr's C-heap allocation is invisible to `assert_no_alloc`**:
  `streaming_process_and_finish_do_not_allocate_after_setup` hooks the Rust
  allocator only; libsoxr's internal FIFO reallocs on first process or on a
  larger chunk (bounded after the 16384-frame cap warms). The "process does
  not allocate" guarantee strictly holds only for the pure-Rust backend —
  document the per-backend strength difference where the guarantee is
  advertised.
- **SoXR High tier is single-precision internally** (`soxr_backend.rs:
  33-36`): Low/Standard/High map to ≤20-bit recipes processed in f32 despite
  f64 I/O; UltraHigh (Bits28) is double. Meets each recipe's spec, but sits
  oddly against "f64 Hi-Fi" phrasing. Decide: add `SOXR_DOUBLE_PRECISION`
  to High, or document the tier precision table.
- **`resample_parallel` nonlinear-phase output length is undocumented**
  (`mod.rs:443-579`): Minimum/Maximum output = duration + latency + tail
  (unlike Linear's exact duration), and `converted_output_frames`
  under-reserves by that extension (one extra Vec growth, no error).
- **Rubato sinc fallback carries float-ratio drift** (`rubato_backend.rs:
  819-829`): pathological ratios (reduced component > 1024 rejected; the
  surviving sinc route) accumulate sub-sample phase in f64; the rational
  drain budget clamps total duration to ±1 frame. Note as accepted bound.
- **`phase_peak_latency_frames` is a scalar approximation**
  (`polyphase_backend.rs:318-330`): kernel-peak position stands in for a
  frequency-dependent group delay; adequate for gapless trimming, documented
  caveat needed for sample-exact A/B alignment use.
- **Upstream watch items** (dependency `soxr 0.6.0`, not exercised by this
  crate): `params.rs:238` `coef_size_kbytes()` returns the wrong field
  (copy-paste); `raw.rs:16` `unsafe impl Sync for SoxrPtr` with no internal
  synchronization (safe here because all mutation is `&mut self` and
  instances are single-threaded). Check both on every soxr crate upgrade.

## Research References

- [`research/review-findings-2026-08-11.md`](research/review-findings-2026-08-11.md)
  — the resampler review's findings and verification notes.

## Requirements

- AVX2: promote the equal-length preconditions to release-mode checks (hard
  assert or min-clamp; pick one crate-wide) with a comment naming the UB
  they preclude; bit-equality tests must stay green.
- Drain: convert the interleaved path to explicit zero-return terminal
  confirmation; add a test proving no tail frame is dropped for both
  backends across irregular output capacities.
- SoXR latency + precision: record both decisions ADR-lite in this PRD when
  taken; implement whichever changes fall out (comment-only is acceptable
  for latency; precision may change High-tier output and then needs a
  quality-bench re-baseline with a new algorithm identifier per
  `streaming-lifecycle.md`).
- Document: per-backend allocation-guarantee strength;
  `resample_parallel` nonlinear output-length contract (and fix the
  reservation); sinc-route duration bound; latency-scalar caveat.
- Add the two upstream watch items to a dependency-upgrade checklist note
  (CONTRIBUTING or a comment at the dependency declaration).

## Out of Scope

- Any routing, kernel, or quality-tier redesign (`streaming-lifecycle.md`
  pins routing changes behind benchmark evidence and new identifiers).
- New SIMD paths.

## Technical Notes

- Files: `src/processor/resampler/{mod,soxr_backend,rubato_backend,
  halfband_backend,polyphase_backend,contiguous_polyphase_backend}.rs`.
- Spec: `streaming-lifecycle.md` "Feature-Selected Resampler Channel
  Architecture" scenario — its test list is the regression floor.
- If High-tier double precision is adopted: THD+N and speed both move;
  budget a full matrix re-measure and baseline identifier bump.

## Implementation Plan

1. AVX2 release-mode precondition hardening.
2. Drain termination unification + tail-loss test.
3. Latency + precision ADR decisions, implementation/doc fallout.
4. Documentation batch + upstream watchlist.
5. Both-backend matrices, quality quick benches if output changed.
