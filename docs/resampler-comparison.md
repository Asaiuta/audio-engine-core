# Resampler Comparison & Pareto Analysis

> Comparative study of audio resampling engines and recipes evaluated for
> `audio-engine-core`.

This document is the specialized third layer of the project documentation
chain. The general quality contracts, CI gates, and the crate's own
representative evidence live in [`docs/quality.md`](quality.md); the README
summarizes the results that matter for users. This document answers the
remaining question: **what do the cross-engine measurements actually mean?**

It is not a universal ranking of resamplers.

Different engines expose different quality targets, phase responses, latency
policies, buffering models, SIMD strategies, and implementation constraints.
A result that is faster in one workload may be substantially worse in another.
The goal of this study is therefore to identify **Pareto-relevant trade-offs**
across signal quality, throughput, streaming latency, setup/lifecycle cost,
phase/frequency-response behavior, channel and buffer handling, and
implementation constraints — not to crown a single winner.

---

## Executive Summary

Representative 2026-07-27 pinned-heavy results (44.1→48 kHz unless stated;
steady cells are median / p95 ns/input-sample; lifecycle pairs are
44.1-to-48 / 48-to-44.1 in microseconds):

| Configuration / family | Steady 44.1→48 | THD+N (study probe) | Reverse alias 48→44.1 | API latency | Main trade-off |
| --- | ---: | ---: | ---: | ---: | --- |
| audio-engine-core SoXR v2 (`soxr` default) | 8.569 | −134.31 dB | −137.67 dB | 0 frames | Native dependency (LGPL-2.1); excellent general-purpose balance |
| audio-engine-core Rubato v17 (`rubato`) | 8.182 | −200.76 dB | −232.81 dB | 0 frames | Pure Rust, no native dependency; strongest measured stopband |
| raw libsoxr (HQ/Bits20) | 8.632 | −134.31 dB | −137.67 dB | n/a | Upstream control for SoXR |
| raw Rubato 1024/2 | 9.082 | −200.76 dB | −232.81 dB | 0 frames | Upstream control for Rubato (supplement run) |
| FFmpeg libswresample (selected recipe) | **5.798** | −106.56 dB | **−19.93 dB** | n/a | Fastest steady throughput; quality depends strongly on recipe |
| WebRTC (selected recipe) | 8.206 | −89.60 dB | −42.87 dB | 17 frames | Fast; voice-oriented frequency response (−0.66 dB at 18 kHz) |
| libsamplerate (SRC_SINC_BEST_QUALITY) | 384.294 | −149.43 dB | −163.66 dB | n/a | Best-known general route; ~45× the project's steady cost |
| r8brain | 22.673 | −180.54 dB | −213.63 dB | 1851 frames | Excellent measured rejection; high buffering latency |
| zita-resampler | 33.856 | −144.73 dB | −114.82 dB | 104 frames | High setup cost (2,084.6 μs) |
| SpeexDSP | 318.558 | −136.63 dB | −123.40 dB | 139 frames | Voice-grade DSP quality-10 route |
| WDL | 47.830 | −121.27 dB | −32.76 dB | 34 frames | Weak stopband for this recipe |
| libresample | 37.778 | −79.23 dB | −78.88 dB | 30 frames | High-quality label; weakest measured THD+N here |

Project-internal conclusion (same-run, same-recipe controls only): SoXR v2 is
statistically tied with raw libsoxr in both directions (−0.73% / +0.75%);
Rubato v17 wins 44.1→48 (−4.77%) and ties 48→44.1 (+1.70%) against its
same-geometry raw control. **Neither backend is "the default because it is
fastest"** — see [SoXR vs Rubato](#soxr-vs-rubato).

---

## 1. Scope & Comparison Philosophy

### What is being compared

`audio_resampler_comparison_perf` compares reusable streaming sample-rate
conversion engines under one benchmark-owned lifecycle: the same stereo
512-input-frame schedule, the same quality probes, and the same latency,
lifecycle, and provenance gates for every engine. It is a
quality/latency/throughput Pareto report. No adapter changes the production
resampler selection or ordinary linkage.

### What is not being claimed

- No universal fastest-engine ranking exists in this study.
- No cross-machine absolute timing claim is made: this is one Windows machine,
  same-machine evidence.
- No perceptual listening conclusion follows from the objective probes.
- A row that is faster here may be slower under a different recipe, lane,
  ratio, or latency policy.

### Engine vs recipe

A comparison row identifies an **engine + recipe + configuration**, not merely
an engine name. "FFmpeg", "Rubato", or "SoXR" alone is not a benchmark
identity: the same engine with a different quality preset produced materially
different stopband and latency results in the evaluated set.

