# Long IR convolver performance

## Goal

Improve the long-impulse-response convolution path for room/reverb-sized IRs
without changing zero algorithmic latency, convolution output, reset/finish
semantics, or callback safety. The first implementation target is the
frequency-domain cost of 8192 taps and longer IRs; short and medium IRs must
remain compatible with their existing overlap-save behavior.

## What I already know

* `src/processor/convolver.rs` routes IRs above 4096 frames to a 1024-frame
  head/tail uniform partitioned engine.
* The partitioned tail performs one full complex forward and inverse FFT per
  channel at partition boundaries and accumulates every tail partition as a
  full complex spectrum.
* Current direct quick evidence on this machine is 8192 taps at 45.693
  ns/sample (`process_into`, stereo) and 39.212 ns/sample (`process_inplace`,
  stereo); six-channel results are 48.598 and 39.446 ns/sample.
* The existing direct convolver bench covers only 256, 2048, and 8192 taps,
  2/6 channels, and a 2048-frame process workload. It does not measure long-IR
  callback bursts or 4097/16384+ boundaries.
* The callback chain benchmark covers only a 256-tap convolver, so it cannot
  expose the periodic tail FFT work of long IRs.
* `realfft` is already present in the resolved dependency graph through other
  components, while the convolver currently uses full `rustfft` complex
  spectra.

## Requirements (evolving)

* Add reproducible long-IR benchmark coverage for 4097, 8192, 16384, 32768,
  and 65536 taps, 2 and 6 channels, and 64/128/256/512 callback blocks.
* Report both steady-state throughput and per-callback distribution (median,
  p95, p99, and maximum) so partition-boundary FFT bursts are visible.
* Evaluate partition sizes 512, 1024, and 2048 before selecting a routing
  policy. Keep short/medium overlap-save cases in the same report.
* Implement an allocation-free hot path. No callback allocation, locks,
  logging, I/O, panics, or unbounded work.
* Preserve exact public behavior: zero algorithmic latency, duration/tail,
  reset isolation, irregular block handling, in-place/out-of-place parity,
  and direct-convolution oracle agreement.
* If a real-FFT or spectrum-layout optimization is selected, validate output
  quality and numerical error against the current complex implementation.
* After implementation, rerun `audio_convolver_perf`, `audio_fir_eq_perf`,
  `audio_callback_chain_perf`, and the relevant convolution correctness and
  no-allocation tests.

## Acceptance Criteria (evolving)

* [x] Benchmark report contains all required IR/channel/block cases and a
      versioned same-machine baseline.
* [x] The chosen long-IR path improves the 8192-tap steady-state median by at
      least 20% or documents why a lower gain is the best safe trade-off.
* [x] No short/medium FIR apply case regresses by more than the project gate;
      callback 512-frame active-chain median stays within the task acceptance
      limit.
* [x] Long-IR callback p99/max burst cost is reported for 64/128/256/512
      frames. The canonical report stays below deadline; a repeat shows that
      raw max is scheduler-sensitive on Windows and records that limitation.
* [x] Existing convolution, lifecycle, reset, finish, irregular-chunk, and
      no-allocation tests pass.
* [x] Quality parity is demonstrated against the pre-change complex path and
      direct-convolution oracle for mono and stereo at minimum.

## Definition of Done

* Tests and benchmark coverage updated.
* `cargo fmt`, clippy, type-check, relevant test matrices, and all required
  performance probes are green.
* Benchmark evidence and the selected partition/routing rationale are written
  under this task's `research/` directory.
* Any new realtime or convolution contract is reflected in `.trellis/spec/`.
* Changes are committed separately from unrelated worktree files.

## Technical Approach

1. Establish the expanded throughput and callback-burst baseline.
2. Prototype the lowest-risk spectrum/layout optimization first: use a
   real-valued FFT or an equivalent half-spectrum representation while keeping
   the existing public engine contract.
3. Compare fixed partition sizes and, if necessary, a non-uniform head/tail
   partition schedule against the baseline.
4. Select the smallest design that improves long IRs without violating the
   callback and numerical gates.

## Decision (ADR-lite)

**Context**: The current uniform 1024 partition is a compromise. Larger
partitions reduce tail MAC work but increase head FFT cost for small callbacks;
smaller partitions reduce burst size but increase total frequency-domain work.
Full complex spectra also do unnecessary work for real audio.

**Decision**: Measure the partition/workload matrix first, then prioritize a
half-spectrum real-FFT implementation. Keep partition-size changes and
non-uniform partitioning as measured alternatives rather than changing the
constant speculatively.

**Consequences**: The likely gain is largest for 8192+ taps, but the real-FFT
path requires careful latency, tail, and numerical-parity validation. The
benchmark becomes a maintained evidence contract instead of a print-only
algorithm smoke test.

## Out of Scope

* Changing short/medium overlap-save mathematics for its own sake.
* Altering public latency, finish/drain semantics, or convolver ownership.
* Introducing native dependencies or runtime threads in the callback path.
* Tuning FIR EQ control-side regeneration or Rubato UltraHigh in this task.

## Technical Notes

* Relevant implementation: `src/processor/convolver.rs`.
* Relevant benches: `benches/audio_convolver_perf.rs`,
  `benches/audio_fir_eq_perf.rs`, and `benches/audio_callback_chain_perf.rs`.
* Required specs: `.trellis/spec/backend/realtime-safety.md`,
  `.trellis/spec/backend/streaming-lifecycle.md`,
  `.trellis/spec/backend/quality-guidelines.md`, and
  `.trellis/spec/backend/analysis-fir-correctness.md`.
