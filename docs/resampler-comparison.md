# Cross-project resampler comparison

`audio_resampler_comparison_perf` compares reusable streaming sample-rate
conversion engines under one benchmark-owned lifecycle. It is a
quality/latency/throughput Pareto report, not a single fastest-engine ranking.
No adapter changes the production resampler selection or ordinary linkage.

## Representative matrix

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

## Workload and measurements

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
baselines may become regression gates.

Native libraries are loaded only from explicit paths. The report records the
canonical path, upstream version, shim/source revision, build provenance,
SHA-256, size, sample lane, and linked runtime artifacts. Acquired binaries
stay under ignored `target/benchmark-deps/` storage. Formal complete-matrix
runs reject unverified native provenance. API buffering latency, input consumed
before first output, and complete-stream impulse alignment are separate fields.

## Reproduction

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
  --out .trellis/tasks/07-26-cross-project-audio-benchmarks/research/resampler-comparison-representative-11-full-20260726.json
```

The 2026-07-27 confirmation pins both heavy runs to logical core 2. The primary
all-feature build supplies the complete 11-engine matrix; the Rubato-only build
supplies the alternate production backend and intentionally cannot run the raw
libsoxr control.

```powershell
cargo bench --bench audio_resampler_comparison_perf --all-features -- `
  --heavy --pinned --pin-core 2 --enforce --require-complete-matrix `
  --raw-rubato-geometry 1024/2 `
  --libsamplerate $srcDll @shimArgs `
  --out .trellis/tasks/07-26-optimize-resampler-pareto-frontier/research/resampler-comparison-latest-soxr-v2-11-pinned-heavy-20260727.json

cargo bench --bench audio_resampler_comparison_perf `
  --no-default-features --features rubato -- `
  --heavy --pinned --pin-core 2 --enforce `
  --raw-rubato-geometry 1024/2 `
  --libsamplerate $srcDll @shimArgs `
  --out .trellis/tasks/07-26-optimize-resampler-pareto-frontier/research/resampler-comparison-latest-rubato-v17-10-pinned-heavy-20260727.json
```

## 2026-07-27 pinned-heavy Pareto result

Both reports used rustc 1.93.1, Windows x86-64, an Intel Family 6 Model 154
CPU, release mode, revision `342fd447c4c92025c86497b3cfb0d729559046ab`,
a dirty worktree, 512 input frames, 4,000 timed buffers per trial, 15 trials,
and verified core-2 affinity. The primary SoXR build measured all 11 engine IDs
and all 22 cases with zero unavailable engines or run failures. The Rubato-only
supplement measured 10 runnable engine IDs and 20 valid cases with zero run
failures; raw libsoxr is correctly marked unavailable because that feature was
not compiled.

The table expands `audio_engine_core` into its two production backend
configurations, so it has 12 configuration rows but still represents the same
11 engine IDs. All rows except `audio-engine-core Rubato v17` come from the
single complete primary run. Within a steady cell, values are median / p95 in
ns/input-sample. Lifecycle pairs are 44.1-to-48 / 48-to-44.1 in microseconds.

| Configuration | Lane | Steady 44.1->48 | Steady 48->44.1 | Setup us | Reset us | Drain us |
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

The raw Rubato row above belongs to the complete primary run so the 11-engine
matrix remains a single-run comparison. The strict project/raw Rubato control
must instead use the raw row from the Rubato-only supplement. These are the
only same-run, same-recipe adapter conclusions:

| Pair | Direction | Project median | Raw median | Project delta | Verdict | Project / raw p95 |
| --- | --- | ---: | ---: | ---: | --- | ---: |
| SoXR v2 / raw libsoxr | 44.1->48 | 8.569 | 8.632 | -0.73% | tied | 10.775 / 10.158 |
| SoXR v2 / raw libsoxr | 48->44.1 | 7.424 | 7.368 | +0.75% | tied | 10.815 / 12.505 |
| Rubato v17 / raw Rubato 1024/2 | 44.1->48 | 8.182 | 8.592 | -4.77% | project faster | 13.696 / 14.226 |
| Rubato v17 / raw Rubato 1024/2 | 48->44.1 | 7.025 | 6.908 | +1.70% | tied | 8.336 / 8.599 |

The project convention requires at least a 2% median advantage or separated
trial distributions for a faster claim. Therefore SoXR v2 is statistically
tied with raw libsoxr in both directions; Rubato v17 wins forward and ties
reverse against its same-geometry raw control. The Rubato project p95 is about
3% to 4% lower in both directions, but that does not turn the reverse median
into a win.

