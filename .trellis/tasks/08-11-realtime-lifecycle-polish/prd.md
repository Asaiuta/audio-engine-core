# Realtime Lifecycle Polish

2026-08-11 full-code-review follow-up, batch 4 of 8. The concurrency review
confirmed the crate's core realtime claims outright (hazard-pointer/seqlock
protocol correct under interleaving analysis, reclamation strictly
control-side, armed `assert_no_alloc`); 1.0.1 fixed the two real state-machine
defects. What remains are bounded-cost and semantic-hygiene items.

## Goal

Remove the last known deadline hazard (O(IR) in-callback reset), and clean up
three small semantic lies in the realtime plumbing so future readers cannot
build on them.

## What I Already Know

- **`REQUEST_RESET` performs O(IR) memset on the audio thread**
  (`pipeline.rs` REQUEST_RESET → `adapters/convolver.rs:528-531` →
  `convolver.rs:789-808`): partitioned engines `fill` history buffers
  proportional to IR length — tens of MB for million-frame IRs, in one
  callback. No allocation and no lock, but it contradicts the crate's own
  spread-quanta amortization philosophy and risks deadline misses on short
  buffers. Options: amortize the clear across blocks (mirror the Bresenham
  tail pattern), or hand the clear to the control side via the existing
  kernel handoff (publish-fresh-kernel semantics).
- **Dynamic-loudness telemetry pseudo-fence**
  (`lockfree_params.rs:1459-1479`): `update` Release-writes `factor` first,
  then band values; the reader's `let _ = factor.load(Acquire)` only orders
  against the *previous* update — it guarantees nothing for the current one.
  The type is documented as non-coherent per-field telemetry, so behavior is
  fine; the fence is misleading code. Either drop the fake fence or move the
  factor write last.
- **Latched `NoiseShaperProcessor` still subscribes a realtime reader it
  never uses** (`adapters.rs:1600-1615`, `1631-1637`): wastes a hazard slot
  and makes every publisher publication scan it. Skip subscription in latch
  mode.
- **`PeakLimiter::is_enabled()` is constitutionally `true`**
  (`limiter.rs:438-441`), which turns the adapter's "enabled changed ⇒ reset"
  check (`adapters.rs:1270-1272`) into "any publish while disabled ⇒ reset".
  Coincidentally covers the disable-moment state clear, but the code says
  something other than what it does. Fix the predicate or the comment plus a
  targeted test for the disable-transition reset.
- **40-bit lifecycle generation wraparound** (`pipeline.rs:236-250`):
  `(generation+1) << 24` truncates after 2^40 requests (~35 years at 1 kHz);
  `take_newer_than` uses equality and could mismatch once at wrap. Not
  reachable in practice — document with a comment, no code change.
- 1.0.1 already fixed: drain+fade error loop, post-fade silent drain, fade
  restart continuity. `consume_lifecycle_request`'s
  mark-applied-despite-reset-error remains deliberate (anti-retry) and all
  stage resets are infallible today — leave as is.

## Research References

- [`research/review-findings-2026-08-11.md`](research/review-findings-2026-08-11.md)
  — findings C, D, E, F, G, I from the realtime-safety review report with
  interleaving analyses.

## Requirements

- Reset cost: choose amortized-clear vs control-side-clear (ADR-lite here),
  implement, and add a bench/regression showing bounded per-block reset work
  for a long-IR kernel; preserve "reset ⇒ logically new stream" semantics
  and all existing lifecycle tests.
- Telemetry: remove or fix the pseudo-fence; keep the documented
  non-coherent contract; adjust the comment to match memory-order reality.
- Latched noise shaper: no subscription in latch mode; assert publisher scan
  count (or hazard-slot occupancy) in a test.
- `is_enabled` cleanup: make the adapter predicate express its real
  condition; add a disable-transition state-clear test so the accidental
  coverage becomes intentional.
- Wraparound comment on the lifecycle channel.
- Everything stays inside `assert_no_alloc` coverage; no public API change.

## Out of Scope

- Any change to the hazard-pointer snapshot protocol or its orderings (the
  review confirmed all-SeqCst is necessary and correct — do not "optimize").
- Convolver publication/reclamation protocol changes.
- New lifecycle request kinds.

## Technical Notes

- Files: `src/pipeline.rs`, `src/processor/adapters.rs`,
  `src/processor/adapters/convolver.rs`, `src/processor/convolver.rs`,
  `src/processor/lockfree_params.rs`, `src/processor/loudness/limiter.rs`.
- Specs: `realtime-safety.md` (bounded-work rule is the driver for the reset
  item), `streaming-lifecycle.md` (facade scenario contracts).
- If amortized clear is chosen: the partitioned convolver's Bresenham
  spread-quanta scheduler is the in-crate pattern; a cleared-up-to watermark
  with lazy zeroing on first reuse is the cheaper alternative.

## Implementation Plan

1. ADR-lite: reset-clear strategy; implement + bounded-cost evidence.
2. Telemetry fence cleanup + comment truth.
3. Latch-mode subscription skip + test.
4. `is_enabled`/adapter predicate cleanup + disable-transition test.
5. Wraparound comment; full matrix + `assert_no_alloc` suite.
