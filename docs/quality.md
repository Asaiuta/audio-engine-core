# Performance And Audio Quality

> All values on this page are representative single-machine runs; they are
> reproducible via `cargo bench` but will differ by CPU, compiler version, and
> load.

These numbers come from the benchmarks in `benches/`, which run entirely against
this crate's public API. They are evidence for one machine and one configuration,
not a universal claim. Reproduce them with `cargo bench`; the exact values will
differ by CPU, compiler version, and load.

```bash
cargo bench --bench audio_callback_chain_perf -- --quick
cargo bench --bench audio_callback_tail_perf -- --quick
cargo bench --bench audio_output_render_perf -- --quick
cargo bench --bench audio_resampler_streaming_perf -- --quick
cargo bench --bench audio_resampler_matrix_perf -- --quick
cargo bench --bench audio_convolver_perf -- --quick
cargo bench --bench audio_lockfree_params_perf -- --quick
cargo bench --bench audio_fir_eq_perf -- --quick
cargo bench --bench audio_quality_measurements -- --quick
cargo bench --bench audio_gapless_comparison_perf -- --quick
cargo bench --bench audio_decoder_perf -- --quick
cargo bench --bench audio_component_perf -- --quick
cargo bench --bench audio_lifecycle_memory_perf -- --quick
```

The standardized evidence entry points can also write versioned JSON reports:

```bash
cargo bench --bench audio_quality_measurements -- --quick --enforce --out target/bench-reports/quality.json
cargo bench --bench audio_callback_chain_perf -- --quick --enforce --out target/bench-reports/callback.json
cargo bench --bench audio_callback_tail_perf -- --quick --enforce --out target/bench-reports/callback-tail.json
cargo bench --bench audio_output_render_perf -- --quick --enforce --out target/bench-reports/render.json
cargo bench --bench audio_resampler_streaming_perf -- --quick --enforce --out target/bench-reports/resampler.json
cargo bench --bench audio_resampler_matrix_perf -- --quick --enforce --out target/bench-reports/resampler-matrix.json
cargo bench --bench audio_convolver_perf -- --quick --enforce --out target/bench-reports/convolver.json
cargo bench --bench audio_convolver_perf -- --quick --enforce --pinned --out target/bench-reports/convolver-pinned.json
cargo bench --bench audio_fir_eq_perf -- --quick --enforce --out target/bench-reports/fir-eq.json
cargo bench --bench audio_gapless_comparison_perf -- --quick --enforce --out target/bench-reports/gapless.json
cargo bench --bench audio_decoder_perf -- --quick --enforce --out target/bench-reports/decoder.json
cargo bench --bench audio_component_perf -- --quick --enforce --out target/bench-reports/components.json
cargo bench --bench audio_lifecycle_memory_perf -- --quick --enforce --out target/bench-reports/lifecycle-memory.json
```

Quality `--enforce` applies deterministic objective gates while keeping
report-only metrics and missing optional corpora distinct. Performance
`--enforce` validates finite timing, stable case keys, and report integrity for
the work each probe actually recorded; it does not by itself prove that every
intended case ran. Probes state their own completeness rule: the fixture-driven
`audio_gapless_comparison_perf` fails on any attempted fixture whose correctness
probe could not produce a verdict (reported as `probe_failures`), but a fixture
that was never supplied is recorded as `skipped` and gates nothing. Timing
remains report-only unless a compatible same-machine baseline is supplied; the
default gate allows exactly 10% median regression and fails above it. On
Windows, the convolver's opt-in `--pinned` mode records the logical core in
report conditions and additionally enforces the machine-local 65536-tap,
6-channel, 64-frame p99/max callback gates. Pinned and unpinned reports are
baseline-incompatible.

`audio_lockfree_params_perf` is a machine-local exploratory probe, not a
report-backed evidence gate. It emits no JSON artifact and carries no
environment, case-key, or baseline identity, and it is not run in CI. Its
`--enforce` mode asserts a fixed 3% same-run improvement of the lock-free path
over its mutex reference from a single wall-clock sample, so a red result there
is a hint to re-measure rather than a traceable regression.