Objective response and latency from the same reports are below. Pairs are
44.1-to-48 / 48-to-44.1; alias is measured only in the reverse downsampling
case.

| Configuration | API latency frames | Input before output | Impulse peak | 18 kHz gain dB | THD+N dB | Reverse alias dB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| audio-engine-core SoXR v2 | 0 / 0 | 512 / 512 | 0 / 0 | -0.00172 / -0.00218 | -134.31 / -134.38 | -137.67 |
| audio-engine-core Rubato v17 (supplement) | 0 / 0 | 512 / 512 | 0 / 0 | 0.00000 / 0.00000 | -200.76 / -209.82 | -232.81 |
| raw libsoxr | n/a / n/a | 512 / 512 | 0 / 0 | -0.00172 / -0.00218 | -134.31 / -134.38 | -137.67 |
| raw Rubato 1024/2 | 0 / 0 | 512 / 512 | 0 / 0 | 0.00000 / 0.00000 | -200.76 / -209.82 | -232.81 |
| libsamplerate | n/a / n/a | 0 / 0 | 0 / 0 | -0.00000 / -0.00000 | -149.43 / -145.61 | -163.66 |
| FFmpeg libswresample | n/a / n/a | 0 / 0 | 0 / 0 | 0.00015 / -0.00961 | -106.56 / -108.69 | -19.93 |
| SpeexDSP | 139 / 129 | 0 / 0 | 139 / 129 | -0.00001 / -0.00001 | -136.63 / -137.78 | -123.40 |
| r8brain | 1851 / 1533 | 1536 / 1536 | 0 / 0 | 0.00000 / 0.00000 | -180.54 / -181.11 | -213.63 |
| zita-resampler | 104 / 99 | 0 / 0 | 0 / 0 | -0.00001 / 0.00000 | -144.73 / -145.07 | -114.82 |
| WebRTC | 17 / 15 | 0 / 0 | 18 / 15 | -0.65685 / -0.83634 | -89.60 / -91.54 | -42.87 |
| WDL | 34 / 28 | 0 / 0 | 0 / 0 | -0.00444 / -0.00394 | -121.27 / -123.36 | -32.76 |
| libresample | 30 / 27 | 0 / 0 | 0 / 0 | -0.22695 / -0.22696 | -79.23 / -76.66 | -78.88 |

FFmpeg and WebRTC are faster in parts of this workload, but their selected
recipes measured only -19.93 dB and -42.87 dB reverse alias attenuation. The
Rubato route measured -232.81 dB. Different lanes, filters, transition bands,
phase behavior, and latency policies make the cross-project rows Pareto
evidence, not an equal-quality speed ranking.

The complete primary JSON is
[`resampler-comparison-latest-soxr-v2-11-pinned-heavy-20260727.json`](../.trellis/tasks/07-26-optimize-resampler-pareto-frontier/research/resampler-comparison-latest-soxr-v2-11-pinned-heavy-20260727.json),
SHA-256
`CFDE63CA94A027C226D947C37F79AFF839347D76BBBD200721DD9F463E73310A`.
The Rubato-only supplementary JSON is
[`resampler-comparison-latest-rubato-v17-10-pinned-heavy-20260727.json`](../.trellis/tasks/07-26-optimize-resampler-pareto-frontier/research/resampler-comparison-latest-rubato-v17-10-pinned-heavy-20260727.json),
SHA-256
`D8156EB3A6CEC733924E9A45EB72C8C05BF3D128C6E26AA40DE0B59618C09919`.
Absolute timing across the two separately compiled reports is informative but
is not a controlled AB comparison.

## 2026-07-26 primary full result

The primary report used rustc 1.93.1, Windows x86-64, an Intel Family 6 Model
154 CPU, release mode, revision `342fd447c4c92025c86497b3cfb0d729559046ab`,
and a dirty worktree. It contains 11 terminal coverage rows, 22 measured cases,
zero unavailable engines, and no invalid quality or work rows.

Steady medians and lifecycle medians are:

| Engine | Lane | Steady 44.1->48 | Steady 48->44.1 | Setup us | Reset us | Drain us |
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

Values separated by `/` are 44.1-to-48 / 48-to-44.1. The raw trial vectors and
p95 values remain in JSON; the table does not hide scheduler outliers.

Objective response evidence is:

