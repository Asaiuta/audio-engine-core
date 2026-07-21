# brainstorm: rubato resampler performance

## Goal

Close (or drastically shrink) the 16-27x performance gap between the pure-Rust rubato resampler
backend and the default SoXR backend, without giving up the crate's Hi-Fi evidence standards
(-180 dB-class fidelity metrics), and decide whether that work should happen on rubato 0.16.2 or
after an upgrade to rubato 4.0.0.

## What I already know

* Measured gap (2026-07-21, same machine, `audio_resampler_streaming_perf --quick`, 512f
  `process_checked` medians): rubato sinc High 133.59 ns/input-sample vs SoXR 8.45 (44.1k→48k);
  179.59 vs 6.73 (48k→96k). Utilization is still comfortably realtime (p95 ≤ 2%).
* Root cause is algorithmic, not missing SIMD: rubato runtime-selects AVX+FMA f64; High-tier
  sinc params (256 taps × cubic = 4 dot products/output frame) simply cost ~1100 MACs/input sample.
* Probe (validated against repo quality evidence) shows rubato's own `FftFixedIn` (synchronous
  FFT resampling for rational ratios; already compiled in via rubato's default `fft_resampler`
  feature) reaches 8.8 ns/sample (44.1k→48k) and 25.9 (48k→96k) with fidelity equal or better
  than sinc High: 1 kHz residual -201.6 dB, 20 kHz gain exactly 0.0000 dB, 26 kHz alias -184.2 dB.
  Full data: `../07-19-upgrade-symphonia-0-6/research/rubato-performance-optimization.md`.
* Sinc parameter tuning (cubic→linear) only gains ~2x and collapses 20 kHz residual to
  -105..-117 dB — off-brand for this crate's evidence tables.
* `FftFixedIn` integration needs: leading-delay skip in the adapter (`output_delay()` is a real
  delay for FFT, unlike SincFixedIn which pre-compensates), sinc fallback for pathological
  ratios (e.g. 44100→44101), and the full evidence protocol re-run (27 quality gates, resampler
  bench, docs/quality.md refresh) per `.trellis/spec/backend/quality-guidelines.md`.
* rubato 4.0.0 exists on crates.io (major version, API rework). Our Cargo.toml pins
  `rubato = { version = "0.16", optional = true }`; crate MSRV is rust-version = 1.87.
* Both rubato paths (sinc + FFT) are linear-phase only; `PhaseResponse` stays accepted-not-applied.

## Assumptions (validated by research 2026-07-21)

* rubato 4.0.0 provides a synchronous FFT resampler equivalent to `FftFixedIn`
  (`Fft::new_custom(..., FixedSync::Input)`, same gcd/sub_chunks/delay semantics). ✔
* MSRV 1.85 (< our 1.87), license MIT OR Apache-2.0, `fft_resampler` still default,
  process calls allocation-free after construction. ✔

## Requirements (evolving)

* Keep the public `Resampler` / `StreamingResampler` API and streaming contract unchanged
  (arbitrary input granularity, duration-aligned drain, bitwise chunking invariance, reset).
* Keep f64 processing and current evidence quality class (passband/THD+N/alias all ≤ existing
  documented rubato numbers or better, still beating the SoXR reference rows where they already do).
* No new native dependencies; `--no-default-features --features rubato` stays pure Rust.

## Acceptance Criteria (evolving)

* [x] rubato backend 44.1k→48k 512f High `process_checked` median lands within ~2x of SoXR
      (9.86 vs 8.45 ns/input-sample) on the reference machine.
* [x] All 27 quick quality gates pass under the rubato feature. Quality-aware routing keeps the
      public default High on FFT while UltraHigh uses sinc, restoring 44.1k→48k THD+N to
      -216.24 dB (old sinc: -216.2 dB), 20 kHz gain to -0.0017 dB, and 96k→48k worst alias to
      -208.11 dB. The rejected all-common-ratio FFT route measured -200.63 dB THD+N.
* [x] Resampler suite passes (impulse alignment ±1 frame, duration alignment, bitwise chunking
      invariance, no-alloc, reset), including an end-to-end pathological-ratio sinc fallback.
* [x] docs/quality.md budget + backend tables refreshed with new same-machine evidence.

## Definition of Done (team quality bar)

* Tests added/updated; both clippy matrices, fmt, both test matrices green
* Evidence JSONs regenerated with backend-aware labels
* CHANGELOG + docs updated
* Spec updated if new contracts emerge

## Decision (ADR-lite)

**Context**: rubato backend is 16-27x slower than SoXR; the fix (FFT routing) can be built on
frozen 0.16.2 or on the 12-day-old 4.0.0; 4.0 also changes sinc delay semantics (real leading
delay), making the delay-skip adapter mandatory on any future 4.x regardless.