`audio_callback_chain_perf` and `audio_callback_tail_perf` intentionally answer
different questions. The chain probe averages many callbacks into each trial,
which is suitable for steady-state throughput and historical median/p95
baselines. The tail probe retains one raw `Instant` duration per callback and
reports median/p95/p99/p99.9/max, the same percentiles as deadline utilization,
and missed-deadline count/rate. Quick mode records 4,000 callbacks for every
bypass/no-convolver/convolver scenario at 64/128/256/512 frames, so p99.9 is
backed by four observations rather than collapsing to one maximum.

The timed interval starts before copying the synthetic stereo input into the
preallocated callback buffer and ends after `DspChain::process` plus an output
`black_box`; fixture construction, warmup, work validation, and report I/O are
outside it. A case deadline is `frames / 48,000 Hz`, and a callback is counted
as missed only when its retained duration exceeds that deadline. Full and
heavy modes retain 20,000 and 100,000 callbacks per case respectively; use
them for deeper machine-local evidence rather than routine shared CI.

Unpinned callback-tail runs are report-integrity gates, including on shared CI.
Strict callback-tail timing comparison is Windows-only and requires both
reports to use `--pinned` with the same verified affinity/priority state. The
active-chain defaults allow 10% median, 20% p99, and 30% p99.9 regression.
Sub-microsecond bypass tails remain report-only because a 100 ns timer tick can
look like a 50% relative change; their raw samples and missed deadlines are not
trimmed or hidden. Processor affinity and elevated priority reduce migration
and ordinary contention, but they cannot exclude interrupts, DPC activity,
frequency changes, or other scheduler noise; those events remain in the raw
distribution.

```bash
cargo bench --bench audio_callback_chain_perf -- --quick --enforce \
  --baseline target/bench-reports/callback-baseline.json \
  --out target/bench-reports/callback-candidate.json

cargo bench --bench audio_callback_tail_perf -- --quick --enforce --pinned --pin-core 2 \
  --out target/bench-reports/callback-tail-baseline.json
cargo bench --bench audio_callback_tail_perf -- --quick --enforce --pinned --pin-core 2 \
  --baseline target/bench-reports/callback-tail-baseline.json \
  --out target/bench-reports/callback-tail-candidate.json
```

Baseline comparison rejects mismatched schema, probe, Rust target/compiler,
OS/architecture, CPU, Cargo profile, feature set, mode, conditions, or case set;
an unavailable required environment field is also rejected. Revision and dirty
state are recorded but may differ. Aggregate reports retain every trial plus
min/median/nearest-rank p95/max; the tail report retains every callback and
extends the distribution through p99.9. All include the complete build
environment. Omit `--quick` for the full workload or pass `--heavy` to
performance probes that advertise it. The quality bench uses quick/full only.
The three coverage probes also accept `--baseline` and the shared 10% median
limit. Their case keys and conditions include fixture identity, feature set,
workload geometry, and backend, so a PCM fixture cannot be compared with a
different codec corpus and a SoXR lifecycle report cannot be compared with a
Rubato report.

GitHub's default-feature shared runner now generates and uploads nine quick JSON
artifacts; the pure-Rust job additionally runs decoder, component, and
lifecycle-memory quick reports with `--no-default-features --features rubato`.
The fixture-driven gapless comparator remains outside CI. Neither job imposes
a cross-machine absolute nanosecond threshold.

## Decoder, component, and lifecycle coverage

`audio_decoder_perf` creates one byte-stable 12-second stereo PCM16 RIFF/WAVE
fixture before any timed region. It times local source open, container probe,
decoder build, first borrowed PCM, steady borrowed decode, coarse seek, and
seek-to-first-PCM separately. Every report records the complete fixture format,
length, and content hash; validates decoded frame count, finite output, packet
count, and output hash; and reports exact crate-owned `f64` staging capacity.
Its allocation rows count only operations routed through Rust's global
allocator. Symphonia or system/native storage that bypasses that allocator is
not estimated.

