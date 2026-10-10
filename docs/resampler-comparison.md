# Resampler quality and performance comparison

The later [CPU/memory report and fused-FFT optimization](resampler-resources.md)
adds 57 resource configurations and a same-filter production optimization. The
quality evidence and **pre-fusion throughput baseline** below retain their v7
scope; use the linked report for the latest before/after measurements.

The current default `rubato` feature selects the owned `rust` backend. Upstream
Rubato 4.0.0 is an optional control enabled by `raw-rubato`; production code does
not use it. Enabling `soxr` selects SoXR instead. The runner has **11 engine IDs**;
`audio_engine_core` reports the backend selected by each compiled build.

This document uses the repaired **v7 measurements from 2026-10-10**. The earlier
October v6 summaries mixed trial records between engines: 96 of 100 rows and
672 of 1,500 trials were affected. Those reports and the prior documents remain
retained as historical evidence and must not serve as current timing baselines.
The independent R5 complete-waveform and `l5_retest_v1` experiments used different
drivers and retain their original scope; see [replacement evidence](resampler-replacement.md).

## Current evidence and its limits

The two sequential runs exited 0 and retained source, executable, native-library,
command and scheduling identities. An independent reconstruction made 30,803
integrity checks against the raw records and found no errors.

| Evidence | Owned-Rust build | SoXR build |
| --- | ---: | ---: |
| Measured recipes / rate-channel cases | 10 / 48 | 11 / 52 |
| Identified throughput trials | 720 | 780 |
| Individually timed process calls | 192,000 | 208,000 |
| Complete quality signal renders | 2,384 | 2,584 |
| Quality output scalar samples checked | 111,609,704 | 118,651,448 |
| Frequency-response points | 3,328 | 3,536 |
| Alias / image points | 500 / 304 | 550 / 320 |
| Independently excited channel pairs | 448 | 464 |
| Reset/render repetition rounds | 576 | 624 |

Raw libsoxr requires the `soxr` feature and is explicitly unavailable in the
Rust build. Raw libsoxr, FFmpeg and WebRTC adapters support the four stereo cases;
their current adapters do not support the six-channel case. This is adapter
coverage, not a general upstream-library limitation. Every measured case passes
complete-work and finite-output checks. Cross-engine quality and speed are
**report-only**, not an assertion that different filters have equal quality.

The comparison covers High/Linear, 512-frame caller blocks and the five listed
rate/channel cases. It does not cover every rate, quality, phase, amplitude,
channel count or buffer size. A finite frequency grid does not establish a
continuous-band envelope, perceptual transparency or independent analyzer
accuracy. Other package probes and R5 research provide separately scoped
coverage; device/driver/DAC latency belongs to the consuming application.

## Throughput

Cells below are median **nanoseconds per input scalar sample**. Stereo has two
samples per frame. Each trial uses 32 warmup buffers followed by 4,000 timed
process calls; each case has 15 alternating forward/reverse trials. All factories
use the same maximum advertised output capacity within a rate/channel case.
Input preparation, setup, reset, quality analysis and drain are outside steady
timing. Complete input and every drain output still participate in work checks.

| Recipe | 44.1→48 | 48→44.1 | 44.1→96 | 96→44.1 | 44.1→48, 6ch |
| --- | ---: | ---: | ---: | ---: | ---: |
| Owned Rust, High/Linear | 7.559 | 7.029 | 11.438 | 4.919 | 7.945 |
| Raw Rubato, FFT 1024/2 | 7.542 | 7.098 | 11.352 | 5.131 | 8.303 |
| Project SoXR, High/Linear | 6.897 | 6.563 | 9.572 | 3.878 | 8.023 |
| Raw libsoxr, HQ/Bits20 | 6.878 | 6.560 | 9.584 | 3.875 | — |
| FFmpeg SWR, default filter | 4.521 | 4.401 | 9.586 | 4.316 | — |
| libsamplerate, SINC_BEST | 288.589 | 265.508 | 572.578 | 260.944 | 134.844 |
| SpeexDSP, quality 10 | 240.667 | 221.354 | 481.231 | 216.193 | 240.696 |
| r8brain 7.1 | 17.377 | 17.192 | 22.838 | 10.712 | 17.359 |
| zita-resampler 1.11.2 | 24.147 | 22.808 | 44.359 | 21.816 | 20.179 |
| WebRTC v1.3 shim | 6.201 | 5.303 | 11.983 | 2.940 | — |
| WDL 96b770f shim | 35.779 | 30.229 | 71.436 | 15.219 | 32.198 |
| libresample 0.1.5 shim | 25.160 | 31.686 | 49.160 | 27.809 | 28.985 |

The project SoXR and raw-libsoxr rows come from one SoXR build; their median
differences are below 0.3% in all four stereo cases. The remaining rows come from
one Rust build. Comparing absolute Rust and SoXR rows across these two builds is
descriptive, not a paired optimization acceptance. Native f32 lanes
(libsamplerate, SpeexDSP, zita, WebRTC and libresample) and f64 lanes remain
explicit in the raw report and must not be treated as the same precision recipe.