| Engine | API latency frames | Input before output | Impulse peak | 18 kHz gain dB | THD+N dB | 48->44.1 alias dB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| audio-engine-core | 0 / 0 | 512 / 512 | 0 / 0 | -0.00172 / -0.00218 | -134.31 / -134.38 | -137.67 |
| raw libsoxr | n/a / n/a | 512 / 512 | 0 / 0 | -0.00172 / -0.00218 | -134.31 / -134.38 | -137.67 |
| raw Rubato | 160 / 147 | 0 / 0 | 160 / 147 | -0.00000 / -0.00000 | -196.54 / -206.60 | -202.86 |
| libsamplerate | n/a / n/a | 0 / 0 | 0 / 0 | -0.00000 / -0.00000 | -149.43 / -145.61 | -163.66 |
| FFmpeg libswresample | n/a / n/a | 0 / 0 | 0 / 0 | 0.00015 / -0.00961 | -106.56 / -108.69 | -19.93 |
| SpeexDSP | 139 / 129 | 0 / 0 | 139 / 129 | -0.00001 / -0.00001 | -136.63 / -137.78 | -123.40 |
| r8brain | 1851 / 1533 | 1536 / 1536 | 0 / 0 | 0.00000 / 0.00000 | -180.54 / -181.11 | -213.63 |
| zita-resampler | 104 / 99 | 0 / 0 | 0 / 0 | -0.00001 / 0.00000 | -144.73 / -145.07 | -114.82 |
| WebRTC | 17 / 15 | 0 / 0 | 18 / 15 | -0.65685 / -0.83634 | -89.60 / -91.54 | -42.87 |
| WDL | 34 / 28 | 0 / 0 | 0 / 0 | -0.00444 / -0.00394 | -121.27 / -123.36 | -32.76 |
| libresample | 30 / 27 | 0 / 0 | 0 / 0 | -0.22695 / -0.22696 | -79.23 / -76.66 | -78.88 |

For r8brain, zita, WDL, and libresample, non-zero API/buffering latency with a
frame-zero complete-stream impulse peak is intentional: their file-aligned
adapters pre-roll or internally consume lookahead, so wall-clock output
availability and output-file sample alignment are different quantities.

The closest wrapper control is audio-engine-core SoXR against raw libsoxr in
the same f64 lane and quality recipe. The wrapper measured 4.0% slower for
44.1-to-48 and 2.5% faster for 48-to-44.1 in this run. FFmpeg and WebRTC had
competitive throughput but materially weaker measured stopband response under
their selected public/default recipes. r8brain and raw Rubato had the strongest
measured rejection, with very different buffering latency. These are trade-offs,
not interchangeable-quality rankings.

The primary JSON is
[`resampler-comparison-representative-11-full-20260726.json`](../.trellis/tasks/07-26-cross-project-audio-benchmarks/research/resampler-comparison-representative-11-full-20260726.json),
SHA-256
`43F0A6F0DCD6F4443854CC6598904F63C20E1B44A8B505FDDE358FB7CB6D485F`.

## Rubato-backend supplementary result

The Rubato-only build measured the project backend at 16.496 / 11.965
ns/input-sample and raw Rubato at 12.612 / 12.750. It contains 20 valid cases
for 10 runnable engines. Its 11-row coverage table correctly remains
`all_terminal=false` because raw libsoxr was intentionally not compiled in
that feature set; the report is supplementary and is not the representative
matrix completion artifact.

The supplementary JSON is
[`resampler-comparison-rubato-backend-supplementary-full-20260726.json`](../.trellis/tasks/07-26-cross-project-audio-benchmarks/research/resampler-comparison-rubato-backend-supplementary-full-20260726.json),
SHA-256
`2862137A5FC95CFF3B9EC8E54E80E3603F242420DF4BBC05821C193DBF91EBE8`.
Absolute timing from the two separately compiled runs must not be treated as a
controlled AB comparison.

## Limits

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
- The primary 11-engine matrix is fully measured for the stated scope. The
  alternate project Rubato row comes from the explicitly incomplete
  Rubato-only feature build. Neither report establishes a universal ranking of
  every audio resampler or every configuration.

The earlier four-engine report remains available as explicitly historical
[phase-1 evidence](../.trellis/tasks/07-26-cross-project-audio-benchmarks/research/phase-1-results.md).
The earlier `resampler-comparison-all-11-full-20260726.json` artifact is also
retained only as invalidated history: its v3 harness allowed output slack,
conflated latency concepts, accepted undefined quality floors, and lacked the
new provenance/reset gates. It must not be cited as current benchmark evidence.
