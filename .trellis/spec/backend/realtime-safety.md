# Realtime Safety

> The single most important invariant in this crate. Read this before touching
> any code that runs inside an audio callback or the DSP processing chain.

---

## The Hot Path

The "hot path" (a.k.a. realtime / RT path) is any code that executes once per
audio callback while audio is playing. In this crate that means:

- `DspChain::process` (`src/processor/dsp_chain.rs`)
- Every `StreamingProcessor::process` / `finish` adapter
  (`src/processor/adapters.rs`)
- The per-sample / per-buffer inner loops of the individual processors
  (`eq`, `crossfeed`, `saturation`, `convolver`, `dynamic_loudness`,
  `loudness/limiter`, `dsp` volume/noise-shaping, `resampler` streaming feed).
- Atomic parameter reads via `lockfree_params` snapshots.

The `process_checked` / `finish_checked` drivers are hot-path code too. See
`streaming-lifecycle.md` for their progress, backpressure, and terminal-state
contracts.

A real audio callback has a hard deadline (e.g. ~10.7 ms for a 512-frame buffer
at 48 kHz). Missing it produces an audible glitch, so the hot path must have
bounded, predictable cost.

## Hard Prohibitions In The Hot Path

The following are **forbidden** inside the hot path:

- **Heap allocation / deallocation** — no `Vec::push` that can grow, no
  `Box::new`, no `String`, no collection resize, no `clone()` that allocates.
  Buffers and ring buffers are sized once during setup (see the limiter's
  `MonotonicMaxQueue`, which allocates its `Box<[..]>` in `new`, never in
  `push`).
- **Locks** — no `Mutex`, `RwLock`, or `parking_lot` guard. Parameter updates
  cross the thread boundary through the lock-free atomic snapshots in
  `lockfree_params`, not through locks.
- **Logging** — no `log::*` macros. `dsp_chain.rs` and `adapters.rs` contain
  zero `log::` calls by design. See `logging-guidelines.md`.
- **File I/O** — no reads, writes, or `loudness-db`/SQLite access. See
  `database-guidelines.md`.
- **Network I/O** — no HTTP; network only happens in the decoder open/fetch
  path, never during processing.
- **Unbounded work** — no loop whose iteration count is not bounded by the
  buffer size or a fixed, preallocated structure. No work that scales with
  total stream length inside a single callback.
- **Panics** — no `unwrap()`, `expect()`, or `panic!` in hot-path code; a panic
  across the callback boundary is undefined/aborting. `dsp_chain.rs` and
  `adapters.rs` currently contain zero of these. Native consumed/produced counts
  must be bounds-checked before they are used for slicing so malformed backend
  progress becomes a typed error rather than a callback panic. See
  `error-handling.md`.

## What Is Allowed

- Allocation, locking, logging, and I/O during **setup/configuration** before
  the processor enters the realtime path (construction, `set_*`, coefficient
  (re)design on parameter change).
- `ConvolverControl::publish`, `reclaim_retired`, and `status` are
  control/offline operations. Publication/reclamation may allocate or take the
  control-only serialization gate; `ConvolverProcessor::process` and `finish`
  never acquire that gate and only perform fixed atomic/ownership-stage work.
  Dynamic kernel ownership crosses through one published and one retired
  `AtomicPtr` slot. The control side creates and destroys `Box` values; audio
  only performs a bounded exchange/CAS and moves unique local ownership.
- `ArcSwap` remains a control-side convenience for immutable parameter reads.
  Callback adapters register a `RealtimeSnapshotReader<T>` during setup and
  copy `Copy` snapshots through its preallocated hazard slot. Registration may
  allocate and lock; callback reads only perform bounded atomic loads/stores.
  Replaced `Box<T>` storage is reclaimed by a later control-side publication,
  never by the reader that copied it. Dropping the reader/processor is also a
  non-realtime teardown operation because its final `Arc` may deallocate.
- `ArcSwap` is forbidden for dynamic Convolver kernel ownership: its first-use
  debt node, writer traversal, and last-`Arc` destruction do not satisfy the
  hard realtime bound.
- Decode-side allocation: the decoder is not on the audio callback. Even so,
  `decode_next_into` reuses its `sample_buf` and is allocation-free in steady
  state.
- Recomputing coefficients on a parameter change — but do it on the control
  thread / on the snapshot swap, not per sample.

## How Parameters Cross The Boundary

Tunable parameters are pushed into the callback without locks via the atomic
snapshot types in `src/processor/lockfree_params.rs` (`AtomicEqParams`,
`AtomicVolumeParams`, `AtomicPeakLimiterParams`, etc.). Each callback adapter
calls `subscribe_realtime` during setup and then
`load_realtime_if_changed_since` once per buffer. The measured hazard read is
about 13 ns on the recorded Windows/x86_64 environment and materially faster
than rebuilding a split-atomic snapshot; see `audio_lockfree_params_perf`.
Control/reporting code may retain the `ArcSwap` `load` APIs. New callback
tunables must use the realtime reader rather than acquiring an ArcSwap guard.

## Verifying

`assert_no_alloc` (a dev-dependency) is the tool for asserting no steady-state
allocation on a processing path. New hot-path code should be covered by a
no-allocation test after setup. Fixed-stage migrations additionally require
irregular frame-chunk equivalence and reset-isolation coverage so callback block
size cannot silently change signal content or leak a prior stream's history.
Variable-I/O processors additionally pre-size every deinterleave/interleave and
native output scratch buffer for their documented maximum step. Their tests must
cover both ordinary process and finish/drain because a grow-on-finish path is
still an audio-thread allocation defect.

Realtime snapshot tests must publish concurrently while a reader performs at
least thousands of copies inside `assert_no_alloc`. Run the reader on a newly
created OS thread so Arc/TLS initialization is not hidden by control-thread
prewarming, and assert every observed snapshot is one complete generation.

Dynamic heavy-kernel publication additionally requires a destructor-thread
probe and a no-allocation assertion around adoption, retirement, backpressure,
recovery, and terminal finish. A control snapshot must expose enough monotonic
counters to distinguish a publication waiting for the next block from a
retirement slot that the control side has stopped consuming. Run the first
audio-side boundary on a newly created OS thread so TLS or lazy initialization
cannot be hidden by control-thread setup. Authoritative teardown checks must
version their audio-drained acknowledgement and recheck both ownership slots
after reading that acknowledgement; an eventually-consistent telemetry
snapshot is not a lifecycle barrier.
