# Route UltraHigh Linear Resampling to the FFT Engine

## Goal

Cut the pure-Rust `ResampleQuality::UltraHigh` + `PhaseResponse::Linear`
streaming cost by routing it off the per-sample sinc engine onto the rubato
`Fft` engine with a quality-conditional sub-chunk count, while measurably
strengthening (not just preserving) UltraHigh's quality lead over High.

## Requirements

* Routing: drop the UltraHigh exclusion in `should_use_fft`; UltraHigh Linear
  uses rubato `Fft` with **1 sub-chunk** (2× longer internal FIR), High keeps
  its current 2 sub-chunks. Keep the sinc fallback for pathological ratios
  (reduced ratio > FFT limit) exactly as today.
* No changes to other tiers, phases, ratios, sinc parameters, or public APIs.
* Preserve realtime-safety and lifecycle contracts (delay skip for the Fft
  engine's real leading delay, emitted, expected duration, finish, reset,
  backpressure; process/finish allocation-free after setup).
* Quality: UltraHigh must remain the strongest tier with fresh evidence from
  the quality harness (THD+N, passband ripple, alias/stopband), compared
  against both High (Fft sub2) and the previous UltraHigh sinc numbers.
* Fresh same-revision baseline + candidate matrix evidence with distinct
  compatible algorithm identities; retention gate ≥5% (expected ~3–10x),
  no >5% regression on other cases.

## Acceptance Criteria

* [ ] Baseline (sinc UltraHigh) and candidate (Fft sub1) matrix evidence
      persisted under `research/`.
* [ ] 44.1→48 and 48→96 UltraHigh Linear improve ≥3x (evidence target:
      ~114→35 ns and ~220→19 ns); setup drops from ~5.7–7.6 ms to sub-ms.
* [ ] Quality harness (UltraHigh cases) rerun: alias/stopband and passband
      at least match Fft-sub1 archived numbers (THD+N ≈ −205 dB, alias
      ≈ −290 dB) and beat both High and old sinc on the tier-ordering axes.
* [ ] 27 quick quality gates, strict clippy, fmt, both feature-matrix test
      configs pass; streaming bench case keys unchanged; matrix bench
      algorithm id bumped.
* [ ] Reset/lifecycle regressions for the new routing pass (existing suites).

## Definition of Done

* Evidence JSON under `research/`; `docs/quality.md` updated where the
  retained evidence supersedes public numbers; spec updated if a durable
  contract emerges (e.g. sub-chunk count as the quality knob).

## Decision (ADR-lite)

**Context**: UltraHigh Linear was kept on per-sample sinc "to preserve the
strongest sinc quality tier"; sinc costs 91–220 ns/input sample and ~5.7 ms
setup. Candidates: A′ rubato Fft with 1 sub-chunk; B spectral engine with a
linear Kaiser kernel; C half-band cascade hybrid.

**Decision**: A′. Archived same-machine harness runs show Fft-sub1 beats both
Fft-sub2 (High) and the current sinc on THD+N, passband flatness, and alias
(−290.5 dB), so the tier ordering survives with evidence. B is rejected:
beta-17 Kaiser caps stopband at ~163 dB (a quality regression) and the alias
fold makes 147:160 slower than today's sinc; C is rejected with it.

**Consequences**: ~10-line routing change plus tests/evidence. UltraHigh and
High now differ by sub-chunk count rather than engine family; that knob and
its quality effect must be documented. Sinc remains only as the pathological-
ratio fallback.

## Out of Scope

* Nonlinear-phase 147:160 speed (separate task).
* Changing sinc parameters or other tiers' routing.
* Offline/parallel path changes beyond shared routing.

## Technical Notes

* Files: `src/processor/resampler/rubato_backend.rs` (`should_use_fft`,
  Fft construction with quality-conditional sub-chunks, doc comments).
* Quality harness: `audio_quality_measurements` (UltraHigh-hardcoded cases).
* Benches: `audio_resampler_matrix_perf.rs` UltraHigh cases; streaming bench
  untouched.

## Research References

* [`research/ultrahigh-linear-fft-routing.md`](research/ultrahigh-linear-fft-routing.md)
  — candidate comparison, archived quality numbers, perf estimates, rejected
  alternatives.
