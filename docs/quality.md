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
cargo bench --bench audio_output_render_perf -- --quick
cargo bench --bench audio_resampler_streaming_perf -- --quick
cargo bench --bench audio_convolver_perf -- --quick
cargo bench --bench audio_lockfree_params_perf -- --quick
cargo bench --bench audio_fir_eq_perf -- --quick
cargo bench --bench audio_quality_measurements -- --quick
cargo bench --bench audio_gapless_comparison_perf -- --quick
```

The standardized evidence entry points can also write versioned JSON reports:

```bash
cargo bench --bench audio_quality_measurements -- --quick --enforce --out target/bench-reports/quality.json
cargo bench --bench audio_callback_chain_perf -- --quick --enforce --out target/bench-reports/callback.json
cargo bench --bench audio_output_render_perf -- --quick --enforce --out target/bench-reports/render.json
cargo bench --bench audio_resampler_streaming_perf -- --quick --enforce --out target/bench-reports/resampler.json
cargo bench --bench audio_convolver_perf -- --quick --enforce --out target/bench-reports/convolver.json
cargo bench --bench audio_convolver_perf -- --quick --enforce --pinned --out target/bench-reports/convolver-pinned.json
cargo bench --bench audio_fir_eq_perf -- --quick --enforce --out target/bench-reports/fir-eq.json
cargo bench --bench audio_gapless_comparison_perf -- --quick --enforce --out target/bench-reports/gapless.json
```

Quality `--enforce` applies deterministic objective gates while keeping
report-only metrics and missing optional corpora distinct. Performance
`--enforce` always validates finite timing, complete work, stable case keys, and
report integrity. Timing remains report-only unless a compatible same-machine
baseline is supplied; the default gate allows exactly 10% median regression and
fails above it. On Windows, the convolver's opt-in `--pinned` mode records the
logical core in report conditions and additionally enforces the machine-local
65536-tap, 6-channel, 64-frame p99/max callback gates. Pinned and unpinned
reports are baseline-incompatible.

```bash
cargo bench --bench audio_callback_chain_perf -- --quick --enforce \
  --baseline target/bench-reports/callback-baseline.json \
  --out target/bench-reports/callback-candidate.json
