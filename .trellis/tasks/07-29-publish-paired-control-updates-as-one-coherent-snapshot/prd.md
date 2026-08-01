# Publish paired control updates as one coherent snapshot

## Goal

Audit finding 10: `PlaybackParameters::set_saturation_gains_db` is one semantic
control operation but performs two separately observable publications. The
audio callback can adopt the intermediate snapshot and run a block with the new
input gain against the old output gain, contradicting the facade's documented
"each update becomes visible as one complete snapshot at a later callback block
boundary" promise. Make one control operation equal one publication, and make
that rule enforced by a test rather than by convention.

## Revalidation verdict (2026-07-29)

Finding 10 is **accurate and not stale** in the current tree.

- `src/pipeline.rs:798-814` — `set_saturation_gains_db` validates both gains and
  then calls `self.saturation.set_input_gain(input)` followed by
  `self.saturation.set_output_gain(output)`.
- `src/processor/lockfree_params.rs:790-812` — each of those setters calls
  `SharedParams::update`, which takes the writer lock, patches one field, and
  calls `publish_locked`. `publish_locked` bumps the generation and publishes a
  new realtime snapshot, so each call is an independently observable state.
- `src/processor/adapters.rs:392-412` — `SaturationProcessor::sync_params`
  adopts whichever complete snapshot is current at the block boundary and
  applies both gains immediately, with no ramp. A torn pair is therefore
  audible for at least one full block.
- `src/pipeline.rs:657-666` — the `PlaybackParameters` type contract promises
  complete-snapshot visibility, so this is a contract violation, not just a
  race window.
- A survey of every publication call in the facade (`set_volume`, `set_muted`,
  `set_eq_*`, `set_limiter_*`, all `set_saturation_*`, `set_crossfeed*`,
  `set_dynamic_loudness*`, `set_noise_shaping*`) found `set_saturation_gains_db`
  to be the **only** setter that publishes twice.
- No test calls `set_saturation_gains_db` today.

One same-class defect was confirmed in the same module during revalidation:

- `src/processor/lockfree_params.rs:1296-1310` —
  `AtomicDynamicLoudnessParams::set_ref_volume_db` reads the whole snapshot with
  `self.shared.read()` **outside** the writer lock, mutates two fields, and then
  calls `self.shared.publish(snapshot)`. A concurrent `set_strength`,
  `set_enabled`, or `set_volume` landing between the read and the publish is
  silently overwritten. This is a lost update, not merely a torn read. It is the
  only read-modify-publish-outside-the-lock site in the file; every other
  multi-field publisher constructs a fresh snapshot or goes through
  `SharedParams::update`.

## What I already know

- `SharedParams::update(f)` holds the writer mutex across read, patch, and
  publish, so it is the correct primitive for a coherent multi-field patch. It
  publishes exactly once, advancing the reported generation by one.
- `AtomicSaturationParams::write(snapshot)` exists but replaces every field, so
  the facade cannot use it to change two fields without reading the current
  snapshot at the call site — which would reintroduce the same lost-update race
  as `set_ref_volume_db`.
- Multi-field coherent publishers are already the house pattern:
  `AtomicEqParams::write`, `AtomicCrossfeedParams::write`,
  `AtomicNoiseShaperParams::write`, `AtomicDynamicLoudnessParams::write`.
- Every `Atomic*Params` exposes `load_with_generation()` publicly, so a test can
  count publications exactly. `PlaybackParameters`' private `Arc` fields are
  reachable from the in-file `playback_facade_tests` module.
- `PlaybackParameters::saturation()` returns only `(enabled, drive, threshold,
  mix)`; the makeup gains have no facade reader. Widening that tuple is audit
  P3 "positional/incomplete readback" and a breaking change, so publication
  coherence is asserted at the `AtomicSaturationParams` boundary instead.
- The `Atomic*Params` family is infallible by design; this task changes
  publication coherence only, not the validation policy.

## Requirements

- One `PlaybackParameters` control operation publishes exactly one snapshot.
- `AtomicSaturationParams` gains a coherent paired makeup-gain publisher that
  patches both fields under the writer lock in a single publication, sanitizing
  each gain with the existing `SATURATION_GAIN_DB_MIN`/`_MAX` policy so a
  non-finite value still leaves the previous snapshot intact.
