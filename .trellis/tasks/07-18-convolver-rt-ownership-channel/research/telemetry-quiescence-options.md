# Convolver Telemetry And Quiescence Options

## Current race and required boundary

The disabled audio path currently derives `has_pending_publication` from
`status().pending_kernels`, which is assembled from independently updated counters. It then
writes `audio_idle`. A concurrent control publication can write `audio_idle=false` before the
audio thread commits an idle decision based on its older snapshot, so the older audio write
wins. The snapshot is explicitly eventually consistent and must not drive lifecycle state.

The AtomicPtr hand-off design gives three kinds of authoritative state:

* control-owned `published` and `retired` ownership slots;
* audio-local `owned`, `incoming`, `pending_retire`, and finish state;
* a publication generation embedded in each transferred kernel.

Telemetry counters may remain independently atomic and eventually consistent. Shutdown /
quiescence needs a separate protocol that proves audio has observed and drained every
publication through a particular generation. It must remain fixed-cost on audio and must not
read `ConvolverStatus` internally.

## Option 1 - Publication generation plus drained acknowledgement (recommended)

Use two monotonic values with distinct writers:

* control publishes the pointer, then Release-publishes `latest_published_generation`;
* after disabled audio has no `owned`, `incoming`, `pending_retire`, or locked finish kernel
  and observes no published pointer, it Release-stores the generation it has drained into
  `audio_drained_generation`.

A publication racing an older idle acknowledgement advances
`latest_published_generation`, so equality is lost rather than a newer busy state being
overwritten. The next audio boundary observes/drains the publication and advances the
acknowledgement. No cross-writer boolean is needed.

Add an authoritative control-side `ConvolverControl::is_quiescent()` check. Under the
control-only gate and after callers have stopped publishers, it requires:

* disabled control state;
* `audio_drained_generation == latest_published_generation`;
* published slot is null;
* retired slot is null.

The audio acknowledgement is written only after all local ownership/finish state is empty,
so equality covers state that the control cannot inspect directly. `ConvolverStatus` remains
an eventually consistent diagnostics snapshot; `audio_idle` can be derived from generation
equality for display, but shutdown code no longer calls `status().is_quiescent()`.

Advantages:

* Single logical writer per generation; stale writes cannot overwrite newer control state.
* No CAS loop or packed-bit protocol on audio; only fixed pointer operations plus a generation
  load/store on disabled drain.
* Reuses the existing kernel generation and has a direct proof obligation: all publications
  through generation N have left audio-local ownership.
* Authoritative quiescence checks actual ownership slots rather than inferred counter deltas.

Costs:

* Adds one acknowledgement atomic and changes the public shutdown call from snapshot-derived
  `status().is_quiescent()` to control-owned `is_quiescent()`.
* Ordering must be documented: pointer installation precedes generation publication, and the
  audio acknowledgement follows empty local/slot observation.
* Generation wrap uses wrapping arithmetic; tests and docs must state that equality is valid
  while fewer than `2^64` unacknowledged publications can exist, which is already implied by
  the current u64 counters.

## Option 2 - Packed epoch and lifecycle flags

Encode an activity epoch plus idle/backpressure/finishing flags in one `AtomicU64`. Control
publishing uses a CAS loop to increment the epoch and clear idle; audio uses a one-shot CAS to
commit its flags only if the observed epoch is unchanged.

Advantages:

* Epoch and flags form one linearizable word.
* A stale audio transition fails its CAS immediately.

Costs:

* Adds bit allocation, flag-transition tables, generation-width/wrap rules, and CAS retry
  logic on control.
* Pointer ownership still lives in separate atomics, so the packed word cannot by itself prove
  that both mailboxes are empty.
* Backpressure and finish are processor-local consequences; mirroring all of them into one
  shared word creates more states than the current quiescence question needs.

## Option 3 - Audio-owned enum plus direct slot inspection

Make an `AtomicU8` audio ownership enum (`Active`, `Finishing`, `Backpressured`, `Idle`) written
only by audio. Control checks that enum and the two AtomicPtr slots directly.

Advantages:

* Small state representation and no cross-writer field.
* Useful diagnostic vocabulary.

Costs:

* Moving a pointer from the published slot into audio-local ownership creates a cross-atomic
  hand-off window; correctness depends on carefully ordering enum and pointer operations.
* An `Idle` value does not identify which publication generation was observed, so future edits
  can reintroduce the same stale-state class.
* The proof is less explicit than a versioned acknowledgement while saving only one u64.

## Recommendation

Choose Option 1. It is a versioned acknowledgement rather than another telemetry snapshot.
It keeps the RT path bounded, avoids packed-state complexity, and makes the public lifecycle
boundary explicit: diagnostics come from `status()`, while teardown authorization comes from
`ConvolverControl::is_quiescent()` after publishers stop. Add deterministic barrier tests for
publish-before-ack, publish-during-ack, pointer-withdrawal, finish-locked tail, retirement
reclaim, and generation wrap-adjacent comparisons.
