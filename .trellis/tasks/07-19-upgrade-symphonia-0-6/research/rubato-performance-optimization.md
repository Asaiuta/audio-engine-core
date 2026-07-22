# Rubato backend performance optimization (probe findings)

Date: 2026-07-21. Machine: same as bench evidence (Intel Alder Lake, x86_64-pc-windows-msvc, release).
Probe crate: standalone, rubato =0.16.2 direct, mono-per-channel streaming model mirroring
`MonoBackend` (1024-frame fixed chunks). Probe validated against repo evidence: sinc High config
reproduces the official quality numbers (1 kHz residual -215.5 dB vs repo THD+N -216.2 dB;
20 kHz gain -0.0017 dB exact match).

## Problem

rubato backend (`SincFixedIn`, High = sinc_len 256 / oversampling 256 / cubic / BH2, f64) measures
133.59 ns/input-sample vs SoXR 8.45 (44.1k→48k, 512f) and 179.59 vs 6.73 (48k→96k) — a 16-27x gap.

## Root cause

- SIMD is NOT the problem: rubato's `make_interpolator` runtime-selects AVX+FMA f64 (`__m256d`)
  on this CPU (probe confirms avx/fma/avx2 all detected). The cost is algorithmic: cubic
  interpolation evaluates 4 sinc sub-filters of 256 f64 taps per output frame (~1100 MACs per
  input sample at ratio 1.088).
- The `fft_resampler` feature (rubato default, already compiled into our build) provides
  `FftFixedIn`: synchronous FFT resampling for rational rate pairs. All our rate pairs are
  rational (u32/u32). We simply never use it.

## Probe results (ns per input channel-sample, median of 7; 2x mono instances)

| Config | 44.1k→48k | 48k→96k | 1 kHz resid | 20 kHz gain | 20 kHz resid | 26 kHz alias 96k→48k |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sinc 256/256/cubic (High, current) | 157.7 | 417.1 | -215.5 dB | -0.0017 dB | -189.0 dB | -173.0 dB |
| sinc 256/256/linear | 77.3 | 285.6 | -157.3 dB | -0.0017 dB | -105.3 dB | -173.0 dB |
| sinc 256/512/linear | 82.4 | 299.5 | -169.4 dB | -0.0017 dB | -117.3 dB | -173.0 dB |
| sinc 128/256/cubic (Std) | 83.4 | 331.9 | -208.9 dB | -10.31 dB | -189.2 dB | -103.6 dB |
| **fft 1024/sub2** | **8.8** | **25.9** | **-201.6 dB** | **+0.0000 dB** | **-184.8 dB** | **-184.2 dB** |
| fft 1024/sub4 | 8.9 | 23.9 | -195.8 dB | +0.0000 dB | -179.1 dB | -174.1 dB |

(Probe absolute numbers run ~15% above the repo bench for the same config due to probe harness
overhead; ratios are the signal.)

- Native-stereo (one 2ch instance) was also measured: helps sinc at 44.1→48 (131 vs 158) but
  HURTS at 48→96 (580 vs 417); FFT unchanged. Not a lever; keep mono-per-channel architecture.
- Linear-interpolation sinc halves cost but collapses high-frequency residual to -105/-117 dB —
  off-brand for this crate's evidence (-180+). Rejected.

## Recommendation

Route the rubato backend through `FftFixedIn<f64>` (sub_chunks=2) whenever the reduced ratio is
small (e.g. `from/gcd <= 1024`, true for every real audio pair), keeping `SincFixedIn` as the
fallback for pathological ratios (e.g. 44100→44101, where FFT sizes/latency explode). Expected
result: rubato backend lands at SoXR-level throughput (~9-26 ns/sample) with equal-or-better
fidelity proxies (passband perfectly flat, alias floor better than sinc High).

### Required adapter changes (`rubato_backend.rs`)

1. `FftFixedIn::new(from, to, CHUNK_IN=1024, 2, 1)` per channel; same fixed-chunk FIFO adaptation
   (chunking invariance preserved).
2. **Leading-delay skip**: unlike `SincFixedIn` (pre-compensates its group delay internally,
   output_delay is theoretical), `FftFixedIn` output has a REAL leading delay of
   `output_delay() = fft_size_out/2` frames (320 out-frames for 44.1→48 sub2; 512 for 48→96).
   The adapter must skip exactly that many produced frames at stream start (and after `clear`).
   Drain padding/truncation to `round(total_input * to / from)` then works unchanged.
3. `reset()` exists on FftFixedIn; `clear` must also reset the skip counter.
4. Allocation-free confirmed: FFT scratch preallocated at construction (`make_scratch_vec`),
   `process_into_buffer` body has no allocation; `assert_no_alloc` test must still be run to prove it.
5. Quality mapping: FFT path has no quality knob; tiers keep meaning only for the sinc fallback.
   Document that (like PhaseResponse, which stays accepted-not-applied; FFT is also linear phase).

### Evidence protocol (per quality-guidelines spec)

- Re-run all 27 quick quality gates under `--no-default-features --features rubato`.
- Regenerate `audio_resampler_streaming_perf` quick JSON for rubato; refresh docs/quality.md
  budget row + backend table (report the new algorithm label, e.g. distinguish fft vs sinc path).
- Impulse-alignment and duration tests in the resampler suite are the guard for the delay-skip
  math (peak within ±1 frame, exact output length, bitwise chunking invariance).

## Non-levers / rejected

- Missing SIMD (verified active), adapter FIFO overhead (equal-rate bypass measures 0.099 ns),
  rayon (streaming path is serial by design; offline path already parallel), f32 internal
  processing (violates f64 Hi-Fi contract), FastFixedIn polynomial (quality far too low).
- target-cpu=native / LTO: legitimate but non-portable build-config gains (~10-20%), orthogonal.

## Upstream note

rubato 4.0.0 is on crates.io (major API rework). Not needed for this win — FftFixedIn ships in
0.16.2 — but a later migration evaluation may bring further gains and should re-run this probe.