The owned Rust and same-geometry raw Rubato quality grids are exactly equal in
all five cases. Their current common-rate throughput is close; the historical
R5 population speedup is a different measurement over 828 configurations.

The runs pin logical processor 2 and record affinity mask 4, process priority
class 128 and thread priority 2. `QueryThreadCycleTime` is retained alongside
wall time. Its counts are machine/scheduling-specific and are **not** a portable,
frequency-invariant performance unit.

## Why FFmpeg is fast

This measures in-process `libswresample`, not an FFmpeg command line, decoder,
file transfer or process startup. The pinned n8.0.1 source is
`894da5ca7d742e4429ffb2af534fcda0103ef593`. The shim uses packed f64 I/O and SWR's
internal planar f64 path. The build enables x86 assembly, with AVX/FMA3 available
on this host. SWR prepares a rational polyphase coefficient bank during setup
and performs short, vectorized dot products during conversion.

The selected SWR defaults are filter size 32, phase shift 10, exact rational
phases, cutoff 0.97 and Kaiser beta 9. Across the four stereo conversions, the
actual tap counts are **32 / 36 / 32 / 72**, and phase counts are
**160 / 147 / 320 / 147**. Coefficients are reused, the hot work is bounded and
regular, and no large transform is needed for each output block.

The owned High path instead uses real FFT pairs of lengths
**1176/1280, 1280/1176, 1176/2560 and 1280/588**. RustFFT already selects SIMD;
this is not an interpreted-code or missing-vectorization comparison. The long
filter and narrow transition require substantially different work.

The quality difference is visible in the new measurements:

| 48→44.1 kHz metric | Owned Rust High | FFmpeg selected default |
| --- | ---: | ---: |
| Gain at 18 kHz | approximately 0 dB | −0.00961 dB |
| Gain at 19,845 Hz | approximately 0 dB | −1.02350 dB |
| Gain at 20,947 Hz | −0.000000094 dB | −3.95940 dB |
| Output level from 23 kHz input, quadrature pair | −185.34 dB | **−19.93 dB** |
| 997 Hz tone-fit residual / signal | −209.51 dB | −108.68 dB |

The 96→44.1 kHz 23 kHz probe is −179.89 dB for owned Rust and −19.93 dB for
FFmpeg. These are particular measured points, not whole-band rejection bounds.
The old −121.7 dB FFmpeg reverse-alias entry was a miscopied distortion metric.
Increasing SWR filter quality changes its cost; default-filter speed does not
establish speed at the project's current response.

## Can the owned backend reach that speed without losing quality?

| Stereo conversion | Owned Rust / FFmpeg time | Reduction needed to match FFmpeg |
| --- | ---: | ---: |
| 44k1 → 48k | 1.672× | 40.2% |
| 48k → 44k1 | 1.597× | 37.4% |
| 44k1 → 96k | 1.193× | 16.2% |
| 96k → 44k1 | 1.140× | 12.3% |

The current implementation has not demonstrated that result across all four
conversions. SoXR High already approaches or beats the selected FFmpeg recipe
in two cases, but its bandwidth and residuals differ from owned Rust High;
selecting it is not proof of quality nonregression against the owned backend.

Windows CPU sampling was rejected by local profiling policy (`0xc5585011`). A
separate, sealed source-instrumented copy therefore measured the unchanged FFT
path for four seconds per stereo case, outside all throughput trials:

| Work inside process | Fraction of instrumented process time |
| --- | ---: |
| Forward and inverse FFT together | 67.4–70.8% |
| Spectrum multiplication and zero fill | 7.4–10.5% |
| Input deinterleave/zero and output overlap/interleave | 12.6–13.0% |
| Other work, including instrumentation overhead | 7.9–11.0% |

The timings include timer overhead; 10,000 timer-read observations per case are
retained. They diagnose costs and are not optimization acceptance results.
Removing a pending-output copy alone cannot close the two largest gaps. A major
improvement needs faster FFT work, a different block/filter architecture, or both.
The data does not prove this impossible, and it does not establish an eligible
new implementation. The production filter and geometry were retained in this
measurement repair.

Future replacements may differ sample by sample from Rubato. Their own identity
must freeze paired passband error, usable bandwidth, alias/image response,
phase/timeline, full-stream transients and numerical resolution against the
current implementation before scoring. A quality win at one frequency, a broad
audibility threshold, or a faster default preset cannot replace nonregression
across those criteria. Streaming duration, startup, allocations and process
tails remain separate acceptance requirements.

## Metric definitions

`steady_ns_per_input_sample.p95` is the p95 of 15 trial averages; it is not a callback tail.
`process_call_latency` comes from a separate instrumented pass and retains every
duration and output count, p50/p95/p99/p99.9/max, timer observations and exact
complete output. It includes timer overhead and excludes setup/reset/drain and
device scheduling. For example, FFmpeg's 48→44.1 kHz median throughput barely
changes between the two runs, while observed individual-call maxima differ
from 52.2 to 778.2 microseconds. Stable throughput is not a hard deadline guarantee.

