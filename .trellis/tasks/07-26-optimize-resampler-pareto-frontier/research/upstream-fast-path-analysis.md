# Upstream Fast-Path Analysis

Date: 2026-07-26

## SoXR interleaved-channel support

The pinned `soxr 0.6.0` Rust crate already exposes native interleaved formats:

* `Stereo<S>` uses `[[S; 2]]` and creates one libsoxr state with two channels.
* `Interleaved<S, const CHANNELS: usize>` provides the same representation for
  compile-time channel counts.
* `Soxr::process` and `drain` measure input/output capacity in frames through
  the format implementation, so no channel-planar adapter is required.

The benchmark raw-libsoxr control proves this exact API on the current build:
it owns `Soxr<Stereo<f64>>`, reinterprets caller-owned interleaved `&[f64]` as
`&[[f64; 2]]`, and processes directly. The reinterpretation is sound because
`[f64; 2]` has the same alignment and contiguous layout as two adjacent `f64`
values and the sample count is validated as even.

The production backend instead owns one `Soxr<Mono<f64>>` per channel. For
stereo it therefore pays for two native states plus input deinterleave,
sequential mono calls, scratch-to-channel copies, and output reinterleave. A
production stereo variant can use the already-proven raw-control representation
without adding a dependency or changing libsoxr parameters.

Recommended first candidate:

1. Add a `StereoBackend` owning `Soxr<Stereo<f64>>`.
2. Store either one stereo backend or the existing mono vector in
   `StreamingResampler`.
3. Route exactly two channels through direct interleaved process/drain; retain
   mono-vector fallback for every other supported channel count.
4. Keep the existing same-rate bypass above the backend.
5. Verify exact output equality with the former per-channel implementation,
   reset/fresh equivalence, arbitrary chunks, constrained output, terminal
   drain, and no steady allocation.

This is not borrowed DSP code: it uses the public upstream multichannel API.
It should bring production setup and steady work close to the strict raw stereo
control while preserving the identical libsoxr recipe.

## Rubato FFT geometry

Rubato 4 computes its synchronous FFT unit for `FixedSync::Input` as:

```text
min_input = input_rate / gcd(input_rate, output_rate)
wanted_subsize = requested_chunk / requested_sub_chunks
fft_chunks = ceil(wanted_subsize / min_input)
fft_size_in = fft_chunks * min_input
fft_size_out = fft_chunks * output_rate / gcd
```

For 44.1 -> 48 kHz, the reduced ratio is 147:160:

| Requested geometry | wanted subsize | FFT input/output |
| --- | ---: | ---: |
| 1024 frames / 2 sub-chunks | 512 | 588 / 640 |
| 512 frames / 1 sub-chunk | 512 | 588 / 640 |
| 512 frames / 2 sub-chunks | 256 | 294 / 320 |

For 48 -> 44.1 kHz the same pair produces 640 / 588 FFT units. Therefore
`512/1` is the first useful High-quality experiment: it aligns the engine input
with the representative 512-frame callback while retaining the same internal
FFT/filter dimensions as the current `1024/2` route. In contrast, `512/2`
halves the filter and is a quality-changing comparison, not wrapper cleanup.

The expected benefits of `512/1` are lower adapter buffering, smaller rings,
earlier opportunities for direct caller input, and cheaper setup allocations.
Core FFT work per sample may remain similar, so throughput improvement must be
measured rather than assumed. Complete-stream bits may differ if Rubato's saved
frame partitioning changes; objective quality and lifecycle evidence, rather
than theoretical geometry alone, is authoritative.

UltraHigh currently uses `1024/1`, yielding a longer filter. It must remain on
that geometry unless a separate candidate proves the unchanged UltraHigh
quality floor. Nonlinear, sinc-fallback, and half-band engines should also keep
their existing chunk geometry during the first linear-FFT experiment.

## Adapter direct-input opportunity

The project Rubato backend has already retained a budget-bounded direct-output
path for non-integer ratios. However, `process` first copies every caller frame
into `in_fifo`, then passes the contiguous FIFO prefix to the engine. The next
adapter-only candidate should process caller memory directly when all of these
conditions hold:

* the input FIFO is empty;
* caller input contains at least one complete engine chunk;
* pending output has been emitted in order;
* output capacity can hold the engine's bounded result, or the existing
  preallocated spill policy can preserve any excess;
* delay and cumulative rational-prefix accounting remain authoritative.

Irregular prefixes and tails continue through the FIFO. This follows the same
common-path/fallback structure used by high-performance streaming codecs and
avoids making caller chunk regularity part of the public contract.

## Comparator lessons not to copy blindly

* FFmpeg's selected default lane is faster but measured only -19.93 dB folded
  alias for 48 -> 44.1 kHz. Its result motivates lower adapter overhead, not a
  silent adoption of its filter recipe.
* WebRTC is competitive in f32 but has materially weaker passband/THD/alias
  response. It is not a strict f64 High-quality target.
* r8brain and raw Rubato demonstrate that FFT/block processing can obtain very
  strong rejection, but they expose different buffering and alignment
  semantics. Their architectural lesson is useful; their benchmark rows are
  not drop-in policy replacements.
* A canonical 147:160 phase-major polyphase/SIMD engine remains a later option.
  It has a higher ceiling than wrapper cleanup but changes DSP output and must
  be justified only after strict adapter/geometry attribution.

## Selected order

1. Native stereo SoXR production path.
2. Strict evidence rerun against raw stereo libsoxr.
3. Rubato `512/1` High linear FFT candidate with `1024/2` control.
4. Direct caller-input Rubato path.
5. Profile-guided canonical-ratio DSP specialization only if a meaningful gap
   remains.

## Source references

* `soxr-0.6.0/src/format.rs` (`Stereo` and `Interleaved`).
* `soxr-0.6.0/src/lib.rs` (`Soxr::new_with_params`, `process`, `drain`).
* `rubato-4.0.0/src/synchro.rs` (`Fft::new_custom` geometry and FFT kernel).
* `benches/resampler_comparison_support/adapters.rs` (raw stereo SoXR and raw
  512-frame Rubato controls).
* `../07-24-optimize-rubato-44k1-to-48-high/research/noninteger-direct-output.md`
  (retained prefix-budget direct-output evidence).
