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
- Decode-side allocation: the decoder is not on the audio callback. Even so,
  `decode_next_into` reuses its `sample_buf` and is allocation-free in steady
  state.
- Recomputing coefficients on a parameter change — but do it on the control
  thread / on the snapshot swap, not per sample.

## How Parameters Cross The Boundary

Tunable parameters are pushed into the callback without locks via the atomic
snapshot types in `src/processor/lockfree_params.rs` (`AtomicEqParams`,
`AtomicVolumeParams`, `AtomicPeakLimiterParams`, etc.). The callback reads a
generation-stamped snapshot once per buffer (~7 ns; see
`audio_lockfree_params_perf`). New tunables must reuse this mechanism rather
than introducing a lock or an allocation.

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
