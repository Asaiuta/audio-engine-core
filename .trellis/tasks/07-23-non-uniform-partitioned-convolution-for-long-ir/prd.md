# Time-distributed partitioned convolution tail for long IR

> Working title kept from task creation ("non-uniform partitioned
> convolution"); the chosen approach is time-distributed work-spreading over
> the existing uniform 1024 partitions — see Decision.

## Goal

Reduce the worst-case per-callback cost (p99/max deadline utilization) of
long-IR convolution at small callback buffers. With a 65536-tap 6-channel IR
and 64-frame callbacks, the current uniform 1024-frame partitioned engine
reaches p99 58-75% and max 94-96% of the 1.333 ms deadline, leaving almost no
scheduler-jitter margin on Windows.

## What I already know

* The burst source is structural: `PartitionedConvolver` performs the entire
  tail spectral accumulation (all `tail_partitions` MAC passes) plus one
  inverse real FFT per channel inside `prepare_tail_output_block`, and one
  forward real FFT per channel in `commit_input_block` — all on the single
  callback that crosses a 1024-frame partition boundary. At 64-frame
  callbacks, 1 in 16 callbacks carries the full burst; the other 15 do almost
  no tail work.
* 07-22 RealFFT + flattened-layout rewrite already cut steady-state cost
  43-57% at 32768-65536 taps; the partition sweep selected 1024 (512 breaches
  the deadline outright, 2048 has worse max).
* Head path (`OverlapSaveConvolver`, first 1024 taps) is separate and already
  per-callback smooth; the head/tail split constants are public
  (`PARTITIONED_CONVOLUTION_IR_THRESHOLD` = 4096,
  `PARTITIONED_CONVOLUTION_PARTITION_SIZE` = 1024).
* Hard spec constraints (`.trellis/spec/backend/realtime-safety.md`,
  `quality-guidelines.md`): zero steady-state allocation after warmup
  (`assert_no_alloc`), no hot-path lock/IO/log/panic, direct-convolution
  oracle correctness, reset/finish/tail exactness, chunking invariance,
  changing routing constants requires fresh convolver + FIR EQ benchmark
  sweeps.
* Archived option C (non-uniform 256/1024/4096) was named the "likely
  long-term throughput/latency compromise" but was deferred; note that larger
  tail partitions alone make the single-callback burst *worse*, not better —
  they only help total CPU. The p99/max problem needs work-spreading
  regardless.

## Assumptions (temporary)

* The 64-frame 65536-tap 6-channel case remains the design target; 128-512
  frame cases must not regress beyond gates.
* A background/worker thread is likely incompatible with the crate's
  RT-safety spec unless proven otherwise; deterministic in-callback
  scheduling is preferred.

## Decision (ADR-lite)

**Context**: The 64-frame worst-case burst comes from doing all tail spectral
accumulation plus both FFTs on the single boundary-crossing callback.
Non-uniform partitions alone enlarge the burst; a background thread conflicts
with the crate's lock-free hot-path spec.

**Decision**: Keep the uniform 1024-frame partition layout and spread the tail
accumulation passes deterministically across the callbacks within each
partition period (user-selected option A, 2026-07-23). No threads, no locks,
no change to routing constants.

**Consequences**: Total CPU is unchanged; only the distribution improves.
Scheduling constraint discovered during code inspection: the newest history
slot is committed at the period's end, so its accumulation pass and the
inverse FFT must stay on the boundary callback; only the older
`tail_partitions - 1` passes can be spread. The boundary burst shrinks from
"all passes + 2 FFTs" to "1 pass + 2 FFTs". This forces oldest-first
  accumulation sequence walks the older IR partitions first and adds the
  newest partition at the boundary, so output is tolerance-equivalent (f64
  reassociation, ~1e-15 relative), not bit-identical to the current engine; the
direct-convolution oracle contract is unchanged.

## Open Questions

(none — all resolved 2026-07-23)

## Requirements

* Spread the tail spectral-accumulation passes of `PartitionedConvolver`
  deterministically across the callbacks within each 1024-frame partition
  period; the newest-slot pass, forward FFT (commit), and inverse FFT remain
  on the boundary callback.
* Preserve zero steady-state allocation, no locks/IO/log/panic on the hot
  path; no threads introduced.
* Preserve direct-convolution oracle correctness (tolerance-based),
  reset/finish/tail semantics, chunking invariance, and mono/stereo/6ch
  coverage. Output is tolerance-equivalent to the current engine
  (oldest-first f64 reassociation), not bit-identical; the tolerance bound
  must be measured and recorded.
* No change to `PARTITIONED_CONVOLUTION_IR_THRESHOLD` (4096) or
  `PARTITIONED_CONVOLUTION_PARTITION_SIZE` (1024).
* Add an opt-in CPU-affinity-pinned isolated probe mode to
  `audio_convolver_perf` so the worst-case (max) callback gate becomes an
  enforceable hard gate on this machine instead of noise-limited evidence.
* No regression beyond existing gates for short/medium IRs, FIR EQ apply, or
  the full callback chain.

## Acceptance Criteria

* [x] 65536-tap 6ch 64-frame case: callback max utilization <= 50% and
      p99 <= 40% of the 1.333 ms deadline in the pinned probe mode
      (candidate: max 21.203%, p99 16.740%; see
      `research/convolver-spread-direct-cursor-pinned-final-quick.json`).
* [x] Steady-state `process_into`/`process_inplace` medians stay within the
      10% regression gate vs a same-machine pre-change baseline for all IR
      sizes and channel counts (largest paired median regression +5.203%).
* [x] Max abs output delta vs the current engine on the oracle corpus is
      measured, recorded, and within the direct-convolution oracle tolerance
      (8.327e-17 vs 1e-8).
* [x] `assert_no_alloc` steady-state proof passes, including first use on a
      new OS thread.
* [x] Convolver oracle/reset/finish/tail/irregular-chunk tests pass; new
      tests cover the spreading state machine across 64/128/256/512-frame and
      irregular chunk sequences (including chunk sizes that do not divide the
      partition period).
* [x] `audio_convolver_perf`, `audio_fir_eq_perf`, `audio_callback_chain_perf`
      quick runs pass `--enforce`; the pinned probe mode produces versioned
      JSON evidence.
* [x] The conditional negative-result path is not applicable: the 50%/40%
      targets were reached; load-contaminated probe samples are retained as
      diagnostics in `research/final-evidence.md`.

## Definition of Done

* [x] Tests added for the scheduling state machine and pinned probe mode.
* [x] fmt / strict clippy on both feature matrices / both test matrices green.
* [x] Evidence JSON + research notes persisted under `research/`.
* [x] docs/quality.md convolver rows refreshed.
* [x] Trellis specs updated with the frame-based work-spreading and pinned-probe
  contracts.

## Technical Approach

1. Record fresh same-machine baselines (`audio_convolver_perf` burst
   distribution, FIR EQ, callback chain).
2. Restructure `prepare_tail_output_block` into an incremental accumulator:
   after each boundary commit, the older `tail_partitions - 1` passes are
   consumed in per-callback quanta (quota derived from frames advanced, so
   irregular chunk sizes stay correct); the boundary callback finishes with
   newest-slot pass + inverse FFT + forward commit.
3. Accumulation runs older partitions first into the persistent per-channel
   scratch spectrum, with the newest partition added at the boundary; the state
   machine tracks passes-done vs frames-elapsed and must
   complete by construction before each boundary (prove with a debug
   assertion + tests, not runtime fallback work).
4. Add the pinned-affinity isolated probe mode (Windows
   `SetThreadAffinityMask` + elevated priority, opt-in flag) to
   `audio_convolver_perf`, gate max/p99 only in that mode.
5. Full verification sweep + evidence protocol.

## Out of Scope (explicit)

* Non-uniform partition layouts (kept as a possible follow-up if total CPU
  ever becomes the bottleneck; this task fixes distribution, not total cost).
* Background/worker-thread tail computation.
* 32-frame benchmark coverage and any latency-mode changes.
* Changing the SoXR/rubato resampler paths.
* GPU or SIMD-intrinsic convolution kernels.
* Changing the public `FFTConvolver` API surface or routing constants.

## Technical Notes

* Core file: `src/processor/convolver.rs` (`PartitionedConvolver`,
  `prepare_tail_output_block`, `commit_input_block`,
  `advance_partition_block`).
* Prior evidence: `.trellis/tasks/archive/2026-07/07-22-long-ir-convolver-performance/`
  (`research/convolver-realfft-evidence.md`, `research/convolution-options.md`).
* Benches: `benches/audio_convolver_perf.rs` (callback-burst distribution
  cases), `benches/audio_fir_eq_perf.rs`, `benches/audio_callback_chain_perf.rs`.
