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
- `ConvolverControl::publish_at_rate`, `reclaim_retired`, and `status` are
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
  state. `StreamingDecoderBuilder::staging_buffer_bytes()` describes the
  fixed crate-owned interleaved `f64` staging payload; decoding must reject a
  packet that exceeds that capacity rather than resizing it.
- Gapless ownership is codec-aware and exclusive. `GaplessOwner::for_codec`
  enables Symphonia native trimming only for MP3 and Vorbis, the 0.6 decoders
  that consume `AudioDecoderOptions::gapless`; all other codecs retain the
  Track-level fallback. The native branch must never run the fallback delay or
  padding counters. The fallback applies delay only at true stream start and
  padding only at true stream end; a seek must not re-arm start delay. A native
  decoder may return an empty buffer while discarding reset preroll, which
  `decode_next_span` must consume internally rather than expose as `Some(&[])`.
  Symphonia seek timestamps are track timebase ticks, not guaranteed audio-frame
  indices: subtract `Track::start_ts` and apply `Track::time_base` before
  updating frame/sample accounting. Regression tests must cover the codec
  allowlist, fallback start/end trim, post-seek no-double-trim, non-sample-rate
  seek timebases, and an enforced real Ogg/Vorbis seek comparison. MP3 may not
  be claimed corpus-verified until a real LAME fixture is present.
- Recomputing coefficients on a parameter change — but do it on the control
  thread / on the snapshot swap, not per sample.

## Audio-Thread Floating-Point Initialization

`runtime::audio_thread_init()` is part of the realtime boundary because callers
invoke it from the actual callback/playback thread. Its public signature stays
infallible and idempotent on every target, but the compiled work is
architecture-specific:

- x86/x86_64 and aarch64 compile a thread-local once gate around the existing
  MXCSR/FPCR register update. The gate, register read, and register write must
  remain allocation-free, lock-free, I/O-free, panic-free, and logging-free.
- Other architectures compile the initializer to an empty body and do not
  compile its TLS flag. Per-sample correctness comes from the existing
  `flush_subnormal_sample` software fallback, which maps finite subnormal `f64`
  values to zero.
- Do not emit an unsupported-target warning from this function or a private
  helper it calls. If an application eventually needs a capability notice, it
  belongs in an explicit control/setup-side query with a real consumer, not in
  the callback initializer.
- Supported-target tests call the initializer twice and assert the hardware
  mode remains enabled. Unsupported-target cfg tests assert repeated init is a
  no-op, hardware mode reports false, and the software flush still handles a
  smallest-positive subnormal.

Wrong:

```rust
fn set_audio_thread_float_mode() {
    log::warn!("unsupported"); // formatting/dispatch on the callback
}
```

Correct:

```rust
pub fn audio_thread_init() {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
    AUDIO_THREAD_FLOAT_MODE_INITIALIZED.with(|initialized| {
        // one register update per actual audio thread
    });
    // unsupported target: empty compiled body; software sample flush remains
}
```

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

### One Validation Policy For Both Parameter Layers

`f64::clamp` returns `NaN` unchanged. A `NaN` that reaches a filter, smoother,
or coefficient does not merely produce one bad sample — it poisons that stage's
history for the rest of the stream, and no later in-range write repairs it. So
clamping alone is never sufficient validation for a control value.

The single shared policy is `lockfree_params::sanitized(value, min, max)`:
reject non-finite input, clamp the rest into the published range. It is
`pub(crate)` precisely so the standalone DSP cores use the same policy as the
atomic publishers, instead of each core re-encoding bounds.

- **Infallible setter** (`fn set_x(&mut self, v: f64)`, and every atomic
  publisher) — drop a non-finite write and keep the previous value. Silence is
  correct here: these are callback-adjacent and cannot return an error.
- **Fallible setter** (`fn set_x(...) -> Result<(), ProcessError>`, e.g.
  `Equalizer::set_band_gain`) — report the rejection with
  `ProcessError::InvalidParameter`. Do not clamp an out-of-range *index*; that
  would edit a different band than the caller asked for.
- **Published range vs core range** — the constants in `lockfree_params.rs`
  bound what a *facade user* may request. A core may still be driven outside
  them by internal machinery, and clamping in the core would silently break
  that. `PeakLimiter::set_threshold` is the worked example: the intersample-peak
  guard subtracts its additive bound from the user's ceiling, so the core
  legitimately receives a value below `LIMITER_THRESHOLD_DB_MIN`. Cores whose
  range genuinely is the published one (saturation, volume, crossfeed, dynamic
  loudness) must import the constants rather than repeat the literals.

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
