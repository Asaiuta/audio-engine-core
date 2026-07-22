# Optimize Rubato UltraHigh Offline Rendering

## Goal

Reduce the CPU and setup-memory cost of Rubato-backed 44.1 kHz to 48 kHz
offline rendering without weakening the established UltraHigh resampling
quality contract or changing realtime High-quality routing.

## What I Already Know

* Rubato 4 common-ratio High quality uses FFT and meets the streaming target.
* UltraHigh deliberately uses the 256-tap, 512x-oversampled cubic sinc engine.
* The retained UltraHigh path measured -216.24 dB THD+N and -208.11 dB worst
  fitted alias attenuation in the quick quality suite.
* The UltraHigh output-render path measured 353.97 ns/input sample for one
  second and 266.38 ns/input sample for five seconds at 4096-frame blocks.
* A diagnostic all-FFT route measured 126.64 and 93.65 ns/input sample for the
  same cases, but only about -200.63 dB THD+N, so it is not acceptable as an
  UltraHigh replacement.
* FFT sub-chunk tuning has already been swept for the existing engine. One
  sub-chunk improved THD+N only to -204.95 dB and regressed 44.1 to 48 kHz
  streaming performance to 35.10 ns/input sample.

## Assumptions

* UltraHigh keeps its current numerical meaning; this task does not relabel a
  faster lower-quality algorithm as UltraHigh.
* The existing public realtime/default High route remains unchanged.
* A candidate must be measured on the same machine with interleaved or
  otherwise noise-aware A/B trials before it is retained.
* A no-code or benchmark-only result is valid if every feasible candidate
  misses the quality/performance constraints.

## Requirements

* Establish a fresh Rubato UltraHigh output-render and resampler-quality
  baseline from the current revision.
* Evaluate higher-quality FFT tuning, FFT/sinc hybrid opportunities, offline
  batch-specific processing, and offline quality-policy separation at the
  code and benchmark level.
* Retain only a candidate that preserves the established UltraHigh quality
  evidence while materially reducing offline render cost.
* Preserve exact duration, delay compensation, chunking invariance, drain,
  finite-output, and allocation/error contracts.
* Keep pathological-rate sinc fallback behavior correct.
* Record rejected candidates and measured tradeoffs in `research/`.

## Acceptance Criteria

* [x] All existing Rubato tests and all quick audio quality gates pass.
* [x] 44.1 kHz to 48 kHz UltraHigh THD+N does not regress from the retained
      approximately -216.24 dB result beyond normal measurement precision.
* [x] Passband and stopband results do not regress from the retained UltraHigh
      evidence beyond normal measurement precision.
* [x] Any retained implementation improves the active 44.1 kHz to 48 kHz
      output-render median by at least 15% in repeated same-machine A/B trials.
* [x] Existing High streaming performance remains within its benchmark gate.
* [x] Formatting, strict Clippy matrices, compile checks, focused tests, and
      relevant performance gates pass.
* [x] If no candidate satisfies both quality and performance criteria, the
      current sinc route is retained and the negative result is documented.

## Definition of Done

* Candidate measurements and the final decision are persisted under
  `research/`.
* Tests and benchmark labels cover any routing or policy change.
* Public documentation is updated only if observable behavior changes.
* Changes are quality-checked and committed without including unrelated work.

## Technical Approach

1. Reproduce current quality and offline-render baselines.
2. Inspect Rubato's FFT and sinc construction/runtime costs and the
   `OutputRenderChain` resampling boundary.
3. Run narrow probes before product-code changes: FFT window/sub-chunk quality,
   sinc parameter/runtime sweeps, and batch/offline adapter overhead.
4. Prototype the strongest evidence-backed candidate, then run full quality,
   correctness, and performance validation.
5. Revert rejected prototypes and retain their measurements only.

## Decision (ADR-lite)

**Context**: The existing sinc route preserves legacy UltraHigh numbers but is
about 2.8 times slower than the lower-quality FFT reference.

**Decision**: Retain UltraHigh's Cubic/512 sinc parameters and use one native
interleaved Rubato engine for all channels in `StreamingResampler`. This shares
the sinc table or FFT plan while removing adapter-side channel splitting.

**Consequences**: UltraHigh numerical quality and High FFT routing remain
unchanged. Pure-Rubato setup memory drops by about 59%, and noise-aware ABBA
trials improve active offline rendering by 32-58%. SoXR remains unchanged.

## Out of Scope

* Changing the SoXR backend.
* Reworking realtime High routing already completed by the 07-21 task.
* Weakening quality gates or silently changing UltraHigh semantics.
* Unrelated Symphonia upgrade work.

## Technical Notes

* Main backend: `src/processor/resampler/rubato_backend.rs`.
* Offline boundary: `src/processor/output_chain.rs`.
* Performance bench: `benches/audio_output_render_perf.rs`.
* Quality bench: `benches/audio_quality_measurements.rs`.
* Prior evidence: `.trellis/tasks/archive/2026-07/07-21-rubato-resampler-performance/research/final-evidence.md`.
