# Optimize Rubato 44.1 to 48 High Streaming Performance

## Goal

Reduce the realtime CPU cost of the pure-Rust Rubato
`44.1 kHz -> 48 kHz`, `PhaseResponse::Linear`, `ResampleQuality::High`
streaming path without changing its numerical output, duration, lifecycle, or
realtime-safety contracts. The first candidate is a non-integer-ratio direct
caller-output path with a bounded preallocated spill fallback.

## What I Already Know

* The retained route is Rubato `Fft<f64>` with a 1024-frame fixed input chunk,
  two FFT sub-chunks, one native interleaved engine, and fixed-capacity input
  and output rings.
* Existing direct output is limited to ratios where one complete backend chunk
  maps to an integer output duration. `1024 * 48_000 / 44_100` is not an
  integer, so 44.1->48 always follows `engine -> out_stage -> out_fifo ->
  caller` today.
* Per-chunk output at 147:160 can jitter around the cumulative rational
  duration. Direct output therefore needs a prefix-duration budget and a
  bounded spill path; simply removing the current guard can retain
  overproduced frames at drain.
* Caller-visible output must advance `emitted` exactly once. An earlier direct
  path forgot this and corrupted 96->48 finish output, degrading the alias
  result from -208.11 dB to -99.33 dB until the accounting bug was fixed.
* The older public quick table reports 9.86 ns/input sample at 512 frames, but
  a later same-machine heavy ring-FIFO comparison measured 8.200. The new task
  must collect a fresh same-revision baseline and may not assume a persistent
  gap against the older 8.45 ns/sample SoXR report.
* The exact-2x High half-band task changes the benchmark algorithm identifier,
  so its uncommitted work must be committed separately before this task writes
  implementation code or captures compatible baselines.

## Requirements

* Keep routing unchanged: 44.1->48 Linear/High remains on the Rubato FFT
  engine; other ratios, quality tiers, and phase responses retain their current
  engines.
* Add a safe non-integer-ratio direct-output candidate that writes the common
  prefix directly into caller-owned output and stages only bounded overflow.
* Track real backend-processed input separately from caller input queued in the
  FIFO so the direct branch can calculate a cumulative rational output budget.
* Never expose more than
  `round(processed_real_input * to_rate / from_rate)` caller-visible frames
  before finish; zero padding used by drain does not expand that real-input
  budget.
* Preserve the existing delay skip, `emitted`, expected-duration, finish,
  terminal, reset, and backpressure contracts.
* Allocate spill storage during setup. `process` and `finish` remain free of
  allocation, locking, logging, I/O, runtime environment switches, and
  unbounded work.
* Change the benchmark algorithm identifier if the candidate is retained.

## Acceptance Criteria

* [ ] A fresh same-revision Rubato and SoXR baseline covers 44.1->48
      `process_checked` at 128/256/512 caller frames with valid work.
* [ ] Direct and deliberately staged complete streams are bit-exact for
      44.1->48 across ordinary, output-constrained, and irregular caller
      chunks.
* [ ] Prefix output never exceeds its rational real-input budget; complete
      finish output has the established rounded duration with no duplicated or
      missing frames.
* [ ] Reset after process and partial finish matches a fresh instance.
* [ ] Process and finish allocate nothing after setup.
* [ ] The candidate improves the adjacent heavy 512-frame Rubato median by at
      least 5%, while neither 128 nor 256 frames regresses by more than 5%.
      Otherwise revert the candidate and retain only the research result.
* [ ] All 27 quick quality gates, strict Clippy, formatting, and both backend
      feature-matrix tests pass.

## Definition of Done

* Baseline and candidate JSON reports are persisted under `research/` with
  distinct compatible algorithm identities.
* Focused lifecycle and zero-allocation regressions cover the new storage path.
* Public performance documentation changes only if the retained evidence
  supersedes the existing numbers.
* Relevant Trellis streaming and quality specifications record any durable new
  prefix-budget/spill contract.

## Decision (ADR-lite)

**Context**: The 147:160 route cannot use the current integer-duration direct
branch, but copying every produced sample through both the output stage and
ring may be avoidable.

**Decision**: Evaluate direct caller output with a cumulative real-input
duration budget and a preallocated spill fallback before considering a new
147:160 DSP engine.

**Consequences**: This keeps the established FFT numerical sequence and limits
the first change to adapter storage/accounting. It adds lifecycle complexity,
so bit-exact staged parity and emitted/drain regression coverage are mandatory.
If the measured benefit is below the retention threshold, the implementation
is reverted rather than kept as unearned complexity.

## Out of Scope

* 96->48 UltraHigh stopband optimization; that will be a separate task.
* Replacing the 147:160 FFT with a custom block-polyphase FIR in the first
  iteration.
* General FFT chunk/sub-chunk auto-tuning across every ratio.
* Changing public quality presets, phase semantics, or sample-rate APIs.
* Runtime architecture switches or setup-time microbenchmark selection.

## Technical Notes

* Primary implementation: `src/processor/resampler/rubato_backend.rs`.
* Focused benchmark: `benches/audio_resampler_streaming_perf.rs`.
* Required specs: `.trellis/spec/backend/realtime-safety.md`,
  `streaming-lifecycle.md`, `quality-guidelines.md`, and `error-handling.md`.
* Prior direct-output evidence and lifecycle regression:
  `../07-24-optimize-rubato-nonlinear-phase/research/phase-and-optimization.md`.

## Research References

* [`research/noninteger-direct-output.md`](research/noninteger-direct-output.md)
  — current data flow, prefix-budget design, risks, and alternative paths.