**Decision** (user, 2026-07-21): Approach B — upgrade to rubato 4.0.0 first, then FFT routing on
it, staged as two PRs (PR1 upgrade + sinc port + delay-skip + evidence parity; PR2 FFT routing +
sinc fallback + evidence/docs refresh).

**Consequences**: delay-skip written once for both paths; single final evidence state on the
maintained line. Low through High common ratios use FFT for throughput, while UltraHigh retains
the strongest sinc quality semantics. `OutputRenderChain` requests UltraHigh, so its resampled
rubato scenario is about 2.8x slower than the rejected all-FFT route, though the measured realtime
factor remains below 3.2%. We accept 4.0.0 freshness risk (no patch release yet; panic bug #136
sits in ratio-ramping code our fixed-ratio backend never executes), mitigated by deterministic
quality gates and impulse/duration alignment tests.

## Implementation Plan (small PRs)

* PR1: bump `rubato = "4.0"`; port adapter to `Async::new_sinc(..., FixedAsync::Input)`; add
  leading-delay skip (anchor tests on measured behavior; reported delay may be 1 frame high);
  full test + gate parity run — performance may not regress, quality rows may not drop.
* PR2: route Low through High small-rational ratios (reduced numerator bound) through
  `Fft::new_custom(..., FixedSync::Input)` with sinc for UltraHigh and pathological ratios;
  re-run quality gates + resampler bench; refresh docs/quality.md budget + backend tables,
  CHANGELOG.

## Out of Scope (explicit)

* f32 internal processing (violates f64 Hi-Fi contract)
* Minimum-phase support (rubato is linear-phase only, documented)
* SoXR backend changes

## Technical Notes

* Backend adapter: `src/processor/resampler/rubato_backend.rs` (FIFO chunk adaptation, CHUNK_IN
  1024, drain padding/truncation to `round(total_input * to / from)`).
* Spec constraints: `.trellis/spec/backend/quality-guidelines.md` (evidence policy, versioned
  benchmark scenario, backend labeling contract).
* Probe findings: `../07-19-upgrade-symphonia-0-6/research/rubato-performance-optimization.md`.

## Research References

* [`research/rubato-4-migration.md`](research/rubato-4-migration.md) — rubato 4.0.0 verified viable:
  MSRV 1.85, MIT OR Apache-2.0, fft_resampler default, f64 + alloc-free process confirmed against
  the real compiled crate; 0.16.2 frozen since 2025-03-31; migration ~70-90 lines of the adapter.
* `../07-19-upgrade-symphonia-0-6/research/rubato-performance-optimization.md` — probe data: FFT
  path 8.8/25.9 ns per sample with equal-or-better fidelity; sinc tuning rejected.

## Feasible Approaches

**Approach A: FFT routing on 0.16.2 now, defer 4.0**

* How: implement `FftFixedIn` routing + leading-delay skip on the pinned 0.16.2; evidence re-run.
* Pros: zero dependency risk (0.16.2 is the version all current evidence was generated on);
  smallest immediate diff; probe numbers apply verbatim.
* Cons: invests new architecture in a frozen branch (0.16 unmaintained since 2025-03-31); at the
  eventual 4.0 migration the sinc path's delay semantics change anyway, forcing a second adapter
  touch and a second full evidence re-run.

**Approach B (Recommended): upgrade to rubato 4.0.0, then FFT routing on it (staged PRs)**

* How: PR1 bump `rubato = "4.0"`, port adapter (`SincFixedIn` → `Async::new_sinc(...,
  FixedAsync::Input)`), add the now-mandatory delay-skip for sinc, prove evidence parity;
  PR2 add `Fft::new_custom(..., FixedSync::Input)` routing (same gcd/sub_chunks/delay semantics
  as 0.16 FftFixedIn, delay measured exact at 320), refresh evidence and docs.
* Pros: lands on the maintained line; delay-skip written once covers both paths (4.0 unified the
  semantics); one final evidence state instead of two; research verified MSRV/license/defaults/
  alloc-free against the real crate; known 4.0 panic bug (#136) lives in ratio-ramping code our
  fixed-ratio backend never executes.
* Cons: 4.0.0 is 12 days old, no patch release yet; larger single change; undiscovered 4.0
  regressions become ours to catch (mitigated by the deterministic gate suite + parity tests).
* Risk note: 4.0 sinc reported `output_delay()` measured 1 frame high (139 reported vs 138
  actual); impulse test tolerance is ±1 and duration-aligned drain is unaffected, but the
  adapter should anchor on measured behavior in tests.

**Approach C: sinc parameter tuning only (no FFT, no upgrade)**

* How: High tier → linear interpolation / shorter kernels.
* Pros: trivial diff.
* Cons: rejected by probe data — only ~2x (still 8-10x slower than SoXR) and 20 kHz residual
  collapses to -105..-117 dB, below this crate's documented evidence class.

## Open Questions

None. Approach B was selected and its FFT quality/performance tradeoff is captured above.
