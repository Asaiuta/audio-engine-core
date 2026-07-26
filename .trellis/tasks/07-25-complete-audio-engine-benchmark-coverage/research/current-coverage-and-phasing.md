# Current Benchmark Coverage And Recommended Phasing

> Historical planning artifact (superseded 2026-07-26): this file records the
> nine-probe inventory before implementation. The callback-tail, decoder,
> component, and lifecycle-memory gaps listed below are now implemented. See
> `final-coverage-and-results.md` for the final 13-probe coverage matrix and
> verified exclusions.

## Pre-implementation inventory

The repository has nine registered custom-main benchmarks:

1. callback-chain aggregate throughput and deadline reference
2. offline output render CPU and Rust allocation evidence
3. streaming resampler aggregate throughput
4. resampler configuration matrix and setup cost
5. objective audio-quality measurements
6. convolution throughput and per-callback burst timing
7. lock-free parameter read comparisons
8. FIR EQ generation and convolution apply cost
9. fixture-driven gapless decoder comparison

The suite is mature for steady-state DSP and resampling decisions. It is not a
complete map of every public engine surface or every realtime risk dimension.

## Highest-risk blind spots

### Full-chain callback tail

`audio_callback_chain_perf` divides the elapsed time of many callback
iterations to produce one average sample per trial. Its p95 is therefore a
percentile of trial averages, not a percentile of individual callbacks. That
is suitable for steady-state throughput but cannot expose rare callback
spikes, p99.9 behavior, or a missed-deadline rate.

`audio_convolver_perf` already provides the nearest reusable pattern: retain
every callback duration, summarize p95/p99/max, calculate deadline utilization,
and optionally run with Windows affinity and priority controls.

### Decoder and startup

The gapless comparator separates open and borrowed streaming decode time, but
its default corpus is optional Ogg/FLAC, timing is report-only, and enforcement
checks correctness only. A general decoder probe needs stable corpus metadata
and separate open/probe, first-PCM, steady decode, seek, and allocation fields.

### Memory and lifecycle

The offline render probe counts Rust global-allocator activity around setup and
rendering. It deliberately excludes native SoXR bytes and does not cover
process RSS, repeated-track growth, dynamic convolver replacement, reset, or
finish/drain cost.

### Public components without a timing probe

`SpectrumAnalyzer`, `Downmixer`, AutoMix analysis, `RingBuffer`, isolated
`LoudnessMeter`/`TruePeakDetector`, and `LoudnessDatabase` are public or
feature-visible surfaces without dedicated performance evidence.

## Feasible delivery approaches

### A. Phased risk-first program (recommended)

* First add full-chain per-callback tail evidence.
* Then add decoder/startup and memory/lifecycle probes.
* Add component microbenchmarks after the critical paths have stable contracts.
* Integrate portable report integrity in shared CI and reserve strict timing
  gates for compatible same-machine baselines.

Benefits: resolves the largest audible-risk blind spot first, keeps reviews and
runtime bounded, and produces usable evidence after each phase.

Trade-off: the parent coverage task remains open across several deliverables.

### B. Broad shallow component sweep

Add one small timing loop for every currently unbenchmarked public module, then
strengthen distributions, memory, and baselines later.

Benefits: quickly improves the module coverage count.

Trade-off: creates weak evidence contracts and defers callback-tail and decoder
risk; later strengthening is likely to invalidate early baselines.

### C. Decoder-first

Build a deterministic codec corpus and decoder/startup/seek probe before
changing callback evidence.

Benefits: directly supports startup, seek, and dependency-upgrade decisions.

Trade-off: fixture/corpus design is larger and leaves the full callback p99/max
blind spot in place longer.

## Recommended phase boundaries

1. Full-chain callback tail distribution and report/gate support.
2. Decoder local-corpus startup, first-PCM, throughput, seek, and allocation.
3. Memory/lifecycle plus spectrum/downmix/loudness/ring microbenchmarks.
4. AutoMix/database and consuming-application device integration follow-ups.
5. CI/dedicated-runner policy finalized as each probe becomes stable.

## Phase 1 probe-shape decision

Two implementation shapes were compared after the user selected callback tail
as the first slice.

### Extend `audio_callback_chain_perf`

This minimizes executable count and can reuse private scenario configuration
directly. However, the current probe intentionally emits one timing sample per
trial after averaging 1,000 to 30,000 calls. Adding per-call raw samples would
mix two measurement methodologies in one case/report. Adding sampling
conditions or changing the schema would also make historical baselines
incompatible.

### Add `audio_callback_tail_perf` (recommended)

Keep the existing aggregate probe unchanged and add a dedicated report whose
primary samples are individual callback durations. Extract the canonical chain
scenario/configuration into bench-local support so the two probes cannot drift.
Use the convolver callback-burst implementation as the source pattern for raw
sample retention, nearest-rank p99, deadline utilization, and optional Windows
pinning.

This creates one extra executable but preserves baseline meaning and makes the
sampling contract obvious from the probe identity. It also permits a bounded
quick case set without multiplying the runtime of the existing aggregate
benchmark.

### Tail-statistics implications

* Quick mode needs thousands of calls per case so nearest-rank p99.9 is backed
  by multiple observations rather than one maximum sample.
* Max and missed-deadline counts remain evidence even when scheduler activity
  makes them noisy; samples are never trimmed.
* Timer overhead should be documented, not subtracted from individual samples,
  because subtraction can turn small cases invalid and hide real outliers.
* Strict p99/p99.9 regression gates require compatible pinned same-machine
  evidence. Shared CI can validate work/report integrity without claiming a
  portable absolute timing ceiling.

## Constraints

* Preserve unrelated dirty work and existing benchmark case semantics.
* No live network dependency in quick mode.
* Keep device-level claims outside this crate.
* Do not treat a successful compile or report-integrity gate as a timing
  regression pass.
* SoXR and Rubato reports remain backend-identified and baseline-incompatible.