On the 2026-07-26 Windows/x86_64 final quick verification run, the warm local
PCM fixture produced these medians:

| Decoder phase | Median | Additional evidence |
| --- | ---: | --- |
| Local source open | 27.31 us | warm filesystem cache; file open only |
| Container probe | 9.79 us | 18,432-byte fixed decoder staging requirement |
| Decoder build | 9.19 us | 23,328 retained Rust setup bytes in the measured scope |
| First borrowed PCM | 7.43 us | non-empty, finite decoder-owned packet |
| Steady borrowed decode | 19.79 ns/frame | 50.52 million frames/s; 1052.5x realtime factor |
| Coarse seek command | 2.40 us | raw 24-sample quick distribution includes p99/max |
| Coarse seek through first PCM | 9.00 us | seek plus a validated non-empty packet |

These values are PCM/WAV, warm-cache, local-source evidence. Compressed-codec
and gapless behavior remains visible in `audio_gapless_comparison_perf`; cold
disk, live HTTP, cancellation responsiveness, and end-to-end playback are not
implied by the table.

`audio_component_perf` supplies dedicated timing for all previously uncovered
public component families. Its default-feature quick report has 16 cases:
SpectrumAnalyzer (1,024/4,096 FFT), Downmixer (5.1/7.1 to stereo),
LoudnessMeter (512/4,096 frames), contiguous and strided TruePeakDetector,
AutoMix Head/Full, RingBuffer write/read/advance, and five in-memory
LoudnessDatabase operations. The Rubato-only feature set reports 11 cases and
records LoudnessDatabase as explicitly excluded because `loudness-db` is not
compiled. Representative 2026-07-26 medians were 5.05 ns/sample for the
1,024-point spectrum case, 4.72 ns/frame for 5.1 downmix, 42.37 ns/input-sample
for 4,096-frame loudness analysis, 9.96 ns/sample for contiguous true peak,
54.42/108.18 ms for AutoMix Head/Full, and 8.08 us/row for the 128-row SQLite
batch upsert. The JSON retains every case and raw trial.

`audio_lifecycle_memory_perf` separates setup, reset, finish/drain, dynamic
Convolver ownership operations, and a bounded repeated lifecycle. Its 13-case
matrix keeps equal-rate setup/finish and timer-quantized ownership operations
visible while gating only seven stable timing cases against a compatible
baseline. The final SoXR quick run measured 457.5 us active resampler setup,
381.9 us reset, and 66.8 us finish/drain for an 8,192-frame stereo input;
equal-rate setup/finish measured 112.0/4.1 us and remain report-only.
Short/long Convolver setup was 28.4/353.5 us, and the exact 255-frame short-IR
tail drained in 8.1 us. Its five 128-cycle soak trials completed 640
publication/adoption/reclamation/quiescence cycles with zero retained Rust
bytes after each complete trial.

Persistent setup evidence on that run was 691,784 Rust bytes for the SoXR
resampler (691,672 bytes are exact adapter-owned working scratch), 53,008 bytes
for the 256-frame overlap-save Convolver, and 853,360 bytes for the 8,192-frame
partitioned Convolver. The SoXR number explicitly excludes libsoxr allocations
made outside Rust's global allocator; it is not process RSS. Rubato reports use
their own backend identity and report the public adapter working-buffer value
without inventing an opaque engine estimate.

Reset, finish, and audio-side Convolver adoption recorded zero Rust allocator
operations in their measured scopes. Publishing the one-tap ownership wrapper
retained 168 Rust bytes until adoption/retirement, and control-side reclamation
performed the corresponding seven deallocations. These rows describe ownership
movement, not a leak or a realtime-path allocation.

