# Speed up the AutoMix spectral-flux hot path

## Problem

Offline AutoMix analysis is the most expensive operation in this crate. A
measured subtractive breakdown (recorded in `docs/quality.md` under
*AutoMix cost breakdown*) put the spectral-flux FFT at 27.4% of the inner loop.

`SpectralFluxAccumulator::process` recomputes its 1,024-point Hann window with
`cos()` on **every hop**, even though the window is a compile-time constant. At
1,024 `cos()` calls per 512-sample hop, that costs more than the FFT itself.

`SpectrumAnalyzer` in `src/processor/spectrum.rs` already caches its window in a
`Vec<f64>` field (line 17, built at line 53). AutoMix simply never adopted that
pattern.

## Goal

Cache the Hann window in `SpectralFluxAccumulator`, matching the existing
`SpectrumAnalyzer` shape. Output must stay bit-identical.

## Non-goals / rejected alternatives

Three true-peak optimizations were investigated and **rejected on measurement**.
Recorded here so they are not retried:

1. **Symmetric coefficient folding.** The polyphase bank is linear-phase, so
   phase3 is the reverse of phase1 and phase2 is its own reverse (verified to
   2.8e-17). Folding phase2 into 6 mul + 6 add measured **+0.0%** — no gain at
   all — while changing the result on 2,753/4,096 windows. Pure downside.
2. **AVX2 + FMA across the three phases.** Measured **+32.5% slower**, and a
   `mul_add` accumulator chain was **+1082%** slower (serial dependency chain).
   Inspecting the emitted assembly explains it: the compiler already packs the
   multiplies (`14 mulpd`, plus `unpckhpd`/`shufpd` reshuffles) and keeps the
   additions scalar to preserve summation order. It is already near-optimal for
   a form that must not reassociate.
3. **L1-bound early exit.** Any phase output is bounded by
   `max|window| * 1.864182`, so a window could in principle be skipped when that
   bound cannot beat the running maximum. The skip rate measured **0.0% on tonal
   audio** and 1.2% on a pathological quiet signal, while costing 67%-298%. The
   reason is structural, not tunable: the L1 norm is 1.864 > 1, and a 12-sample
   window at 48 kHz spans a quarter cycle of a 1 kHz tone, so it almost always
   contains a near-peak sample. The guard can never fire on real tonal audio.

The 4x-oversampled true-peak FIR therefore stays as it is.

## Acceptance criteria

1. `SpectralFluxAccumulator` holds a precomputed window; no `cos()` in
   `process`.
2. Flux output is **bit-identical** to the current implementation. The existing
   `legacy_spectral_flux` test oracle (which still computes `cos()` inline) must
   keep passing unchanged — it is the independent check.
3. Measured improvement reported with a drift control, per
   `.trellis/spec/backend/quality-guidelines.md`.
4. No public API change; all three public-API matrices unchanged.
5. `docs/quality.md` AutoMix breakdown updated with the re-measured share.

## Evidence

Probe measurements, 9 trials, median, one host:

| Variant | ns/hop | ns/input sample | Change |
| --- | ---: | ---: | ---: |
| Current (`cos()` per hop) | 7,840 | 15.31 | — |
| Cached window | 2,158 | 4.21 | **−72.5%** |
| Cached window + iterators | 2,166 | 4.23 | −72.4% |

Both variants verified bit-identical to the current output across 64 hops.