- `PlaybackParameters::set_saturation_gains_db` uses that publisher.
- The existing single-gain setters remain available for changing one gain, and
  their rustdoc directs callers to the paired publisher when both change.
- `AtomicDynamicLoudnessParams::set_ref_volume_db` performs its read, patch, and
  publish inside the writer lock so a concurrent update cannot be lost. Its
  unchanged-value early return is preserved.
- A regression test asserts that every value-bearing facade setter advances its
  target's generation by exactly one, so this defect class cannot silently
  return.
- No change to DSP behaviour, validation policy, callback allocation-freedom, or
  any public signature.

## Acceptance Criteria

- [x] `set_saturation_gains_db` advances the saturation generation by exactly
      one and both gains change together.
- [x] No intermediate snapshot with a mixed new-input/old-output gain pair is
      observable from a realtime subscriber.
- [x] A non-finite gain in either position still rejects with
      `InvalidParameter`, publishes nothing, and leaves both stored gains
      unchanged.
- [x] Every value-bearing `PlaybackParameters` setter is covered by a
      single-publication assertion.
- [x] A concurrent `set_ref_volume_db` and `set_strength` pair cannot lose
      either write.
- [x] Existing saturation output, transition, latency, and no-allocation tests
      pass unchanged.
- [x] `cargo fmt --all -- --check`, strict Clippy, and the full test suite pass
      on both `--all-features` and `--no-default-features --features rubato`.
- [x] CHANGELOG records the fix; the final review records adopted and rejected
      broader refactors.

## Definition of Done

- One semantic control operation is one publication, everywhere in the facade.
- `SharedParams::update` is the only way a stored snapshot is patched, so no
  read-modify-publish race remains in `lockfree_params.rs`.
- The single-publication rule is enforced by a test, not by reviewer memory.
- Existing unrelated dirty work remains untouched; no commit, push, or archive
  without explicit direction.

## Technical Approach

1. Add `AtomicSaturationParams::set_gains_db(input_gain_db, output_gain_db)`,
   sanitizing both gains and then patching both fields in one
   `SharedParams::update` closure.
2. Point `PlaybackParameters::set_saturation_gains_db` at it.
3. Rewrite `AtomicDynamicLoudnessParams::set_ref_volume_db` on top of
   `SharedParams::update`, keeping the "same dB value publishes nothing" short
   circuit inside the closure's guarded read.
4. Add a `playback_facade_tests` helper that measures generation deltas through
   the private `Arc<Atomic*Params>` fields, and apply it to every value-bearing
   setter.
5. Add a concurrency regression for the dynamic-loudness lost update.
6. Update rustdoc on the single-gain setters, the spec, and the CHANGELOG.

## Decision (ADR-lite)

**Context**: The facade promises complete-snapshot visibility, but one setter
composes two publications, and one lower-level setter composes a read and a
publish across the writer lock. Both break the same rule from different sides.

**Decision**: Treat "one control operation is one guarded publication" as the
invariant, fix both violations through `SharedParams::update`, and pin the rule
with a generation-counting test over the whole facade surface.

**Consequences**: `AtomicSaturationParams` grows one method, which is the same
shape as the existing multi-field publishers on its sibling types. No public
signature changes and no DSP behaviour changes. The systemic test makes future
paired setters fail loudly instead of silently tearing.

## Out of Scope

- Widening `PlaybackParameters::saturation()` to report the makeup gains or the
  saturation type (audit P3 positional/incomplete readback, breaking change).
- Renaming `set_input_gain`/`set_output_gain` or `PeakLimiter::set_threshold`
  to carry their dB units (audit P3 naming).
- Making the `Atomic*Params` family fallible (audit P2 §3).
- Ramping saturation makeup-gain changes across a block boundary; the stage
  applies control values immediately by design.
- Audit finding 11 and every later finding.
- Committing, pushing, or archiving anything without explicit direction.

## Technical Notes

- Primary code: `src/processor/lockfree_params.rs`, `src/pipeline.rs`.
- Contracts: `.trellis/spec/backend/realtime-safety.md`,
  `.trellis/spec/backend/dsp-state-correctness.md`,
  `.trellis/spec/backend/error-handling.md`.
- Source finding:
  `.trellis/tasks/07-28-codebase-maintainability-audit/research/01-public-api-and-control-boundaries.md`.
