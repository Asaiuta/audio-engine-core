# Optimize Rubato Nonlinear Phase Streaming Performance

## Goal

Replace the time-domain rational polyphase FIR used by the pure-Rust
`PhaseResponse::Minimum` / `Maximum` streaming path with an FFT block-convolution
(spectral rational resampling) engine, cutting 48→96 High/Minimum from
~761 ns/input sample toward the Linear FFT path's ~4.7 ns (soxr reference:
13.8 ns), across all quality tiers and all accepted rational ratios.

## Requirements

* New spectral nonlinear engine: overlap-save input FFT (`Nin = 2·nin`,
  `nin = down·s ≥ taps_per_phase`), one complex multiply by the precomputed
  minimum-phase kernel spectrum, inverse FFT at `Nout = 2·nout`
  (`nout = up·s`); ×up normalization folded into the kernel spectrum.
* Reuse the existing kernel design (`design_linear_prototype` →
  `minimum_phase_prototype`) unchanged; Maximum phase = reversed kernel as
  today. `latency_frames` (phase-peak) and `finish_extension_frames`
  ((L−1)/down) formulas carry over; `output_delay() = 0`.
* Route ALL nonlinear ratios (up, down ≤ MAX_REDUCED_RATE = 1024) and all
  quality tiers to the new engine; remove/retire the time-domain polyphase
  from the streaming path only if the candidate is retained.
* Exact-rational per-block pacing so the adapter's `prefix_budget_direct`
  fast path applies where eligible.
* Realtime safety: realfft plans and all buffers allocated in setup;
  bounded ≤ ⌈chunk/nin⌉ blocks of work per call; no allocation/locking/
  logging in process/finish.
* Preserve lifecycle contracts: emitted advances exactly once, delay skip,
  expected duration, finish/terminal, reset-to-fresh, backpressure.
* Acceptance is quality-gate based (not bit-exact vs old polyphase):
  numerical parity regression FFT-vs-polyphase max error < 1e-9, plus the
  established gates.

## Acceptance Criteria

* [ ] Fresh same-revision baseline (matrix bench, nonlinear cases) and
      candidate evidence persisted under `research/` with distinct
      compatible algorithm identities.
* [ ] 48→96 High/Minimum improves ≥10x (retention gate: ≥5% at minimum,
      no >5% regression on Linear-path cases); 44.1→48 nonlinear also improves.
* [ ] FFT-vs-polyphase max-error < 1e-9 regression on representative ratios,
      tiers, and both nonlinear phases.
* [ ] Staged/split-pattern streaming parity, reset-to-fresh, and
      assert_no_alloc regressions pass for the new engine.
* [ ] Listening/nonlinear quality gates, all 27 quick quality gates, strict
      clippy, fmt, and both backend feature-matrix test configs pass.

## Definition of Done

* Evidence JSON persisted under `research/`; `docs/quality.md` updated only if
  the retained evidence supersedes public numbers.
* Spec updated (`.trellis/spec/backend/`) if a durable new engine contract
  emerges.

## Decision (ADR-lite)

**Context**: The polyphase hot loop (per-tap modulo indexing) could be fixed
in place (~5–20x) or replaced with FFT block convolution (near-Linear ceiling).

**Decision**: Approach B — spectral rational resampling with the minimum-phase
kernel spectrum, phase-agnostic engine structure, quality-gate acceptance.

**Consequences**: Larger change surface; latency/finish/reset re-verified but
formulas carry over. If the retention gate fails, revert to the current
polyphase backend and keep only research evidence.

## Out of Scope

* UltraHigh Linear FFT routing (separate follow-up task).
* Changing phase semantics, public presets, or sample-rate APIs.
* Offline/parallel (non-streaming) resample paths beyond what routing shares.

## Technical Notes

* Primary files: new engine file (~400–500 lines) +
  `src/processor/resampler/rubato_backend.rs` routing;
  `src/processor/resampler/polyphase_backend.rs` retirement decision.
* Risks with tests: circular aliasing (`nin ≥ taps_per_phase`), energy
  normalization, spectrum = FFT of zero-padded time kernel.
* Benches: `audio_resampler_matrix_perf.rs` (nonlinear cases),
  `audio_resampler_streaming_perf.rs` unchanged case keys.
* Specs: realtime-safety.md, streaming-lifecycle.md, quality-guidelines.md,
  listening-nonlinear-correctness.md, dsp-state-correctness.md.

## Research References

* [`research/fft-nonlinear-phase-design.md`](research/fft-nonlinear-phase-design.md)
  — structure comparison, cost model, routing, risks, recommended design.