Startup distinguishes the API's compensated buffering report, the cursor before
the producing call, input consumed through that call, and whether first output
occurred during drain. At 512-frame calls, owned Rust first produces output by
the call that brings total consumed input to 1,024 frames; FFmpeg does so by
512. SoXR reaches 1,024 or 2,048 depending on the conversion. These are caller-
block observations, not exact internal lookahead or device/group-delay values.

The quality grid measures 13 response frequencies for each input quadrature
phase and output channel: 20, 100, 997, 5k, 10k, 15k, 18k Hz plus 85/90/95/98/99%
of common Nyquist and Nyquist−1 Hz. Both downsampling directions include a
stopband grid, exact output/input Nyquist, and legal exact/near-DC folds.
Upsampling image fits jointly model the fundamental and image. Channel
isolation excites each source channel independently. DC and both step polarities
are checked, including complete-stream peaks and ending transients.

Tone generation uses bounded integer-Hz rational phase, and fitting uses
compensated sums. The analysis guard is capped at one quarter of usable output;
the old 96→44.1 kHz window could shrink to just 32 frames. Each point retains its
window and residual metadata. Analytic/quantized analyzer self-controls are not
an external accuracy bound. The −400 dB display floor is not analyzer resolution.

The optional `--soak-rounds 12` check uses a persistent adapter per case, reset
between rounds. Every round processes 32 warmup plus 64 regular input calls and
all drain chunks. Actual sample-bit vectors are compared; digests accompany,
rather than replace, the comparison. The two repetition phases lasted only
7.19 and 7.34 seconds in total. Working set is report-only; this is not a
long-duration leak test or complete accounting of native C/C++ allocations.

## Integrity and reproduction

Every trial carries engine, algorithm, sample format, rate pair, channel count,
input chunk, output capacity and trial index. Accumulators use stable factory
IDs even when execution order reverses. Missing, duplicate, wrong-identity and
inconsistent records fail. Published statistics and work counters are rebuilt
from raw records; `valid: true` cannot override corruption. JSON reload uses
`serde_json/float_roundtrip` and exact floating-bit validation. This subsequent
reader fix changes no DSP arithmetic or timed conversion work in the retained
v7 runs; its own regression and integration checks are recorded separately.
Later feature-matrix checks also repaired missing `raw-rubato` guards in adapter
dispatch arms; both heavy builds already enabled that feature.

The ignored seven-shim fixture in the SoXR-linked test harness completed its
assertions but stalled during Windows process teardown. Its incomplete exits
remain recorded separately from the normally exiting heavy main executables
and ordinary tests. The specific DLL/runtime cause has not been established.

libsamplerate receives end-of-input on the final call containing real samples.
The v4 adapter uses the pinned 0.2.2 source's floating clock and native endpoint
rule, restricted to the supported 2/6-channel matrix. Ceil, rounding and a
one-frame tolerance do not reproduce that rule. Native observations cover 5,416
stereo/six-channel streams; the tested mono model disagreed once and was not
generalized. Nothing is padded or trimmed to manufacture matching lengths.

Build native controls with `benches/native/build_resampler_shims.ps1`. Pass the
libsamplerate DLL through `--libsamplerate <absolute-path>` and each shim through
`--engine-library <engine-id>=<absolute-path>`. Libraries are not discovered by
searching PATH. Record source/build/runtime artifacts and hashes from each run.

```text
cargo bench --bench audio_resampler_comparison_perf --features raw-rubato --   --heavy --enforce --pinned --pin-core 2 --soak-rounds 12   --out target/bench-reports/rust-new.json <explicit-native-arguments>

cargo bench --bench audio_resampler_comparison_perf --all-features --   --heavy --enforce --pinned --pin-core 2 --soak-rounds 12   --require-complete-matrix   --out target/bench-reports/soxr-new.json <explicit-native-arguments>
```

Run feature builds and timing sequentially. Use unique evidence paths and retain
failures. The all-feature command requires every declared engine; the Rust build
can require each of its ten expected engines explicitly. Optional
`--raw-rubato-geometry 512/1` is a different control recipe, not the High/1024/2
reference used above. There is no claim of native ARM/Linux runtime performance.

Local evidence is retained under
`.trellis/tasks/10-08-rubato-dependency-audit/research/comparison-repair-v1/`:

- `results/{rust,soxr}-heavy-v7.json`: all measurements and raw records.
- `results/integrity-audit-v1.json`: reconstructed statistics and denominators.
- `binaries/{rust,soxr}/freeze.json`: exact commands, source/native/binary hashes.
- `profile/instrumented-v2/measurement/`: diagnostic stages, raw counters and scope.
- `runs/`: successful and failed command requests, logs and actual exit codes.

Rust executable SHA-256:
`8812e1ab8e58e15a66f32a393ff3a8c348bc2722eedf4789589d7d748a3ef858`.
SoXR executable SHA-256:
`037eb6c51de08b58102f82570c6a9b36a80c156265b65addd1038c0c4eeea088`.