```

Baseline comparison rejects mismatched schema, probe, Rust target/compiler,
OS/architecture, CPU, Cargo profile, feature set, mode, conditions, or case set;
an unavailable required environment field is also rejected. Revision and dirty
state are recorded but may differ. Reports retain every trial plus
min/median/nearest-rank p95/max and the complete build environment.
Omit `--quick` for the full workload or pass `--heavy` to the five performance
benches for stress runs. The quality bench uses quick/full only. GitHub shared
runners generate and upload five quick JSON artifacts (the gapless comparison
bench is not part of CI) without imposing a cross-machine absolute nanosecond
threshold.

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
| Streaming resampler, 44.1 kHz to 48 kHz (`process_checked`, SoXR backend) | 8.45 ns/input sample | 8.65 us/input buffer | seven-trial quick median (2026-07-21); p95 source-buffer reference utilization 0.118% |
| Streaming resampler, 44.1 kHz to 48 kHz (`process_checked`, rubato High FFT route) | 9.86 ns/input sample | 10.10 us/input buffer | seven-trial quick median (2026-07-22, `--no-default-features --features rubato`); p95 source-buffer reference utilization 0.091% |
| Streaming resampler, 48 kHz to 96 kHz (`process_checked`, rubato High half-band route) | 6.03 ns/input sample | 6.17 us/input buffer | seven-trial quick median (2026-07-24, `--no-default-features --features rubato`); p95 source-buffer reference utilization 0.082% |
| `FFTConvolver` alone, 256-tap IR, stereo | 9.39 ns | n/a | seven-trial pinned quick median (2026-07-23) |
| FIR EQ apply, 511-tap IR via `FFTConvolver`, stereo | 10.9 ns | 11.2 us | seven-trial quick median (2026-07-23); versioned `audio_fir_eq_perf --quick` report |

For a 512-frame buffer at 48 kHz (about 10.7 ms of audio), even the heaviest
chain measured here uses well under one callback period.

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

### Resampler backends

The resampler quality rows above measure the default native SoXR (SoX VHQ)
backend. The pure-Rust rubato backend (`default-features = false,
features = ["rubato"]`) uses a dedicated 127-tap symmetric half-band FIR for
exact 2x `Linear + High` upsampling. Other common Low-through-High ratios use
rubato 4.0's synchronous FFT engine; UltraHigh and ratios whose reduced
components would create pathological FFT blocks use windowed sinc. The shared
adapter removes each linear engine's leading delay. The backend passes the
same 27 quick-run quality gates on this machine; that bench explicitly requests
UltraHigh, while route-specific tests separately enforce the High half-band's
20 kHz gain, THD+N, interpolation images, lifecycle, and zero-allocation
contracts. Representative same-machine UltraHigh deltas:

| Metric | SoXR (default) | rubato |
| --- | ---: | ---: |
| Resampler THD+N, 44.1 kHz to 48 kHz | -187.0 dB | -216.2 dB |
| Passband max deviation, 20 Hz to 18 kHz | 0.0013 dB | 0.0000 dB |
| 20 kHz resampler gain | -0.0062 dB | -0.0017 dB |
| Worst fitted alias attenuation, 96 kHz to 48 kHz | -290.2 dB | -208.1 dB |

Same-machine streaming cost (`audio_resampler_streaming_perf`; 512-frame stereo
buffers, `process_checked`, seven-trial medians). The 44.1-to-48 row uses the
2026-07-22 FFT report and the 2026-07-21 SoXR reference. The 48-to-96 Rubato row
uses the 2026-07-24 exact-2x half-band report:

| Case | SoXR (default) | rubato selected route |
| --- | ---: | ---: |
| 44.1 kHz to 48 kHz, ns/input sample (us/input buffer) | 8.45 (8.65 us) | 9.86 (10.10 us) |
| 48 kHz to 96 kHz, ns/input sample (us/input buffer) | 6.73 (6.89 us) | 6.03 (6.17 us) |

Against a same-revision retained-FFT baseline, the 48-to-96 half-band route
reduced 128/256/512-frame `process_checked` medians from
36.104/14.354/17.667 to 5.849/5.807/6.026 ns/input sample (83.8%, 59.5%, and
65.9%). All cases passed consumed/produced and finite-output work validation.

`OutputRenderChain` deliberately requests UltraHigh, so pure-Rust resampled
offline rendering uses sinc rather than the High FFT route. In the same
2026-07-22 quick probe, the active 44.1-to-48 kHz 4096-frame render measured
353.97 ns/input sample for a one-second input and 266.38 for five seconds
(3.12% and 2.35% realtime factors). A diagnostic all-FFT reference measured
126.64 and 93.65 respectively, so preserving UltraHigh sinc evidence costs
about 2.8x in that offline scenario; both remain comfortably faster than
realtime.

Benchmark reports now record the compiled backend in the environment
`features` field (`resampler-soxr` / `resampler-rubato`) and in the
`algorithm` labels, so performance baselines recorded before backend labeling
are incompatible with new reports.

For `PhaseResponse::Linear`, rubato keeps the half-band/FFT/sinc routing
described above.
For `Minimum` and `Maximum`, the pure-Rust backend instead creates a bounded
rational polyphase FIR during setup from the same low-pass magnitude target:
real-cepstrum spectral factorization produces the causal minimum-phase kernel,
and its reversal produces the maximum-phase kernel. The nonlinear bank accepts
only reduced rate components up to 1024; unsupported geometry returns a typed
initialization error instead of silently using linear phase. Its reported
algorithmic latency and finite tail preserve the actual causal response. Tests
cover phase-energy ordering, magnitude preservation, 20 kHz gain, THD+N,
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