**Engine ≠ Recipe** is a standing principle of this study. Every row is
therefore reported with its adapter/recipe (see
[Compared Engines & Recipes](#2-compared-engines--recipes)), and results are
only compared across rows that share the same lane, recipe intent, and
measurement conditions unless the limitation is stated.

---

## 2. Compared Engines & Recipes

The selected representative matrix has 11 engine IDs. The primary evidence
run measured both 44.1-to-48 kHz and 48-to-44.1 kHz for every row.

| Engine ID | Adapter / recipe | Lane |
| --- | --- | --- |
| `audio_engine_core` | Public `StreamingResampler`, High, Linear | interleaved `f64` |
| `raw_libsoxr` | libsoxr HQ/Bits20, linear phase, one thread | interleaved `f64` |
| `raw_rubato` | Rubato FFT, BlackmanHarris2, two sub-chunks | interleaved `f64` |
| `libsamplerate` | `SRC_SINC_BEST_QUALITY` | interleaved `f32` |
| `ffmpeg_libswresample` | SWR defaults, filter size 32, exact rational | interleaved `f64` |
| `speexdsp` | SpeexDSP quality 10 | interleaved `f32` |
| `r8brain` | `CDSPResampler24`, 2% transition, 180.15 dB request | interleaved `f64` |
| `zita_resampler` | zita `Resampler`, half length 96 | interleaved `f32` |
| `webrtc` | `PushResampler<float>`, 10 ms staged sinc blocks | interleaved `f32` |
| `wdl` | `WDL_Resampler`, sinc mode, feed mode | interleaved `f64` |
| `libresample` | high-quality `resample_process` | interleaved `f32` |

Every report contains a machine-readable `coverage` table with exactly these
11 engine IDs. A row is terminal only when it is `measured`,
`not_comparable`, or `infeasible_with_evidence`; `unavailable` is explicitly
non-terminal. `--require-complete-matrix` writes requested JSON first and then
fails if any row is non-terminal. A measured row must be backed by both rate
case keys, and the report reconstructs and validates the table from actual case
and unavailable rows.

---

## 3. Methodology

### Test matrix and workload

Every engine receives the same stereo 512-input-frame schedule. The probe
records setup, steady processing, reset, drain, exact consumption and output
length, finite output, native/API latency, complete-stream impulse peak, 997 Hz
gain and THD+N, 18 kHz gain, and the folded 23 kHz alias for downsampling.
Every timed trial reconciles warm-up, steady, and drain output against the
adapter's exact complete-stream contract. Every factory also proves reset
output is bit-for-bit identical to a fresh instance before timing. Quality
validation rejects silence and non-analyzable signals rather than flooring
undefined values into an apparent pass.

Full mode uses 32 warm-up buffers, 1,000 timed buffers per trial, 11
alternating forward/reverse trials, and 16,384 input frames per quality signal.
Heavy mode raises the timed work to 4,000 buffers per trial and 15 trials while
keeping the same warm-up and quality signal. Steady throughput is reported as
nanoseconds per input sample. Setup, reset, and drain are timed separately.

Cross-engine timing and quality are report-only; only compatible same-engine
baselines may become regression gates. Quality-gate semantics, baseline
schema, and CI enforcement follow [`docs/quality.md`](quality.md) and are not
repeated here.

### Statistical treatment

The project convention requires at least a **2% median advantage or separated
trial distributions** for a faster claim; anything else is "tied". Lifecycle
pairs are reported as 44.1-to-48 / 48-to-44.1. Steady cells report
median / p95. Pinned heavy runs verify core affinity and raised
process/thread priority; interrupts, frequency changes, and background load
remain visible in the raw distributions.

### Provenance

Native libraries are loaded only from explicit paths. The report records the
canonical path, upstream version, shim/source revision, build provenance,
SHA-256, size, sample lane, and linked runtime artifacts. Acquired binaries
stay under ignored `target/benchmark-deps/` storage. Formal complete-matrix
runs reject unverified native provenance. API buffering latency, input consumed
before first output, and complete-stream impulse alignment are separate fields.

---

## 4. Quality Comparison

The study's objective-response rows use its own 997 Hz / 18 kHz / 23 kHz
probes (least-squares sine fit, 16,384 input frames). These are **not
row-for-row interchangeable** with the crate's quality-gate numbers in
`docs/quality.md` (e.g. the SoXR 44.1→48 THD+N of −187.0 dB quoted in the
README): the two suites are independent detectors and workloads. What the
study probe does is keep one identical detector across all 11 engines, which
is what makes the cross-engine comparison valid.

### THD+N and frequency response

| Configuration | 18 kHz gain dB (44.1→48 / 48→44.1) | THD+N dB (44.1→48 / 48→44.1) | Reverse alias dB |
| --- | ---: | ---: | ---: |
| audio-engine-core SoXR v2 | −0.00172 / −0.00218 | −134.31 / −134.38 | −137.67 |
| audio-engine-core Rubato v17 (supplement) | 0.00000 / 0.00000 | −200.76 / −209.82 | −232.81 |
| raw libsoxr | −0.00172 / −0.00218 | −134.31 / −134.38 | −137.67 |
| raw Rubato 1024/2 | 0.00000 / 0.00000 | −200.76 / −209.82 | −232.81 |
| libsamplerate | −0.00000 / −0.00000 | −149.43 / −145.61 | −163.66 |
| FFmpeg libswresample | 0.00015 / −0.00961 | −106.56 / −108.69 | −19.93 |
| SpeexDSP | −0.00001 / −0.00001 | −136.63 / −137.78 | −123.40 |
| r8brain | 0.00000 / 0.00000 | −180.54 / −181.11 | −213.63 |
| zita-resampler | −0.00001 / 0.00000 | −144.73 / −145.07 | −114.82 |
| WebRTC | −0.65685 / −0.83634 | −89.60 / −91.54 | −42.87 |
| WDL | −0.00444 / −0.00394 | −121.27 / −123.36 | −32.76 |
| libresample | −0.22695 / −0.22696 | −79.23 / −76.66 | −78.88 |

Alias is measured only in the reverse downsampling case (48→44.1 kHz).

Observed clustering (same study probe, same workload):

- **Greenfield pure-Rust route** (Rubato v17): flat 18 kHz response, THD+N
  ≈ −201/−210 dB, reverse alias −232.81 dB — the strongest measured stopband
  in the matrix.
- **mature native routes** (libsoxr, libsamplerate, r8brain, SpeexDSP, zita):
  THD+N between ≈ −135 and −181 dB, alias between ≈ −114 and −214 dB; r8brain
  is the notable native high-rejection case (alias −213.63 dB).
- **multimedia/voice-oriented recipes** (FFmpeg selected recipe, WebRTC, WDL,
  libresample): weak measured stopband (−19.93 to −78.88 dB) and, for WebRTC
  specifically, a −0.66/−0.84 dB 18 kHz shelf that is a deliberate voice-band
  trade-off.

### Phase / response behavior

- **Same-result identity**: reset output is bit-for-bit identical to a fresh
  instance for every factory before timing, so phase behavior is stable across
  lifecycle transitions in this harness.
- **Latency phases differ by design**: r8brain, zita, WDL, and libresample
  show non-zero API/buffering latency with a frame-zero complete-stream
  impulse peak — their file-aligned adapters pre-roll or internally consume
  lookahead, so wall-clock output availability and output-file sample
  alignment are different quantities. SpeexDSP (139/129) and WebRTC (18/15)
  also carry non-zero impulse-peak frames matching their buffering latency.
- The project routes (SoXR, Rubato) report 0 API latency frames and a
  frame-zero impulse peak on the complete stream: buffer-aligned, no hidden
  pre-roll.

---

## 5. Performance Comparison

### Steady-state throughput and lifecycle

2026-07-27 pinned-heavy run (see [Reproduction](#12-reproduction)). The table
expands `audio_engine_core` into its two production backend configurations,
so it has 12 configuration rows but still represents the same 11 engine IDs.
All rows except `audio-engine-core Rubato v17` come from the single complete
primary run. Within a steady cell, values are median / p95 in
ns/input-sample. Lifecycle pairs are 44.1-to-48 / 48-to-44.1 in microseconds.

| Configuration | Lane | Steady 44.1→48 | Steady 48→44.1 | Setup μs | Reset μs | Drain μs |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| audio-engine-core SoXR v2 | f64 | 8.569 / 10.775 | 7.424 / 10.815 | 234.6 / 255.6 | 256.2 / 266.9 | 98.3 / 94.3 |
| audio-engine-core Rubato v17 (supplement) | f64 | 8.182 / 13.696 | 7.025 / 8.336 | 139.5 / 128.2 | 1.9 / 2.0 | 19.4 / 17.6 |
| raw libsoxr | f64 | 8.632 / 10.158 | 7.368 / 12.505 | 255.9 / 284.2 | 263.7 / 284.3 | 102.1 / 103.7 |
| raw Rubato 1024/2 | f64 | 9.082 / 11.168 | 7.859 / 12.940 | 172.3 / 158.6 | 1.6 / 1.5 | 20.8 / 19.5 |
| libsamplerate | f32 | 384.294 / 456.832 | 307.736 / 454.858 | 224.1 / 197.5 | 58.8 / 49.7 | 0.6 / 0.5 |
| FFmpeg libswresample | f64 | 5.798 / 8.315 | 5.246 / 7.003 | 112.1 / 414.7 | 148.9 / 104.0 | 2.2 / 2.2 |
| SpeexDSP | f32 | 318.558 / 369.372 | 274.130 / 335.916 | 532.0 / 536.1 | 503.7 / 562.1 | 72.4 / 71.1 |
| r8brain | f64 | 22.673 / 27.744 | 22.076 / 28.256 | 22.3 / 23.3 | 0.9 / 0.9 | 107.4 / 97.6 |
| zita-resampler | f32 | 33.856 / 38.312 | 28.843 / 40.746 | 2084.6 / 2164.3 | 0.7 / 0.8 | 6.8 / 9.1 |
| WebRTC | f32 | 8.206 / 10.879 | 7.045 / 9.973 | 314.4 / 318.8 | 319.5 / 312.9 | 8.0 / 7.7 |
| WDL | f64 | 47.830 / 54.607 | 40.754 / 48.591 | 199.6 / 197.8 | 0.4 / 0.4 | 8.3 / 8.1 |
| libresample | f32 | 37.778 / 41.072 | 41.055 / 50.621 | 13167.9 / 12544.7 | 13305.5 / 12859.4 | 37.5 / 45.5 |

### Strict same-run, same-recipe controls

The raw Rubato row in the table above belongs to the complete primary run so
the 11-engine matrix remains a single-run comparison. The strict project/raw
Rubato control must instead use the raw row from the Rubato-only supplement.
These are the only same-run, same-recipe adapter conclusions:

| Pair | Direction | Project median | Raw median | Project delta | Verdict | Project / raw p95 |
| --- | --- | ---: | ---: | --- | --- | ---: |
| SoXR v2 / raw libsoxr | 44.1→48 | 8.569 | 8.632 | −0.73% | tied | 10.775 / 10.158 |
| SoXR v2 / raw libsoxr | 48→44.1 | 7.424 | 7.368 | +0.75% | tied | 10.815 / 12.505 |
| Rubato v17 / raw Rubato 1024/2 | 44.1→48 | 8.182 | 8.592 | −4.77% | project faster | 13.696 / 14.226 |
| Rubato v17 / raw Rubato 1024/2 | 48→44.1 | 7.025 | 6.908 | +1.70% | tied | 8.336 / 8.599 |

SoXR v2 is statistically tied with raw libsoxr in both directions; Rubato v17
wins forward and ties reverse against its same-geometry raw control. The
Rubato project p95 is about 3% to 4% lower in both directions, but that does
not turn the reverse median into a win.

Absolute timing across the two separately compiled reports is informative but
is not a controlled AB comparison.

---

## 6. Streaming & Lifecycle

- **Consumed / produced accounting**: every timed trial reconciles warm-up,
  steady, and drain output against the adapter's exact complete-stream
  contract; exact consumption and output length are recorded per case.
- **Partial input / buffering**: `input-consumed-before-first-output` and
  `api_buffering_latency_frames` are separate fields. r8brain consumes 1536
  input frames before first output (1851/1533 API latency frames); SpeexDSP
  and WebRTC buffer 139/129 and 17/15 frames respectively; the project routes
  and libsamplerate report frame-zero first output.
- **Reset**: bit-for-bit identical to a fresh instance for every factory;
  project Rubato reset cost is 1.9 μs vs 256.2 μs for the native SoXR adapter
  and 503.7 μs for SpeexDSP.
- **Finish / drain**: drain is timed separately and is not implied by steady
  throughput; e.g. libresample drains in 37.5–45.5 μs while its steady cost is
  ~38 ns/input-sample, and FFmpeg drains in 2.2 μs.
- **Channel handling**: the primary matrix is stereo interleaved `f64` (native
  routes) or `f32` (voice/multimedia routes); lane is recorded per row and is
  part of the comparison identity.
- **Backend identity in evidence**: benchmark reports record the compiled
  backend in the environment `features` field (`resampler-soxr` /
  `resampler-rubato`) and in `algorithm` labels, so a SoXR lifecycle report
  cannot be compared with a Rubato report.

---

## 7. Route Specialization

The largest within-backend differences in this study do not come from choosing
a library — they come from **choosing a route inside the same backend**.
Rubato's pure-Rust adapter routes ratios to specialized engines:

```text
Generic path
     │
     ├── arbitrary ratios      → FFT engine (synchronous, 2 sub-chunks;
     │                            UltraHigh: 1 longer sub-chunk)
Known exact 2×                 → 127-tap symmetric half-band FIR
     │                            (Linear + High)
Pathological reduced ratios    → windowed sinc
Minimum / Maximum phases       → exact spectral rational resampler
                                  (real-cepstrum factorization, ≤ 1024
                                  reduced components)
```

Measured route effects (same-machine paired quick matrices,
512-frame stereo `process_checked`, `audio_resampler_matrix_perf`):

| Optimization | Before → After | Improvement |
| --- | ---: | --- |
| 48→96 half-band route (128/256/512-frame medians, ns/input-sample) | 36.104 / 14.354 / 17.667 → 5.849 / 5.807 / 6.026 | 83.8% / 59.5% / 65.9% |
| UltraHigh Linear onto one-sub-chunk FFT, 44.1→48 | 101.13 → 8.15 ns/input-sample | ~12.4× |
| UltraHigh Linear onto one-sub-chunk FFT, 48→96 | 163.87 → 10.22 ns/input-sample | ~16× |
| Median setup (44.1→48 / 48→96) | 5.82 / 6.26 ms → 0.16 / 0.20 ms | ~36× / ~31× |
| High/Minimum spectral route, 48→96 | 1098.5 → 15.5 ns/input-sample | ~71× |
| High/Minimum spectral route, 44.1→48 | 604.0 → 191.4 ns/input-sample | ~3.2× |

No Linear-path case regressed beyond run noise in either matrix. The spectral
route matches the previous time-domain polyphase convolution to < 1e-9 across
ratios, tiers, and both phases.

**Conclusion**: a resampler backend is not a scalar; its Pareto position
depends on which route the ratio selects. This is why
[Route selection in the project](#9-soxr-vs-rubato) matters more than a single
median.

---

## 8. Pareto Analysis

### Why throughput alone is insufficient

FFmpeg (5.798 ns/input-sample) and WebRTC (8.206) are faster than both project
backends on parts of this workload, but their selected recipes measured only
−19.93 dB and −42.87 dB reverse alias attenuation; the Rubato route measured
−232.81 dB — over 100 dB better. A resampler is not a scalar performance
number; it is a point in a quality × speed × latency × phase × lifecycle
space, and "fastest" is only meaningful inside an equal-quality class.

### Quality × throughput

Two quality/preference clusters are visible in the matrix:

- **High-rejection work**: r8brain (alias −213.63 dB) and Rubato routes
  (−202.86 to −232.81 dB) dominate; r8brain pays 1851 frames of API latency
  and ~22.7 ns/input-sample, Rubato pays none of that latency and
  ~8.2 ns/input-sample.
- **High-throughput work**: FFmpeg selected recipe and WebRTC lead steady
  throughput; their measured stopband under the selected recipes is ~110 dB
  (alias) weaker than the Rubato route's.

The project backends sit in the high-rejection cluster with competitive
throughput, which is the Pareto-relevant region for a music player.

### General-purpose vs specialized routes

General-purpose native routes (libsoxr, libsamplerate) are mature and
predictable; specialized routes (Rubato half-band/FFT/spectral) show that
knowing the ratio in advance buys order-of-magnitude cost reductions within
one backend. The same trade-off structure applies to FFmpeg's filter
selection: its quality is recipe-dependent, not engine-fixed.

---

## 9. SoXR vs Rubato

The two production backends answer different design constraints; neither is
"the default because it is fastest".

**SoXR (default).** Chosen as the general-purpose native route:

- mature native implementation (SoX heritage), strong quality/performance
  balance across arbitrary ratios,
- predictable streaming behavior with the adapter's exact complete-stream
  contract,
- the full matrix's strict control shows it is statistically tied with raw
  libsoxr (−0.73% / +0.75%), i.e. the adapter adds no measurable overhead
  beyond a clean wrapper.

**Rubato (optional, pure Rust).** Exists for a different reason:

- pure Rust, no native dependency, no LGPL obligation,
- strongest measured stopband (THD+N ≈ −201/−210 dB; reverse alias
  −232.81 dB),
- source-level control enables the route specialization of
  [Section 7](#7-route-specialization) — the same engine, 12.4×–71× faster on
  specialized ratios than its generic predecessor routes,
- slightly faster than its raw control forward (−4.77%), tied reverse.

**Where each route is preferable:** choose SoXR for maximum compatibility and
maturity with a native toolchain available; choose Rubato for
no-native-dependency builds, offline rendering (where its UltraHigh stopband
and 0.73% realtime factor matter), or ratios/geometries that hit the
specialized routes. When both features are enabled, `soxr` wins — matching
the default-features configuration shipped to users.

---

## 10. Engineering Evolution

These records are kept here, not in `quality.md`, because they document how
engineering changed the Pareto position — not what the current position is.

### SoXR v2 (interleaved native state)

Stereo SoXR v2 replaces two native mono states plus
deinterleave/reinterleave scratch with one native interleaved state. Against
the former project route, steady/setup/reset/drain improved by
**56.16 / 44.45 / 47.73 / 58.28%** for 44.1-to-48 kHz and
**39.42 / 33.68 / 44.30 / 49.69%** for 48-to-44.1 kHz. Five pinned 15-trial
heavy reports compare v2 with raw stereo libsoxr under the same f64 HQ/Bits20
recipe. The median of the five per-run deltas is +1.73% forward and −1.94%
reverse, so both directions are classified as statistically tied rather than a
universal win. The reports retain one +11.11% reverse steady outlier, one
+4.27% forward outlier, and one +6.40% reverse drain outlier; none repeated in
the adjacent confirmations. Median-of-run setup/reset/drain deltas remain
between −3.48% and +0.70%.

The final complete 11-engine confirmation measured SoXR v2 / raw libsoxr at
8.569 / 8.632 ns/input-sample forward and 7.424 / 7.368 reverse (deltas
−0.73% / +0.75%), preserving the tied conclusion. All 22 cases were valid, the
11 coverage rows were terminal, and `run_failures` was empty.

### Rubato v17 (adapter + route work)

Rubato v17 retains 1024/2 High geometry while removing adapter work through
bulk channel copies, split FIFO/caller input, partial-zero FFT drain, and
direct terminal truncation. In the final-source public heavy confirmation, all
16 canonical rate/API/caller medians improved **12.35% to 36.83%** against the
matched v12 A2 report; the least favorable p95 delta was +2.67%. A same-run
strict 1024/2 control measures project/raw medians of 8.182/8.592
ns/input-sample forward and 7.025/6.908 reverse: **4.77% faster forward, 1.70%
slower reverse** (forward win, reverse tie by the 2% rule); project/raw p95 is
13.696/14.226 forward and 8.336/8.599 reverse. The first final-source public
heavy run is also retained: system-wide frequency variation on the Balanced
Windows plan made several rows fail the v12 threshold before the immediate
confirmation passed. Raw trials and all reports remain under the Pareto task's
`research/` directory.

### Offline render consequence

`OutputRenderChain` deliberately requests UltraHigh; since the 2026-07-25
routing change, pure-Rust resampled offline rendering uses the one-sub-chunk
FFT route instead of sinc. In the 2026-07-25 quick probe, the active
44.1-to-48 kHz 4096-frame render measured 103.02 ns/input sample for a
one-second input and 82.92 for five seconds (0.91% and 0.73% realtime
factors), compared with 353.97 and 266.38 (3.12% and 2.35%) for the retired
UltraHigh sinc route in the 2026-07-22 quick probe.

---

## 11. Findings

1. The project adapters add no measurable overhead: SoXR v2 is statistically
   tied with raw libsoxr in both directions; Rubato v17 is faster forward and
   tied reverse against its same-geometry raw control.
2. "Fastest" is meaningless without a quality class: the fastest rows here
   (FFmpeg selected recipe, WebRTC) measured the weakest stopband; the
   strongest stopband (Rubato) is also among the fastest steady rows.
3. Route specialization is the largest controllable lever: 12.4×–71×
   improvements within one backend through ratio-aware routing, with no
   quality regression and zero Linear-path noise regressions.
4. Latency policy and phase behavior are first-class dimensions: r8brain's
   rejection comes with 1851 frames of API latency; the project routes
   advertise 0 latency frames and frame-zero impulse peaks.
5. Recipe identity is mandatory: same engine, different recipe → different
   Pareto point. Every conclusion above is scoped to the exact recipe in
   [Section 2](#2-compared-engines--recipes).

---

## 12. Reproduction

The source-integrated shims are rebuilt with pinned sources and MinGW-w64 GCC
15.2.0 by:

```powershell
.\benches\native\build_resampler_shims.ps1
```

The primary full run uses all Cargo features so both raw upstream controls are
compiled. The environment overrides make revision and dirty state explicit.

```powershell
$env:AUDIO_BENCH_REVISION = '342fd447c4c92025c86497b3cfb0d729559046ab'
$env:AUDIO_BENCH_DIRTY = 'true'
$shimDir = (Resolve-Path 'target/benchmark-deps/build/resampler-shims').Path
$srcDll = (Resolve-Path 'target/benchmark-deps/libsamplerate-0.2.2-1/mingw64/bin/libsamplerate-0.dll').Path
$shimArgs = @(
  '--engine-library'; "ffmpeg_libswresample=$shimDir\ffmpeg_libswresample_shim.dll"
  '--engine-library'; "speexdsp=$shimDir\speexdsp_shim.dll"
  '--engine-library'; "r8brain=$shimDir\r8brain_shim.dll"
  '--engine-library'; "zita_resampler=$shimDir\zita_resampler_shim.dll"
  '--engine-library'; "webrtc=$shimDir\webrtc_shim.dll"
  '--engine-library'; "wdl=$shimDir\wdl_shim.dll"
  '--engine-library'; "libresample=$shimDir\libresample_shim.dll"
)

cargo bench --bench audio_resampler_comparison_perf --all-features -- `
  --enforce --require-complete-matrix --libsamplerate $srcDll @shimArgs `
  --out .trellis/tasks/archive/2026-07/07-26-cross-project-audio-benchmarks/research/resampler-comparison-representative-11-full-20260726.json
```

The 2026-07-27 confirmation pins both heavy runs to logical core 2. The
primary all-feature build supplies the complete 11-engine matrix; the
Rubato-only build supplies the alternate production backend and intentionally
cannot run the raw libsoxr control.

```powershell
cargo bench --bench audio_resampler_comparison_perf --all-features -- `
  --heavy --pinned --pin-core 2 --enforce --require-complete-matrix `
  --raw-rubato-geometry 1024/2 `
  --libsamplerate $srcDll @shimArgs `
  --out .trellis/tasks/archive/2026-07/07-26-optimize-resampler-pareto-frontier/research/resampler-comparison-latest-soxr-v2-11-pinned-heavy-20260727.json

cargo bench --bench audio_resampler_comparison_perf `
  --no-default-features --features rubato -- `
  --heavy --pinned --pin-core 2 --enforce `
  --raw-rubato-geometry 1024/2 `
  --libsamplerate $srcDll @shimArgs `
  --out .trellis/tasks/archive/2026-07/07-26-optimize-resampler-pareto-frontier/research/resampler-comparison-latest-rubato-v17-10-pinned-heavy-20260727.json
```

### Evidence artifacts

The complete primary JSON is
[`resampler-comparison-latest-soxr-v2-11-pinned-heavy-20260727.json`](../.trellis/tasks/archive/2026-07/07-26-optimize-resampler-pareto-frontier/research/resampler-comparison-latest-soxr-v2-11-pinned-heavy-20260727.json),
SHA-256
`CFDE63CA94A027C226D947C37F79AFF839347D76BBBD200721DD9F463E73310A`.
The Rubato-only supplementary JSON is
[`resampler-comparison-latest-rubato-v17-10-pinned-heavy-20260727.json`](../.trellis/tasks/archive/2026-07/07-26-optimize-resampler-pareto-frontier/research/resampler-comparison-latest-rubato-v17-10-pinned-heavy-20260727.json),
SHA-256
`D8156EB3A6CEC733924E9A45EB72C8C05BF3D128C6E26AA40DE0B59618C09919`.
The primary 2026-07-26 full JSON is
[`resampler-comparison-representative-11-full-20260726.json`](../.trellis/tasks/archive/2026-07/07-26-cross-project-audio-benchmarks/research/resampler-comparison-representative-11-full-20260726.json),
SHA-256
`43F0A6F0DCD6F4443854CC6598904F63C20E1B44A8B505FDDE358FB7CB6D485F`.
The 2026-07-26 Rubato-backend supplementary JSON is
[`resampler-comparison-rubato-backend-supplementary-full-20260726.json`](../.trellis/tasks/archive/2026-07/07-26-cross-project-audio-benchmarks/research/resampler-comparison-rubato-backend-supplementary-full-20260726.json),
SHA-256
`2862137A5FC95CFF3B9EC8E54E80E3603F242420DF4BBC05821C193DBF91EBE8`.

---

## 13. Limitations

- This is one Windows machine and has no compatible historical baseline. The
  latest runs verify core affinity and raised process/thread priority, but
  interrupts, frequency changes, and background load remain visible in the raw
  distributions. The historical 2026-07-26 run was unpinned.
- f32 and f64 lanes, filter recipes, transition bands, phase behavior, and
  latency policies differ. There is no strict cross-format speed gate.
- The benchmark measures reusable in-process sample-rate conversion only. It
  does not measure decoder/player startup, device buffers, drivers, DACs, or
  user-perceived playback latency.
- The selected 997 Hz, 18 kHz, and 23 kHz probes are useful objective points,
  not a complete frequency-response, phase, or perceptual listening study.
- This benchmark is not run in CI: it requires locally built native shims and
  external DLLs with verified provenance. General quality gates for the
  production backends are enforced by the CI workflows described in
  `docs/quality.md`.
- The primary 11-engine matrix is fully measured for the stated scope. The
  alternate project Rubato row comes from the explicitly incomplete
  Rubato-only feature build. Neither report establishes a universal ranking of
  every audio resampler or every configuration.
- The study's THD+N / alias probes are independent from
  `audio_quality_measurements`; treat rows from the two suites as
  non-interchangeable evidence classes.

The earlier four-engine report remains available as explicitly historical
[phase-1 evidence](../.trellis/tasks/archive/2026-07/07-26-cross-project-audio-benchmarks/research/phase-1-results.md).
The earlier `resampler-comparison-all-11-full-20260726.json` artifact is also
retained only as invalidated history: its v3 harness allowed output slack,
conflated latency concepts, accepted undefined quality floors, and lacked the
new provenance/reset gates. It must not be cited as current benchmark evidence.

---

## Appendix: 2026-07-26 historical tables

The primary 2026-07-26 report used rustc 1.93.1, Windows x86-64, an Intel
Family 6 Model 154 CPU, release mode, revision
`342fd447c4c92025c86497b3cfb0d729559046ab`, and a dirty worktree. It contains
11 terminal coverage rows, 22 measured cases, zero unavailable engines, and no
invalid quality or work rows. It was unpinned and is retained for trend
reading, not as the current evidence baseline.

Steady medians and lifecycle medians:

| Engine | Lane | Steady 44.1→48 | Steady 48→44.1 | Setup μs | Reset μs | Drain μs |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| audio-engine-core | f64 | 12.638 | 9.294 | 606.2 / 534.1 | 435.6 / 458.6 | 107.5 / 86.5 |
| raw libsoxr | f64 | 12.155 | 9.528 | 283.2 / 269.2 | 434.9 / 350.4 | 175.1 / 96.2 |
| raw Rubato | f64 | 14.858 | 9.145 | 124.6 / 69.5 | 0.9 / 0.8 | 6.5 / 5.4 |
| libsamplerate | f32 | 406.935 | 346.797 | 202.5 / 203.6 | 72.2 / 66.7 | 0.5 / 0.4 |
| FFmpeg libswresample | f64 | 8.585 | 6.910 | 120.1 / 582.8 | 153.8 / 125.8 | 2.2 / 1.9 |
| SpeexDSP | f32 | 343.331 | 330.236 | 517.1 / 496.2 | 548.2 / 623.8 | 107.9 / 102.6 |
| r8brain | f64 | 21.721 | 23.165 | 21.6 / 22.7 | 0.6 / 0.8 | 61.5 / 92.6 |
| zita-resampler | f32 | 37.614 | 37.171 | 1957.6 / 2917.1 | 0.7 / 0.6 | 5.8 / 6.5 |
| WebRTC | f32 | 9.511 | 9.330 | 318.8 / 309.8 | 304.0 / 343.4 | 7.4 / 8.7 |
| WDL | f64 | 47.834 | 39.985 | 174.5 / 202.6 | 0.4 / 0.6 | 10.2 / 11.1 |
| libresample | f32 | 39.931 | 48.644 | 12409.9 / 12270.7 | 16029.3 / 13150.5 | 36.4 / 58.7 |

The closest wrapper control in that run: audio-engine-core SoXR against raw
libsoxr in the same f64 lane and quality recipe measured 4.0% slower for
44.1-to-48 and 2.5% faster for 48-to-44.1. FFmpeg and WebRTC had competitive
throughput but materially weaker measured stopband response under their
selected public/default recipes. r8brain and raw Rubato had the strongest
measured rejection, with very different buffering latency. These are
trade-offs, not interchangeable-quality rankings.

Objective response evidence (2026-07-26):

| Engine | API latency frames | Input before output | Impulse peak | 18 kHz gain dB | THD+N dB | 48→44.1 alias dB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| audio-engine-core | 0 / 0 | 512 / 512 | 0 / 0 | −0.00172 / −0.00218 | −134.31 / −134.38 | −137.67 |
| raw libsoxr | n/a / n/a | 512 / 512 | 0 / 0 | −0.00172 / −0.00218 | −134.31 / −134.38 | −137.67 |
| raw Rubato | 160 / 147 | 0 / 0 | 160 / 147 | −0.00000 / −0.00000 | −196.54 / −206.60 | −202.86 |
| libsamplerate | n/a / n/a | 0 / 0 | 0 / 0 | −0.00000 / −0.00000 | −149.43 / −145.61 | −163.66 |
| FFmpeg libswresample | n/a / n/a | 0 / 0 | 0 / 0 | 0.00015 / −0.00961 | −106.56 / −108.69 | −19.93 |
| SpeexDSP | 139 / 129 | 0 / 0 | 139 / 129 | −0.00001 / −0.00001 | −136.63 / −137.78 | −123.40 |
| r8brain | 1851 / 1533 | 1536 / 1536 | 0 / 0 | 0.00000 / 0.00000 | −180.54 / −181.11 | −213.63 |
| zita-resampler | 104 / 99 | 0 / 0 | 0 / 0 | −0.00001 / 0.00000 | −144.73 / −145.07 | −114.82 |
| WebRTC | 17 / 15 | 0 / 0 | 18 / 15 | −0.65685 / −0.83634 | −89.60 / −91.54 | −42.87 |
| WDL | 34 / 28 | 0 / 0 | 0 / 0 | −0.00444 / −0.00394 | −121.27 / −123.36 | −32.76 |
| libresample | 30 / 27 | 0 / 0 | 0 / 0 | −0.22695 / −0.22696 | −79.23 / −76.66 | −78.88 |

The Rubato-only build from that period measured the project backend at
16.496 / 11.965 ns/input-sample and raw Rubato at 12.612 / 12.750, with 20
valid cases for 10 runnable engines; its 11-row coverage table correctly
remains `all_terminal=false` because raw libsoxr was intentionally not
compiled. That report is supplementary and is not the representative matrix
completion artifact.