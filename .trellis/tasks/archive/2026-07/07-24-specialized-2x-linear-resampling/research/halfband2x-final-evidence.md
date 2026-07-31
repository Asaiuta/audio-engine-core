# Exact 2x Linear High Half-Band Final Evidence

## Retained design

`Linear + High + to_rate == 2 * from_rate` now selects a dedicated 127-tap
Kaiser-windowed symmetric half-band interpolator. Every other quality, phase,
direction, and ratio keeps its previous Rubato FFT, sinc, or nonlinear
polyphase route.

The engine processes the existing fixed 1024-frame backend chunk. One output
phase is the delayed source sample; the companion phase uses 32 symmetric
coefficient pairs. Coefficients, transposed channel history, and output staging
are allocated during setup. Construction selects a fixed AVX2+FMA accumulator
function pointer when supported, with a scalar fallback; processing performs no
runtime feature detection and allocates nothing.

The engine is intentionally behind the existing `MonoBackend` adapter. It does
not duplicate the fixed-capacity rings, integer-ratio direct output,
leading-delay skip, caller-visible `emitted` accounting, duration-aligned
drain, terminal state, or reset lifecycle.

## Candidate evolution

All rows are seven-trial quick `process_checked` medians for stereo 48->96 kHz
at 128/256/512 input frames. These intermediate runs are diagnostic rather than
cross-algorithm baselines, but show why the retained block kernel is necessary.

| Candidate | 128 | 256 | 512 | Decision |
| --- | ---: | ---: | ---: | --- |
| 511-tap first half-band | 374.536 | 377.247 | 379.456 | Reject: far slower than FFT |
| 127-tap direct history traversal | 115.833 | 117.688 | 175.191 | Reject: still slower |
| 127-tap mirrored history | 49.656 | 56.346 | 57.716 | Reject |
| 127-tap mirrored/unrolled | 51.010 | 51.112 | 66.711 | Reject |
| 127-tap blockwise scalar | 29.380 | 29.086 | 28.737 | Promising, but misses larger-block target |
| 127-tap blockwise AVX2+FMA | 5.849 | 5.807 | 6.026 | Retain |

The 127-tap order is the smallest evaluated power-of-two-history design that
kept the 0-20 kHz band flat and pushed the 28-48 kHz interpolation image below
the High-quality floor. The AVX2 wrapper and scalar kernel are bit-equal for
vector and remainder lengths in the retained tests.

## Same-revision FFT comparison

Both reports were captured from revision `abc05e4`, on the same Windows
x86_64 / Intel Family 6 Model 154 machine, with the pure-Rubato release build,
75 iterations per trial, seven trials, and work-validity checks enabled.
Changing the algorithm identifier makes the reports intentionally
baseline-incompatible; the values are compared here explicitly as adjacent
task evidence.

| 48->96 `process_checked` | FFT baseline | Half-band final | Improvement |
| --- | ---: | ---: | ---: |
| 128 frames | 36.104 ns/input sample | 5.849 | 83.8% |
| 256 frames | 14.354 ns/input sample | 5.807 | 59.5% |
| 512 frames | 17.667 ns/input sample | 6.026 | 65.9% |

Every measured case reports valid work, exact consumed-frame totals, finite
output, and the expected duration range.

Evidence:

* `resampler-fft-baseline-quick.json`
* `resampler-halfband2x-127tap-block-avx2-quick.json`

## Quality and lifecycle verification

Route-specific tests cover:

* full zero-stuffed convolution across irregular internal chunks;
* coefficient DC gain and 20 kHz passband / 30 kHz image response;
* 20 kHz THD+N plus 28/30 kHz interpolation images through the public
  streaming adapter;
* exact routing scope, including 44.1->88.2 kHz and negative cases for other
  qualities, phases, downsampling, and non-2x ratios;
* native interleaving vs independent mono engines;
* direct-output vs staged-output bit equality;
* arbitrary caller chunking, duration-aligned finish, reset isolation, and
  allocation-free process/finish.

The final pure-Rubato quality report passed all 27 enforced gates with zero
failures or skips. That report deliberately exercises UltraHigh sinc for its
general resampler rows, so the dedicated High half-band assertions above are
the route-specific quality evidence rather than an accidental inference from
the UltraHigh result.

Evidence: `quality-rubato-halfband2x-final-quick.json`.

## Final build matrix

On 2026-07-24 the following completed without warnings or failures:

* `cargo fmt --all -- --check`
* `cargo clippy --all-targets --all-features -- -D warnings`
* `cargo test --all-features`: 351 library, 10 benchmark-support, 3 runtime,
  and 2 doctests passed
* `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`
* `cargo test --no-default-features --features rubato`: 367 library,
  10 benchmark-support, 3 runtime, and 2 doctests passed