None of these probes owns or opens an audio device. CPAL/WASAPI buffer
negotiation, device callback scheduling, driver latency, DAC latency, and
user-visible play-button-to-sound latency require a consuming-application
integration benchmark and remain outside this crate's evidence boundary.

The table below records representative local runs; rows should be regenerated
after changing the relevant processing path.

## Realtime processing budget

Per-sample/per-buffer cost of the DSP and resampler paths at a 512-frame buffer.
These exclude the decoder and the OS audio device write; they measure only the
in-crate processing.

| Path | Per sample | Per 512-frame buffer | Bench |
| --- | ---: | ---: | --- |
| Isolated `SaturationQuality::Oversampled4x` Tube saturation | 22.9 ns | 23.4 us | seven-trial quick median at 512 frames (2026-07-22); 24.0% below the compatible 30.1 ns fixed-dispatch baseline |
| DSP chain, no convolver (volume, EQ, `SaturationQuality::Oversampled4x`, Bauer crossfeed, convolver slot empty, dynamic loudness, peak limiter, noise shaper) | 50.3 ns | 51.5 us | seven-trial quick median (2026-07-23); p95 callback utilization 0.51% |
| DSP chain with convolver and `SaturationQuality::Oversampled4x` | 60.4 ns | 61.9 us | seven-trial quick median (2026-07-23); p95 callback utilization 0.60% |
| Streaming resampler, 44.1 kHz to 48 kHz (public stereo SoXR v2 path) | 8.57 ns/input sample | 8.77 us/input buffer | latest 15-trial pinned heavy complete-matrix median (2026-07-27); raw libsoxr 8.63, delta -0.73%, classified tied |
| Streaming resampler, 44.1 kHz to 48 kHz (`process_checked`, Rubato v17 High FFT route) | 8.18 ns/input sample | 8.38 us/input buffer | latest 15-trial pinned heavy Rubato-build median (2026-07-27); same-run raw Rubato 8.59, delta -4.77% |
| Streaming resampler, 48 kHz to 96 kHz (`process_checked`, Rubato v17 High half-band route) | 5.15 ns/input sample | 5.27 us/input buffer | same 15-trial pinned heavy report; p95 6.32 ns/input sample |
| `FFTConvolver` alone, 256-tap IR, stereo | 9.39 ns | n/a | seven-trial pinned quick median (2026-07-23) |
| FIR EQ apply, 511-tap IR via `FFTConvolver`, stereo | 10.9 ns | 11.2 us | seven-trial quick median (2026-07-23); versioned `audio_fir_eq_perf --quick` report |

For a 512-frame buffer at 48 kHz (about 10.7 ms of audio), even the heaviest
chain measured here uses well under one callback period.

The 2026-07-25 Windows pinned callback-tail candidate retained 4,000 callbacks
per case. At 512 frames, active DSP without convolution measured 72.8 us p99,
104.9 us p99.9, and 419.5 us max; the active chain with the 256-tap convolver
measured 94.5 us p99, 113.1 us p99.9, and 144.4 us max. All 48,000 callbacks in
the complete matrix met their modeled deadlines. These are same-machine,
scheduler-inclusive observations, not device-callback or cross-machine claims.

## Lock-free parameter reads

The atomic parameter snapshots (`AtomicEqParams`, `AtomicVolumeParams`, and the
rest) are the mechanism for pushing parameter changes into the audio callback
without locks. Reading the full set of cached parameters once per callback costs
about **7 ns** with the generation-based snapshot path, versus ~50 ns for a
naive split-atomic field-by-field read and ~83 ns for an unconditional
`ArcSwap` guard load — an ~86% to ~92% improvement (`audio_lockfree_params_perf`).

## FIR EQ IR generation

