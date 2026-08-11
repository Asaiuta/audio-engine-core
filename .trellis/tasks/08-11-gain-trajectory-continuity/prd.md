# Gain Trajectory Continuity for Stop, Bypass, and Normalization

2026-08-11 full-code-review follow-up, batch 1 of 8. Recommended as the 1.0.2
candidate: every item is audible, has a low-cost standard fix, and shares one
theme — **a gain path must be continuous unless a discontinuity is explicitly
requested**.

## Goal

Remove the four remaining audible gain discontinuities the review confirmed in
the loudness stack. The 1.0.1 lifecycle fixes made the *playback facade's*
stop/drain gain continuous; this task applies the same standard inside the
loudness processors themselves.

## What I Already Know

- **Limiter attack is a single-sample hard step** (`src/processor/loudness/
  limiter.rs:349-351`, post-1.0.1 line numbers may shift): the attack branch
  assigns `gain_reduction = target_gain` in one sample. A transient 6 dB over
  ceiling snaps the gain 1.0 → 0.5 on one sample, multiplying sustained
  program material ~10 ms before the transient (lookahead) — an audible thump.
  Documented as "instant attack", so this is a deliberate tradeoff being
  revisited, not a regression. The ceiling guarantee itself was verified
  correct by per-frame derivation and must not regress.
- **Dynamic loudness inserts a constant -3 dB pre-gain whenever enabled with
  strength > 0** (`src/processor/dynamic_loudness.rs:468`), even at reference
  volume with all bands inactive. The headroom itself is a defensible
  anti-pumping design (`test_identity_path_applies_pregain_without_touching_
  filters` proves intent) but is undocumented, and worse: when the last band
  smoother settles to zero, `can_bypass_for_zero_strength` (`:524-531`) flips
  into full bypass and the -3 dB **vanishes in one sample** (+3 dB step;
  symmetric -3 dB step on re-engage). Band gains get 50 ms smoothing; the
  pre-gain gets none. Adapter-level enable/disable takes the same step.
- **Normalizer gain is block-constant** (`src/processor/loudness/
  atomic_state.rs:201-207`, `normalizer.rs:305-308`): `process_gain(frames)`
  returns one gain per block. With the default 200 ms smoothing at 512-frame
  blocks, a 20 dB track-change target moves ~5.2% of the remaining dB gap per
  block ⇒ ~1.04 dB steps at block boundaries (zipper), audible on tones and
  quiet outros.
- **`LoudnessNormalizer::set_enabled` bypasses the limiter delay line without
  resetting it** (`normalizer.rs:78-81`, `281-283`): disabling removes
  ~10 ms of path latency instantly; re-enabling first plays ~10 ms of stale
  audio from the previous enable epoch, then jumps latency again.
- 1.0.1 established the facade-side precedent (pipeline `fade_base_gain`):
  ramps continue from the current gain, a completed stop pins silence.

## Research References

- [`research/review-findings-2026-08-11.md`](research/review-findings-2026-08-11.md)
  — verbatim confirmed findings D2, D3, D4, D5 from the loudness/convolution
  review report, with derivations and failure scenarios.

## Requirements

- Limiter: implement an attack ramp inside the existing lookahead window
  (linear or raised-cosine over the lookahead), preserving the mathematical
  ceiling guarantee (`.min(target_gain)` semantics and the monotonic max
  queue). The existing per-frame timing/ceiling tests and the
  `true_peak_limiter` quality gates must keep passing; add a gain-slope test
  proving no single-sample gain step larger than a derived bound.
- Dynamic loudness: route the -3 dB pre-gain through the same smoothing
  machinery as band gains (or an equivalent dedicated ramp) for
  strength→0/0→strength, enable/disable, and full-bypass entry/exit, so no
  path takes a >0.5 dB per-sample step. Document the insertion loss in the
  public rustdoc of the enabling controls.
- Normalizer: interpolate the smoothed gain across each block (per-sample
  linear within the block is sufficient; target the same endpoint the current
  code reaches so long-term trajectories are unchanged). Bound the per-sample
  step and add a regression test with a 20 dB track-change scenario measuring
  the max inter-sample gain delta.
- Normalizer enable/disable: on `set_enabled(false→true)`, reset the internal
  limiter (delay line + queue) so stale audio cannot replay; document the
  latency implications of bypass. Consider (and decide explicitly, ADR-lite)
  whether disable should drain or hard-switch.
- All changes stay allocation-free on the processing path
  (`realtime-safety.md`), preserve bit-exactness when the feature is disabled,
  and keep every existing quality gate green.

## Out of Scope

- Any change to the 1.0.1 facade lifecycle gain machinery (already fixed).
- New public API surface beyond documentation (patch-compatible only).
- Loudness measurement (meter/R128) behavior.
- The Streaming-mode AGC design questions (tracked in
  `08-11-loudness-aux-hardening`).

## Technical Notes

- Primary files: `src/processor/loudness/limiter.rs`,
  `src/processor/dynamic_loudness.rs`, `src/processor/loudness/normalizer.rs`,
  `src/processor/loudness/atomic_state.rs`.
- The limiter's release path already has correct contractive smoothing; only
  attack needs the ramp. Reuse the lookahead buffer — no new state size.
- `dsp-state-correctness.md` requires: transition completion allocation-free,
  irregular-chunk equivalence tests for any new smoothing.
- Quality gates affected: `true_peak_limiter` (must stay -1.00 dBTP on the
  stress signal), `parameter-change continuity` benches.

## Implementation Plan

1. Limiter attack ramp + slope regression test + gate re-run.
2. Dynamic-loudness pre-gain smoothing (all four entry/exit paths) + step
   tests.
3. Normalizer block-interior interpolation + zipper regression test.
4. Normalizer enable→reset + stale-audio regression test; document bypass
   latency semantics.
5. Full matrix: both feature sets, `assert_no_alloc` coverage, quality bench
   quick run with `--enforce`.
