# Convolver Reclamation Design

## Current failure mechanism

`ConvolverProcessor` keeps memory bounded with four local stages: active
`owned`, withdrawn `incoming`, `pending_retire`, and one `retired` ArcSwap slot.
The audio thread never overwrites a non-empty retirement slot, so it does not
become the last owner of a multi-megabyte kernel during hand-off.

Before this change, the mechanism only progressed if a control thread owned and
drained `disposal_slot()`. `OutputChainBuilder` immediately type-erased the
processor inside `DspChain` and returned no disposal handle. In the canonical
entry point, the documented consumer therefore could not exist. After the
retirement stages filled, current audio remained valid but new adoption could
wait forever.

The input side remains bounded: a kernel already withdrawn into `incoming`
cannot be dropped on audio, while newer not-yet-withdrawn publications replace
one another in the control-owned ArcSwap slot. That is a safe latest-wins
degradation, but it is currently invisible.

## Non-negotiable ownership constraint

`FFTConvolver::process_inplace` needs `&mut self`; the processor obtains it via
`Arc::get_mut`. A control-side registry cannot retain another strong `Arc` to
defer destruction, because doing so makes the kernel permanently non-unique
and therefore unprocessable. Reclamation must transfer unique ownership back
off the audio thread rather than merely add a reference.

## Comparable patterns

### 1. Single-slot RCU publication + explicit reclamation handle

The producer publishes the latest immutable/uniquely transferable object;
audio withdraws at a block boundary; retired values move through fixed slots
to a control-side consumer. This matches the existing code and requires no
new dependency. Backpressure is unavoidable when the consumer stops, so the
handle must expose it and document the current-kernel fallback.

### 2. Bounded SPSC retirement queue

A preallocated ring allows a configurable burst before backpressure. It still
cannot guarantee unlimited adoption without a consumer, adds queue indices and
capacity policy, and is unnecessary when publication already coalesces to the
latest kernel. A larger queue delays rather than fixes a missing control path.

### 3. Epoch/hazard-pointer reclamation

Deferred destruction can avoid explicit slots, but safe mutable ownership of
`FFTConvolver` and control over which thread executes destructors become much
harder. General epoch collectors may run deferred work on a participating
audio thread unless integration is tightly constrained. This is disproportionate
for one single-consumer object.

### 4. Dedicated background reclaimer thread

A worker polling the slot makes ordinary host misuse less likely. It introduces
an implicit thread, wakeup/adoption latency, shutdown/join ordering, and hidden
resources into a library that otherwise leaves scheduling to its host. It also
does not remove the need for bounded backpressure if the worker stalls.

## Recommended control handle

Use one cloneable `ConvolverControl` as the public control-plane contract:

* owns the publication slot, enabled flag, retirement slot, and atomic status;
* accepts a built `FFTConvolver` by value so publication starts uniquely owned;
* performs replacement and opportunistic retired-kernel destruction only on
  the caller/control thread; concurrent control-side callers are serialized by
  a control-only gate that is never acquired by audio;
* provides an explicit `reclaim_retired()` for polling/shutdown;
* exposes a copyable status snapshot including published, adopted,
  superseded-before-adoption, reclaimed, deferred-adoption count,
  pending-reclamation and backpressured state;
* is cloned into exactly one live audio consumer (`ConvolverProcessor`) and is
  retained by the canonical builder caller before type erasure.

The audio path keeps its existing fixed stages. On retirement-capacity
exhaustion it keeps the current kernel, parks the already-withdrawn incoming
kernel, sets backpressure once, and returns normally. Once control drains, the
next block resumes hand-off/adoption. No wait/spin loop is allowed in process.

## Publication and recovery behavior

* Before/after publication, control may drain the current retired slot.
* If audio has not withdrawn the prior publication, replacement is performed
  and the obsolete kernel is destroyed on control; count it as superseded.
* If audio has withdrawn a kernel, it remains parked until uniquely adoptable
  and retirement capacity exists; do not replace/drop it on audio.
* A publish/reclaim race can still produce temporary backpressure. Status makes
  this observable, and explicit polling or the next publish's reclaim restores
  progress.
* One control handle is single-audio-consumer. Multiple callback/render chains
  must use distinct handles, because the retirement slot assumes one writer.

## Required stress evidence

* More than 10,000 publications with audio/control schedules that cover:
  producer-faster-than-audio, audio-faster-than-reclaimer, burst coalescing,
  deliberate retirement saturation, recovery, disable/reenable and final
  latest-kernel adoption.
* Fixed maximum ownership stages and consistent status counters.
* A destructor-thread sentinel proving replaced/unpublished and retired
  kernels are destroyed on the control test thread, never inside the guarded
  audio process closure.
* `assert_no_alloc` around adoption, deferral, retirement and recovery blocks.

## Decision status

Selected and implemented: explicit `ConvolverControl` (Approach A). The
control gate preserves generation/install ordering when cloned control handles
publish concurrently, while the audio-side ownership stages remain lock-free.

## Implementation evidence

* `convolver_control_stress_remains_bounded_and_adopts_latest_generation`
  executes 10,000 publications, burst coalescing, saturation/recovery,
  disable/reenable, and final latest-generation adoption.
* `convolver_control_serializes_concurrent_publishers` verifies unique ordered
  generations across four concurrent control publishers.
* `convolver_kernels_are_destroyed_by_control_not_audio_thread` tracks four
  replaced/retired kernels and proves none is destroyed on the audio thread.
* `convolver_processor_kernel_swap_is_allocation_free_on_audio_side` covers
  adoption, retirement, saturation, recovery, and reenable under
  `assert_no_alloc`.
