# Finding 10 revalidation and refactor review

## Current evidence (2026-07-29)

- `src/pipeline.rs:798-814` (pre-change) `set_saturation_gains_db` validated
  both gains and then called `self.saturation.set_input_gain(input)` followed by
  `self.saturation.set_output_gain(output)`.
- `src/processor/lockfree_params.rs:790-812` (pre-change) each setter used
  `SharedParams::update`, which takes the writer lock, patches one field, and
  calls `publish_locked`. `publish_locked` bumps the generation and installs a
  new realtime snapshot, so each call is an independently observable state.
- `src/processor/adapters.rs:392-412` `SaturationProcessor::sync_params` adopts
  whichever complete snapshot is current at the block boundary and applies both
  gains immediately with no ramp, so a torn pair is audible for a full block.
- `src/pipeline.rs:657-666` the `PlaybackParameters` type contract promises
  complete-snapshot visibility, making this a contract violation.
- A survey of all 18 publication call sites in the facade found
  `set_saturation_gains_db` to be the only setter publishing twice.
- No test called `set_saturation_gains_db` before this task.
- The finding is accurate and not stale. `src/pipeline.rs` and
  `src/processor/lockfree_params.rs` carried unrelated dirty facade work that
  was preserved.

One same-class defect was confirmed in the same module:

- `src/processor/lockfree_params.rs:1296-1310` (pre-change)
  `AtomicDynamicLoudnessParams::set_ref_volume_db` called `self.shared.read()`
  **outside** the writer lock, mutated two fields on the copy, and then called
  `self.shared.publish(snapshot)`. A concurrent `set_strength`, `set_enabled`,
  or `set_volume` landing between the read and the publish was silently
  overwritten. A file-wide search confirmed it was the only
  read-modify-publish-outside-the-lock site; every other multi-field publisher
  builds a fresh snapshot or goes through `SharedParams::update`.

## Adopted refactors

1. `AtomicSaturationParams::set_gains_db(input, output)` sanitizes both gains
   with the existing `SATURATION_GAIN_DB_MIN`/`_MAX` policy and patches both
   fields in one `SharedParams::update` closure — one guarded publication.
2. `PlaybackParameters::set_saturation_gains_db` delegates to it. No public
   signature changed.
3. New `SharedParams::update_if`: a guarded read-modify-publish whose closure
   may decline to publish. This is the primitive that was missing and whose
   absence caused `set_ref_volume_db` to hand-roll an unguarded sequence.
4. `set_ref_volume_db` is rebuilt on `update_if`, keeping its
   "same dB publishes nothing" short circuit but evaluating it inside the lock.
   The `powf`/clamp conversion stays outside the closure so the guarded section
   is a plain field assignment.
5. The single-gain setters keep their role for one-field changes and their
   rustdoc now names `set_gains_db` as the paired path, so a caller does not
   reconstruct the torn sequence by hand.
6. `every_facade_setter_publishes_exactly_one_coherent_snapshot` asserts a
   generation delta of exactly one for all 19 value-bearing facade setters. The
   defect class is now enforced by a test rather than by review memory.
7. `dsp-state-correctness.md` gained a "One control operation is one guarded
   publication" contract, six matrix rows, three required-test bullets, and
   wrong/correct examples.

## Rejected broader refactors

- **Do not expose a generic `patch(|snapshot| ...)` on the `Atomic*Params`
  types.** It would make every future multi-field operation coherent for free,
  but it also lets a caller store a non-finite value directly, destroying the
  family's central sanitization invariant.
- **Do not route the paired write through `AtomicSaturationParams::write`.** It
  replaces every field, so the facade would have to read the current snapshot at
  the call site — reintroducing exactly the `set_ref_volume_db` lost update.
- **Do not widen `PlaybackParameters::saturation()`** to report the makeup gains
  or the saturation type. That is audit P3 positional/incomplete readback and a
  breaking tuple change; publication coherence is asserted at the
  `AtomicSaturationParams` boundary instead, which is where publication happens.
- **Do not rename `set_input_gain`/`set_output_gain`** to carry their dB units.
  Audit P3 naming; renaming two setters while leaving `PeakLimiter::set_threshold`
  and siblings unchanged would trade one inconsistency for another.
- **Do not ramp saturation makeup-gain changes.** The stage applies control
  values immediately by design; adding a ramp is a DSP behaviour change, not a
  publication-coherence fix.
- **Do not make the `Atomic*Params` family fallible** (audit P2 §3), and do not
  change any validation policy in this task.

## Known remaining gaps (not fixed here)

- `PlaybackParameters::saturation()` still omits the makeup gains, the
  saturation type, and the quality setting, and its rustdoc calls the latest
  publication "applied". Audit P3.
- Audit findings 1-8 changed public APIs without CHANGELOG entries; findings 9
  and 10 now have them. The earlier omissions remain a separate cleanup.

## Final validation evidence

Each new test was verified to **fail against the pre-fix source**, by
temporarily reverting each fix and re-running:

- Reverting `set_ref_volume_db` to its unguarded read-modify-publish failed
  `reference_volume_writes_cannot_lose_a_concurrent_strength_update` on 3 of 3
  runs (`strength regressed from 0.00025 to 0`).
- Reverting `set_saturation_gains_db` to two publisher calls failed both
  `every_facade_setter_publishes_exactly_one_coherent_snapshot`
  ("set_saturation_gains_db must publish exactly one coherent snapshot") and
  `paired_saturation_gains_reach_a_realtime_reader_as_one_snapshot`
  ("one paired gain update must be one publication").
- Both files were restored from pre-revert copies and the suite returned green.

The concurrency regression cannot produce a false failure: `update_if` holds the
writer lock across read, patch, and publish, so the published sequence is
totally ordered by that mutex and each snapshot carries the strength as of its
own critical section. A strictly increasing strength writer therefore makes the
published strength non-decreasing by construction.

Quality gates:

- `cargo fmt --all -- --check` passed after one rustfmt pass.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo clippy --all-targets --no-default-features --features rubato --
  -D warnings` passed.
- `cargo test --all-features` passed 435 library, 20 benchmark-support, 25
  resampler-support, 3 Windows deployment, and 6 doctests; the native-shim
  prerequisite test was the single expected ignore.
- `cargo test --no-default-features --features rubato` passed 468 library, 20
  benchmark-support, 25 resampler-support, 3 Windows deployment, and 6 doctests;
  the same single expected ignore.
- The pre-existing `test_dynamic_loudness_ref_volume_db_skips_unchanged_publish`
  still passes, confirming the unchanged-value short circuit survived the
  rewrite.
- No benchmark binary was executed, so this task makes no timing, regression,
  device, driver, DAC, or end-to-end latency claim.