`FirEq` designs a linear- or minimum-phase impulse response from 10 band gains;
the IR is then convolved (typically with `FFTConvolver`) to apply the EQ.
Generation is an offline/control-thread cost, not a per-sample one. On this
machine a 511-tap linear-phase design has a seven-trial quick median of ~37 us;
minimum-phase is ~114 us because of the extra cepstral phase shaping, and cost
scales with tap count (`audio_fir_eq_perf`). The generated response preserves
absolute band gain: a uniform +6 dB curve remains +6 dB. A one-tap design is
explicitly a pure scalar at the 1 kHz reference (flat 0 dB is `[1.0]`).

## AutoMix analysis contract

AutoMix analysis schema version 2 converts spectral-flux lag using the actual
`sample_rate / 512` observation cadence and derives lag bounds from the
supported tempo range. Musical-key detection is not implemented or claimed:
`AutomixKeyStatus::Unsupported` is serialized as `key_status: "unsupported"`,
and the reserved root/mode/confidence/Camelot fields remain null until a future
detector is validated against an independently labeled music corpus.

## FFT convolution routing

`FFTConvolver` keeps the existing overlap-save path for impulse responses up to
4096 taps per channel, which covers the current FIR EQ tap counts. Longer IRs
route to a uniform 1024-frame partitioned tail with an overlap-save head so
room/reverb-length responses avoid one very large callback FFT. Older tail
spectral passes are accumulated through a deterministic frame-position
schedule while the partition fills; the newest pass and inverse FFT complete
the next tail block at the boundary. This keeps the result independent of
callback chunking and leaves only preallocated, bounded work on the realtime
path.

On the 2026-07-23 Windows pinned probe (logical core 2, raised process/thread
priority), the 65536-tap, 6-channel, 64-frame case measured 16.74% p99 and
21.20% max utilization of its 1.333 ms deadline. The two pinned pre-change
baselines measured 62.49-71.68% p99 and 78.74-82.73% max. Collect absolute
max/p99 evidence on a quiet host: externally loaded runs can still contain
multi-millisecond scheduler pauses even when affinity is fixed.

The routing and partition size remain exposed as
`PARTITIONED_CONVOLUTION_IR_THRESHOLD` and
`PARTITIONED_CONVOLUTION_PARTITION_SIZE`; use `audio_convolver_perf` and
`audio_fir_eq_perf` before changing either value.

## Objective audio-quality measurements

`audio_quality_measurements` generates synthetic f64 signals, runs them through
this crate's processor modules, and analyzes the rendered buffers numerically.
This is native-rendered-buffer evidence, not analog output capture: no audio
device, OS mixer, DAC/ADC loopback, or microphone is involved, and it does not
replace listening tests.

