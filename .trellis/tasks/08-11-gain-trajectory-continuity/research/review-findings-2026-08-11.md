# Review Findings 2026-08-11 — Gain Trajectory Continuity

Source: loudness/convolution deep-review agent report from the 2026-08-11
six-track full-code review (session claude_a287435f). Line numbers refer to
the pre-1.0.1 tree; the 1.0.1 fixes did not touch these sites.

## D2 (medium) — Limiter attack is a hard step, not click-free smoothing

- Location: `src/processor/loudness/limiter.rs:349-351`.
- The attack branch `gain_reduction = target_gain` is an instantaneous jump
  within one sample. For a transient exceeding the ceiling by 6 dB, gain jumps
  1.0 → 0.5 on a single sample, applied to sustained program signal sitting in
  the delay line — an audible "thud/duck" edge ~10 ms (lookahead) before the
  transient.
- The lookahead buffer already exists, so a linear/raised-cosine attack ramp
  inside the window costs almost nothing.
- Docs say "Instant attack": deliberate tradeoff, but the review question
  "is gain smoothing click-free" is answered: release yes, attack no.
- Verified context: the reviewer derived the full sliding-window timing
  (output frame `m` gain decided by control window `[m, m+D-1]`, FIR group
  delay 6 frames inside `D = lookahead + 13`), found no off-by-one, and
  confirmed `delay_frames()` matches the actual ring latency exactly. The
  ceiling guarantee is mathematically closed — preserve it.

## D3 (medium-low) — Dynamic loudness -3 dB pre-gain + bypass step

- Locations: `src/processor/dynamic_loudness.rs:468` (pre_gain constant),
  `:524-531` (`can_bypass_for_zero_strength`), `:679-681`/`719-734` (apply
  paths).
- (a) Enabled with strength > 0 ⇒ constant -3 dB even above reference volume
  with zero compensation and all bands inactive.
  `test_identity_path_applies_pregain_without_touching_filters` proves this is
  intended headroom (avoids level pumping when compensation engages) — but it
  is undocumented insertion loss.
- (b) Confirmed discontinuity: when strength is set to 0, band gains decay
  smoothly; on the exact sample the last smoother snaps to 0 and every band
  is inactive, `process_validated` enters full bypass and the -3 dB pre-gain
  **vanishes instantly** — a +3 dB hard step (symmetric -3 dB when strength
  returns). EQ band gains get 50 ms smoothing; the pre-gain path has none.
  Adapter enable/disable (`process_fixed_1_to_1(enabled, ...)`) likewise has
  no crossfade.

## D4 (low-medium) — Normalization gain applied block-constant (zipper)

- Locations: `src/processor/loudness/atomic_state.rs:201-207`,
  `src/processor/loudness/normalizer.rs:305-308`.
- `process_gain(frames)` returns a single gain for the whole block; no
  per-sample ramp inside the block. Default 200 ms smoothing, 512-frame
  blocks ⇒ each block moves ~5.2% of the remaining dB gap: a 20 dB target
  change on track switch produces ~1.04 dB gain steps at block boundaries
  (~0.26 dB at 375 Hz step rate for 128-frame blocks). Audible on pure tones
  and quiet outros. Per-sample interpolation inside the block removes it.

## D5 (low) — Normalizer enable bypass replays stale limiter history

- Locations: `src/processor/loudness/normalizer.rs:78-81` (`set_enabled`
  does not reset), `:281-283` (disabled early-return).
- While disabled, the whole chain including the limiter is skipped: path
  latency instantly shrinks by `delay_frames` (~10 ms). On re-enable, the
  limiter's delay buffer still holds audio from the previous enabled epoch:
  ~10 ms of stale content plays first, then latency jumps again.
  `set_enabled` should reset the limiter (or an upper layer should crossfade;
  currently neither does).

## Related verified-good context (do not regress)

- True-peak detection is a real 49-tap Hann-windowed 4x polyphase FIR shared
  by meter and limiter (`LimiterMode::TruePeak` default).
- Monotonic max queue matches the legacy O(N·L) scan bit-exactly across
  buffer boundaries (existing differential tests).
- `assert_no_alloc` is armed (global allocator registered in test builds);
  all four fixes must stay inside its coverage.