| Metric | Result |
| --- | ---: |
| Resampler THD+N, 44.1 kHz to 48 kHz | -187.0 dB |
| Passband max deviation, 20 Hz to 18 kHz | 0.0013 dB |
| 20 kHz resampler gain | -0.0062 dB |
| Worst fitted alias attenuation, 96 kHz to 48 kHz | -290.2 dB (quick; the full workload measures -297.4 dB, both near the analyzer's -296 dB numeric floor) |
| Saturation threshold max jump / first-derivative mismatch | 1.416e-6 / 3.610e-4 |
| Saturation alias-energy reduction, Direct vs `Oversampled4x` Tube stress | +16.3 dB |
| Limiter output ceiling from a +5.11 dBFS transient | -1.00 dBFS |
| Limiter below-threshold THD+N | -253.9 dB |
| True-peak mode, intersample-stress output (input +0.10 dBTP / -3.01 dBFS) | -1.00 dBTP |
| Sample-peak mode, same input (never engages) | +0.10 dBTP |
| `LoudnessMeter` integrated parity vs direct `ebur128` | 0.000000 LU |
| 10-band EQ +6 dB target response error (62 Hz, 1 kHz, 8 kHz) | 0.0000 dB max |
| Bauer crossfeed low/high levels (80 Hz / 2 kHz) | -17.73 / -27.27 dB |
| Bauer crossfeed low-minus-high separation | +9.54 dB |
| Crossfeed mix-change continuity delta | 0.000e0 (vs 5.762e-3 for a reset simulation) |
| Noise-shaper -140 dBFS changed fraction / non-finite stress outputs | 1.000 / 0 |
| Dynamic loudness low-volume compensation | +8.41 dB at 40 Hz, +2.83 dB at 3 kHz |

### Resampler configuration matrix

`audio_resampler_matrix_perf` measures `StreamingResampler::process_checked`
across an intentional rate/quality/phase/channel set, plus construction
(`StreamingResampler::with_quality`) cost. It complements
`audio_resampler_streaming_perf` (default High/Linear path only)
and does not replace it.

Quick mode is a fixed decision set (primary rates, High/Standard/UltraHigh,
Linear+Minimum, stereo+5.1, setup cost). Full/heavy expands rates and quality
ladders without a pure cartesian product. Build soxr (default features) or
rubato-only (`--no-default-features --features rubato`); backend is recorded in
case keys and environment features so baselines cannot be mixed silently.

Still excluded: decoder, device write, full DSP callback chain, offline
render chain (see the report `excludes` array), and exhaustive pathological
rate pairs.

### Resampler backends

The resampler quality rows above measure the default native SoXR (SoX VHQ)
backend. The pure-Rust rubato backend (`default-features = false,
features = ["rubato"]`) uses a dedicated 127-tap symmetric half-band FIR for
exact 2x `Linear + High` upsampling. Other common ratios use rubato 4.0's
synchronous FFT engine at every quality tier: UltraHigh selects one FFT
sub-chunk (a 2x longer internal FIR) while Low through High use two
sub-chunks (2026-07-25 routing change). Only ratios whose reduced components
would create pathological FFT blocks use windowed sinc. The shared
adapter removes each linear engine's leading delay. The backend passes the
same 27 quick-run quality gates on this machine; that bench explicitly requests
UltraHigh, while route-specific tests separately enforce the High half-band's
20 kHz gain, THD+N, interpolation images, lifecycle, and zero-allocation
contracts. Representative same-machine UltraHigh deltas (rubato column from
the 2026-07-25 one-sub-chunk FFT run; the previous UltraHigh sinc route
measured -216.2 dB THD+N and -208.1 dB alias attenuation):

| Metric | SoXR (default) | rubato |
| --- | ---: | ---: |
| Resampler THD+N, 44.1 kHz to 48 kHz | -187.0 dB | -204.9 dB |
| Passband max deviation, 20 Hz to 18 kHz | 0.0013 dB | 0.0000 dB |
| 20 kHz resampler gain | -0.0062 dB | -0.0017 dB |
| Worst fitted alias attenuation, 96 kHz to 48 kHz | -290.2 dB | -290.5 dB |

Same-machine streaming cost (512-frame stereo buffers). The two project
backends come from separate core-pinned heavy builds, so their absolute values
are informative but not a controlled AB comparison. Each project's strict
claim uses the raw upstream control from its own run:

| Case | SoXR (default) | rubato selected route |
| --- | ---: | ---: |
| 44.1 kHz to 48 kHz, ns/input sample (us/input buffer) | 8.57 (8.77 us) | 8.18 (8.38 us) |
| 48 kHz to 44.1 kHz, ns/input sample (us/input buffer) | 7.42 (7.60 us) | 7.03 (7.19 us) |
| 48 kHz to 96 kHz, ns/input sample (us/input buffer) | not rerun in the strict control | 5.15 (5.27 us) |

#### 2026-07-27 adapter Pareto update

Stereo SoXR v2 replaces two native mono states plus deinterleave/reinterleave
scratch with one native interleaved state. Against the former project route,
steady/setup/reset/drain improved by 56.16/44.45/47.73/58.28% for 44.1-to-48
kHz and 39.42/33.68/44.30/49.69% for 48-to-44.1 kHz. Five pinned 15-trial
heavy reports compare v2 with raw stereo libsoxr under the same f64 HQ/Bits20
recipe. The median of the five per-run deltas is +1.73% forward and -1.94%
reverse, so both directions are classified as statistically tied rather than a
universal win. The reports retain one +11.11% reverse steady outlier, one
+4.27% forward outlier, and one +6.40% reverse drain outlier; none repeated in
the adjacent confirmations. Median-of-run setup/reset/drain deltas remain
between -3.48% and +0.70%.

The final complete 11-engine confirmation measured SoXR v2 / raw libsoxr at
8.569 / 8.632 ns/input-sample forward and 7.424 / 7.368 reverse. Those deltas
are -0.73% and +0.75%, preserving the tied conclusion. All 22 cases were valid,
the 11 coverage rows were terminal, and `run_failures` was empty.

Rubato v17 retains 1024/2 High geometry while removing adapter work through
bulk channel copies, split FIFO/caller input, partial-zero FFT drain, and direct
terminal truncation. In the final-source public heavy confirmation, all 16
canonical rate/API/caller medians improved 12.35% to 36.83% against the matched
v12 A2 report; the least favorable p95 delta was +2.67%. A same-run strict
1024/2 control now measures project/raw medians of 8.182/8.592 ns/input sample
forward and 7.025/6.908 reverse. The project is 4.77% faster forward and 1.70%
slower reverse; by the 2% decision rule that is a forward win and reverse tie.
Project/raw p95 is 13.696/14.226 forward and 8.336/8.599 reverse, about 3% to
4% lower for the project in both directions. The first final-source public
heavy run is also retained: system-wide frequency variation on the Balanced
Windows plan made several rows fail the v12 threshold before the immediate
confirmation passed. Raw trials and all reports remain under the Pareto task's
`research/` directory.

The detailed 11-engine throughput, lifecycle, latency, and objective-response
tables are in [resampler-comparison.md](resampler-comparison.md). They are a
Pareto comparison rather than an equal-quality ranking: for example, FFmpeg
and WebRTC are faster in some rows, but their selected recipes measured only
-19.93 dB and -42.87 dB reverse alias attenuation versus -232.81 dB for the
Rubato route.

Against a same-revision retained-FFT baseline, the 48-to-96 half-band route
reduced 128/256/512-frame `process_checked` medians from
36.104/14.354/17.667 to 5.849/5.807/6.026 ns/input sample (83.8%, 59.5%, and
65.9%). All cases passed consumed/produced and finite-output work validation.

In the 2026-07-25 same-machine paired quick matrix
(`audio_resampler_matrix_perf`, rubato backend, 512-frame stereo
`process_checked`), routing UltraHigh Linear onto the one-sub-chunk FFT
engine cut 44.1-to-48 kHz from 101.13 to 8.15 ns/input sample (~12.4x) and
48-to-96 kHz from 163.87 to 10.22 (~16x), with median setup falling from
5.82/6.26 ms to 0.16/0.20 ms; no other case regressed beyond run noise.

`OutputRenderChain` deliberately requests UltraHigh; since the 2026-07-25
routing change, pure-Rust resampled offline rendering uses the one-sub-chunk
FFT route instead of sinc. In the 2026-07-25 quick probe, the active
44.1-to-48 kHz 4096-frame render measured 103.02 ns/input sample for a
one-second input and 82.92 for five seconds (0.91% and 0.73% realtime
factors), compared with 353.97 and 266.38 (3.12% and 2.35%) for the retired
UltraHigh sinc route in the 2026-07-22 quick probe.

Benchmark reports now record the compiled backend in the environment
`features` field (`resampler-soxr` / `resampler-rubato`) and in the
`algorithm` labels, so performance baselines recorded before backend labeling
are incompatible with new reports.

For `PhaseResponse::Linear`, rubato keeps the half-band/FFT/sinc routing
described above.
For `Minimum` and `Maximum`, the pure-Rust backend instead builds an exact
spectral (FFT block-convolution) rational resampler during setup from the same
low-pass magnitude target: real-cepstrum spectral factorization produces the
causal minimum-phase kernel, its reversal produces the maximum-phase kernel,
and the kernel's complex spectrum is applied through an overlap-save input FFT,
an exact decimation alias fold, and an inverse output FFT. The fold keeps all
`down` alias terms per bin, so the engine matches the previous time-domain
polyphase convolution to < 1e-9 (regression-tested across ratios, tiers, and
both phases). The engine accepts only reduced rate components up to 1024;
unsupported geometry returns a typed initialization error instead of silently
using linear phase. Its reported algorithmic latency and finite tail preserve
the actual causal response. In the 2026-07-25 same-machine paired quick matrix
(`audio_resampler_matrix_perf`, `matrix_process_checked_v2_spectral_nonlinear`,
rubato backend, 512-frame stereo `process_checked`, load-matched runs), the
48-to-96 kHz High/Minimum median fell from 1098.5 to 15.5 ns/input sample
(~71x) and 44.1-to-48 kHz High/Minimum from 604.0 to 191.4 ns/input sample
(~3.2x); no Linear-path case regressed beyond run noise. Tests cover
phase-energy ordering, magnitude preservation, 20 kHz gain, THD+N,
alias rejection, arbitrary chunking, reset, drain, and no allocation after
setup. Both backends otherwise share the streaming cursor and terminal-reset
contract.

The saturation threshold uses a 0.05-full-scale C1 soft knee shared by the
direct, oversampled, and high-pass-exciter paths. The alias probe drives an
11 kHz Tube waveshaper and fits folded above-Nyquist harmonics. In the current
quick run, `Oversampled4x` reduced the aggregate fitted alias energy from
-15.09 dBFS to -31.42 dBFS at equivalent drive/mix settings.

The crossfeed follows the libbs2b-style low-pass/high-boost Bauer topology with
overload-prevention gain. `mix` is a dry-to-reference strength, and mix/cutoff
updates ramp over about 10 ms without clearing filter history. The listening-DSP
rows are synthetic probes after settling; they validate target response/effect
size and parameter-change continuity, not external listening-test or analog
output evidence.

The noise shapers (`NoiseShaper`) continuously dither every finite input,
including exact digital silence, and clamp to the signed target-bit range;
NaN/Inf clears only the affected channel history and returns zero. Shaping
redistributes quantization error rather than lowering broadband noise: the
curves strongly reduce the 2-6 kHz band while pushing energy into 14-18 kHz,
for up to a +34.8 dB ear-band advantage over flat TPDF dither.

The benchmark also includes an optional EBU Tech 3341/3342 expected-value corpus
check. It is skipped unless the `libebur128/test` reference vectors are present
(they are not bundled with this crate); the deterministic `LoudnessMeter` parity
fixtures above always run. Text and JSON summaries report the skipped count
explicitly. Full-output points also publish authoritative rendered frames,
algorithmic latency, retained semantic tail, and truncation state from
`RenderedOutput`; the default compensated timeline uses a -120 dBFS pre-dither
energy threshold, 250 ms continuous silence hold, and 30 s safety maximum for
unknown or infinite tails.

`PeakLimiter` defaults to 4x-oversampled intersample (true-peak) detection: on
an intersample-stress signal whose sample peak sits below the ceiling but whose
true peak is +0.10 dBTP, true-peak mode pulls the output to -1.00 dBTP while the
legacy `LimiterMode::SamplePeak` leaves it untouched at +0.10 dBTP. The limiter
runs at source rate, so resampling plus final quantization downstream of the
limiter can in principle re-introduce intersample peaks; the full output-chain
true-peak probe therefore stays report-only rather than a conformance gate. In
the current quick run the probe meets the target: the worst full-chain output
true peak is -1.000 dBTP with zero over-limit points across the probe corpus.
Runs before the 2026-07-18 DSP lifecycle fixes measured -0.610 dBTP, 0.390 dB
above the -1 dBTP target; the probe is retained as regression evidence for
exactly that failure mode.
