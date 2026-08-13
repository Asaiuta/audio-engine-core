# Audio Quality & Performance Evidence

> `audio-engine-core` treats audio quality and realtime behavior as measurable
> engineering properties.

This document is the detailed evidence layer behind the quality claims
summarized in the project README. It defines the evidence model used to
validate the crate's:

- **Audio quality** — numerical correctness and signal-quality behavior.
- **Realtime safety** — callback execution, deadline behavior, and allocation
  constraints.
- **Performance** — processing cost and regression stability.
- **Lifecycle correctness** — streaming, reset, drain, gapless, and ownership
  transitions.
- **Reproducibility** — deterministic fixtures, public-API benchmarks, and
  compatible baseline comparison.

The benchmark suite is intentionally divided into **quality gates**,
**realtime-safety gates**, **performance evidence**, and **report-only
measurements**.

Results on this page are representative measurements from specific machines,
builds, and configurations. They are evidence of the measured workload, not
universal hardware guarantees.

---

## Evidence Model

Every measurement belongs to one of four evidence classes.

| Class | Purpose | CI-enforced |
| --- | --- | --- |
| Quality gate | Deterministic correctness / signal-quality invariant | Yes |
| Realtime-safety gate | Callback correctness and deadline integrity | Yes |
| Performance regression | Same-machine regression detection | With baseline |
| Report-only | Exploratory or environment-sensitive measurement | No |

**Gate vs evidence.** A measurement becomes a gate only when it carries a
pass/fail contract:

```text
                 Measurement
                      │
          ┌───────────┴───────────┐
          │                       │
       Evidence                  Gate
          │                       │
   useful information       pass/fail contract
```

A THD+N figure is evidence; `full_output_chain_worst_true_peak ≤ -1 dBTP` is a
gate. A 1052× realtime decoder figure is evidence; it declares no contract.
This distinction runs through every section below.

**Enforcement semantics.** Quality `--enforce` applies deterministic objective
gates while keeping report-only metrics and missing optional corpora distinct.
Performance `--enforce` validates finite timing, stable case keys, and report
integrity for the work each probe actually recorded; it does not by itself
prove that every intended case ran. Probes state their own completeness rule:
the fixture-driven `audio_gapless_comparison_perf` fails on any attempted
fixture whose correctness probe could not produce a verdict (reported as
`probe_failures`), but a fixture that was never supplied is recorded as
`skipped` and gates nothing. Timing remains report-only unless a compatible
same-machine baseline is supplied; the default gate allows exactly 10% median
regression and fails above it.

---

## Evidence Summary

The representative results below are the numbers cited by the README; the
sections that follow provide methodology, gates, and reproduction for each.

| Domain | Representative result | Evidence type |
| --- | ---: | --- |
| EBU R128 integrated parity vs direct `ebur128` | **0.000000 LU** | deterministic gate |
| Resampler THD+N, 44.1 → 48 kHz (SoXR) | **−187.0 dB** | quality measurement |
| Resampler THD+N, 44.1 → 48 kHz (Rubato UltraHigh) | **−204.9 dB** | quality measurement |
| Worst fitted alias attenuation, 96 → 48 kHz | **−290.2 dB** | quality measurement |
| True-peak limiter ceiling on intersample stress | **−1.00 dBTP** | enforced gate |
| Full output-chain worst true peak | **−1.000 dBTP** | enforced gate |
| DSP chain, 512 frames @ 48 kHz (no convolver) | **51.5 μs** (p95 callback utilization 0.51%) | performance measurement |
| DSP chain with convolver, 512 frames @ 48 kHz | **61.9 μs** (p95 callback utilization 0.60%) | performance measurement |
| Steady borrowed decode, warm local PCM/WAV | **19.79 ns/frame** (1052.5× realtime) | performance measurement |
| Generation-based parameter snapshot | **~7 ns** | exploratory performance |

---

## Audio Quality

`audio_quality_measurements` generates synthetic f64 signals, runs them through
this crate's processor modules, and analyzes the rendered buffers numerically.
This is native-rendered-buffer evidence, not analog output capture: no audio
device, OS mixer, DAC/ADC loopback, or microphone is involved, and it does not
replace listening tests.

### Loudness

| Metric | Result |
| --- | ---: |
| `LoudnessMeter` integrated parity vs direct `ebur128` | 0.000000 LU |

The benchmark also includes an optional EBU Tech 3341/3342 expected-value
corpus check. It is skipped unless the `libebur128/test` reference vectors are
present (they are not bundled with this crate); the deterministic
`LoudnessMeter` parity fixtures above always run. Text and JSON summaries
report the skipped count explicitly.

Dynamic loudness low-volume compensation measures **+8.41 dB at 40 Hz** and
**+2.83 dB at 3 kHz**.

### True Peak

| Metric | Result |
| --- | ---: |
| Limiter output ceiling from a +5.11 dBFS transient | −1.00 dBFS |
| Limiter below-threshold THD+N | −253.9 dB |
| True-peak mode, intersample-stress output (input +0.10 dBTP / −3.01 dBFS) | −1.00 dBTP |
| Sample-peak mode, same input (never engages) | +0.10 dBTP |

`PeakLimiter` defaults to 4x-oversampled intersample (true-peak) detection: on
an intersample-stress signal whose sample peak sits below the ceiling but whose
true peak is +0.10 dBTP, true-peak mode pulls the output to −1.00 dBTP while
the legacy `LimiterMode::SamplePeak` leaves it untouched at +0.10 dBTP. Full
output-chain enforcement is covered under [Full-chain validation](#full-chain-validation).

### Resampling

| Metric | Result |
| --- | ---: |
| Resampler THD+N, 44.1 kHz to 48 kHz | −187.0 dB |
| Passband max deviation, 20 Hz to 18 kHz | 0.0013 dB |
| 20 kHz resampler gain | −0.0062 dB |
| Worst fitted alias attenuation, 96 kHz to 48 kHz | −290.2 dB (quick; the full workload measures −297.4 dB, both near the analyzer's −296 dB numeric floor) |

The rows above measure the native SoXR (SoX VHQ) backend (`features = ["soxr"]`).
The pure-Rust rubato backend, which the default feature set selects, passes the
same 27 quick-run quality gates on this machine; that bench explicitly requests
UltraHigh, while route-specific tests separately enforce the High half-band's
20 kHz gain, THD+N, interpolation images, lifecycle, and zero-allocation
contracts. Representative same-machine UltraHigh deltas (rubato column from
the 2026-07-25 one-sub-chunk FFT run; the previous UltraHigh sinc route
measured −216.2 dB THD+N and −208.1 dB alias attenuation):

| Metric | SoXR (opt-in) | rubato (default) |
| --- | ---: | ---: |
| Resampler THD+N, 44.1 kHz to 48 kHz | −187.0 dB | −204.9 dB |
| Passband max deviation, 20 Hz to 18 kHz | 0.0013 dB | 0.0000 dB |
| 20 kHz resampler gain | −0.0062 dB | −0.0017 dB |
| Worst fitted alias attenuation, 96 kHz to 48 kHz | −290.2 dB | −290.5 dB |

### EQ

The 10-band IIR biquad `Equalizer` target-response error at +6 dB is
**0.0000 dB max** at the 62 Hz, 1 kHz, and 8 kHz reference points. FIR EQ
design behavior (offline cost, phase modes, absolute gain preservation) is a
contract, documented under [Behavioral Contracts](#behavioral-contracts).

### Nonlinear DSP & listening rows

| Metric | Result |
| --- | ---: |
| Saturation threshold max jump / first-derivative mismatch | 1.416e-6 / 3.610e-4 |
| Saturation alias-energy reduction, Direct vs `Oversampled4x` Tube stress | +16.3 dB |
| Bauer crossfeed low/high levels (80 Hz / 2 kHz) | −17.73 / −27.27 dB |
| Bauer crossfeed low-minus-high separation | +9.54 dB |
| Crossfeed mix-change continuity delta | 0.000e0 (vs 5.762e-3 for a reset simulation) |
| Noise-shaper −140 dBFS changed fraction / non-finite stress outputs | 1.000 / 0 |

The saturation threshold uses a 0.05-full-scale C1 soft knee shared by the
direct, oversampled, and high-pass-exciter paths. The alias probe drives an
11 kHz Tube waveshaper and fits folded above-Nyquist harmonics. In the current
quick run, `Oversampled4x` reduced the aggregate fitted alias energy from
−15.09 dBFS to −31.42 dBFS at equivalent drive/mix settings.

The crossfeed follows the libbs2b-style low-pass/high-boost Bauer topology with
overload-prevention gain. `mix` is a dry-to-reference strength, and mix/cutoff
updates ramp over about 10 ms without clearing filter history. These listening-DSP
rows are synthetic probes after settling; they validate target response/effect
size and parameter-change continuity, not external listening-test or analog
output evidence.

The noise shapers (`NoiseShaper`) continuously dither every finite input,
including exact digital silence, and clamp to the signed target-bit range;
NaN/Inf clears only the affected channel history and returns zero. Shaping
redistributes quantization error rather than lowering broadband noise: the
curves strongly reduce the 2–6 kHz band while pushing energy into 14–18 kHz,
for up to a +34.8 dB ear-band advantage over flat TPDF dither.

### Convolution

Correctness: convolution results are validated against an overlap-save
reference (see [Behavioral Contracts](#behavioral-contracts) for routing
details). Realtime cost on the 2026-07-23 Windows pinned probe (logical core 2,
raised process/thread priority): the 65536-tap, 6-channel, 64-frame case
measured **16.74% p99** and **21.20% max** utilization of its 1.333 ms
deadline. The two pinned pre-change baselines measured 62.49–71.68% p99 and
78.74–82.73% max. Collect absolute max/p99 evidence on a quiet host:
externally loaded runs can still contain multi-millisecond scheduler pauses
even when affinity is fixed.

### Full-chain validation

The full output-chain true-peak probe is enforced as a gate
(`full_output_chain_worst_true_peak`, run with `--enforce` in CI). In the
current quick run the worst full-chain output true peak is **−1.000 dBTP**
with zero over-limit points across the probe corpus. Runs before the
2026-07-18 DSP lifecycle fixes measured −0.610 dBTP, 0.390 dB above the
−1 dBTP target; the gate guards exactly that failure mode.

In the offline render chain the limiter runs in the output-rate domain, after
any resampling, so only final quantization sits downstream of it (with its
derived output-ceiling guard). Full-output points also publish authoritative
rendered frames, algorithmic latency, retained semantic tail, and truncation
state from `RenderedOutput`; the default compensated timeline uses a
−120 dBFS pre-dither energy threshold, 250 ms continuous silence hold, and
30 s safety maximum for unknown or infinite tails.

---

## Realtime Safety

### Callback contract

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
outside it.

### Deadline model

A case deadline is `frames / 48,000 Hz`, and a callback is counted as missed
only when its retained duration exceeds that deadline. Full and heavy modes
retain 20,000 and 100,000 callbacks per case respectively; use them for deeper
machine-local evidence rather than routine shared CI.

### Tail latency

On the 2026-07-25 Windows pinned callback-tail candidate (4,000 callbacks per
case), at 512 frames: active DSP without convolution measured **72.8 μs p99**,
**104.9 μs p99.9**, and **419.5 μs max**; the active chain with the 256-tap
convolver measured **94.5 μs p99**, **113.1 μs p99.9**, and **144.4 μs max**.
All 48,000 callbacks in the complete matrix met their modeled deadlines. These
are same-machine, scheduler-inclusive observations, not device-callback or
cross-machine claims.

Unpinned callback-tail runs are report-integrity gates, including on shared
CI. Strict callback-tail timing comparison is Windows-only and requires both
reports to use `--pinned` with the same verified affinity/priority state.
Sub-microsecond bypass tails remain report-only because a 100 ns timer tick
can look like a 50% relative change; their raw samples and missed deadlines
are not trimmed or hidden. Processor affinity and elevated priority reduce
migration and ordinary contention, but they cannot exclude interrupts, DPC
activity, frequency changes, or other scheduler noise; those events remain in
the raw distribution.

### Allocation behavior

The callback path performs no allocation, locking, I/O, logging, or
panic-based control flow; allocation evidence is enforced by
`audio_lifecycle_memory_perf` (see [Performance](#performance)) and by
route-specific zero-allocation tests for the resampler and convolution paths.
Its allocation rows count only operations routed through Rust's global
allocator; native allocations that bypass it (e.g. inside libsoxr) are not
estimated by the crate-owned accounting.

### Lock-free control

The atomic parameter snapshots (`AtomicEqParams`, `AtomicVolumeParams`, and the
rest) are the mechanism for pushing parameter changes into the audio callback
without locks: control threads publish, the callback reads one coherent
generation snapshot per buffer. The measured cost (~7 ns per callback) is an
exploratory machine-local probe and is reported under
[Exploratory Measurements](#exploratory-measurements), not as a CI gate.

---

## Performance

### Processing budget model

For a 512-frame buffer at 48 kHz (about 10.7 ms of audio), even the heaviest
chain measured here uses well under one callback period:

```text
callback budget ≈ 10,666.7 μs (512 frames @ 48 kHz)
        │
        ├── DSP alone         51.5 μs   → 0.48% of budget (p95 util 0.51%)
        └── DSP + convolution 61.9 μs   → 0.58% of budget (p95 util 0.60%)
```

Per-sample/per-buffer cost of the DSP and resampler paths at a 512-frame
buffer. These exclude the decoder and the OS audio device write; they measure
only the in-crate processing.

| Path | Per sample | Per 512-frame buffer | Evidence |
| --- | ---: | ---: | --- |
| Isolated `SaturationQuality::Oversampled4x` Tube saturation | 22.9 ns | 23.4 μs | seven-trial quick median at 512 frames (2026-07-22); 24.0% below the compatible 30.1 ns fixed-dispatch baseline |
| DSP chain, no convolver (volume, EQ, `SaturationQuality::Oversampled4x`, Bauer crossfeed, convolver slot empty, dynamic loudness, peak limiter, noise shaper) | 50.3 ns | 51.5 μs | seven-trial quick median (2026-07-23); p95 callback utilization 0.51% |
| DSP chain with convolver and `SaturationQuality::Oversampled4x` | 60.4 ns | 61.9 μs | seven-trial quick median (2026-07-23); p95 callback utilization 0.60% |

### Resampling

Streaming cost (512-frame stereo buffers); the two project backends come from
separate core-pinned heavy builds, so their absolute values are informative
but not a controlled AB comparison. Each project's strict claim uses the raw
upstream control from its own run (see
[Resampler performance & Pareto updates](#resampler-performance--pareto-updates)).

| Case | SoXR (opt-in) | rubato selected route (default) |
| --- | ---: | ---: |
| 44.1 kHz to 48 kHz, ns/input sample (μs/input buffer) | 8.57 (8.77 μs) | 8.18 (8.38 μs) |
| 48 kHz to 44.1 kHz, ns/input sample (μs/input buffer) | 7.42 (7.60 μs) | 7.03 (7.19 μs) |
| 48 kHz to 96 kHz, ns/input sample (μs/input buffer) | not rerun in the strict control | 5.15 (5.27 μs; p95 6.32) |

`audio_resampler_matrix_perf` measures `StreamingResampler::process_checked`
across an intentional rate/quality/phase/channel set, plus construction
(`StreamingResampler::with_quality`) cost. It complements
`audio_resampler_streaming_perf` (default High/Linear path only) and does not
replace it. Quick mode is a fixed decision set (primary rates,
High/Standard/UltraHigh, Linear+Minimum, stereo+5.1, setup cost). Full/heavy
expands rates and quality ladders without a pure cartesian product. Build soxr
(default features) or rubato-only (`--no-default-features --features rubato`);
backend is recorded in case keys and environment features so baselines cannot
be mixed silently. Still excluded: decoder, device write, full DSP callback
chain, offline render chain (see the report `excludes` array), and exhaustive
pathological rate pairs.

### Convolution

| Path | Per sample | Evidence |
| --- | ---: | --- |
| `FFTConvolver` alone, 256-tap IR, stereo | 9.39 ns | seven-trial pinned quick median (2026-07-23) |
| FIR EQ apply, 511-tap IR via `FFTConvolver`, stereo | 10.9 ns (11.2 μs/512) | seven-trial quick median (2026-07-23); versioned `audio_fir_eq_perf --quick` report |

On 2026-08-13 `OverlapSaveConvolver` moved from a complex `rustfft` transform to
a real-input `realfft` transform, matching what `PartitionedConvolver` already
did. The absolute figures above predate that change and were taken on a quieter
machine, so they are left as recorded; the improvement was measured as a paired
A/B on one host instead, with interleaved `--quick` runs per side:

| Case | Change |
| --- | ---: |
| `FFTConvolver` throughput, `--quick --pinned`, every IR from 256 to 65,536 taps, 2 and 6 channels | −10% to −54% per sample (28 of 28 cases faster) |
| FIR EQ apply, 511-tap, stereo | −29% per sample |
| FIR EQ apply, 1,023-tap, stereo | −37% per sample |
| FIR EQ regeneration, 511 to 2,047 taps, linear and minimum phase | −6% to −35% per rebuild |

Convolver spectral storage also halves, from `fft_size` complex bins to
`fft_size / 2 + 1`.

### Decoder

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
| Local source open | 27.31 μs | warm filesystem cache; file open only |
| Container probe | 9.79 μs | 18,432-byte fixed decoder staging requirement |
| Decoder build | 9.19 μs | 23,328 retained Rust setup bytes in the measured scope |
| First borrowed PCM | 7.43 μs | non-empty, finite decoder-owned packet |
| Steady borrowed decode | 19.79 ns/frame | 50.52 million frames/s; 1052.5x realtime factor |
| Coarse seek command | 2.40 μs | raw 24-sample quick distribution includes p99/max |
| Coarse seek through first PCM | 9.00 μs | seek plus a validated non-empty packet |

These values are PCM/WAV, warm-cache, local-source evidence. Compressed-codec
and gapless behavior remains visible in `audio_gapless_comparison_perf`; cold
disk, live HTTP, cancellation responsiveness, and end-to-end playback are not
implied by the table.

### Components

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
54.42/108.18 ms for AutoMix Head/Full, and 8.08 μs/row for the 128-row SQLite
batch upsert. The JSON retains every case and raw trial. The spectrum figures
predate the 2026-08-13 `realfft` migration below, which moved that case to
~4.5 ns/sample.

The loudness figure above predates the 2026-08-13 metering change and is left as
recorded. `LoudnessMeter` previously asked `ebur128` for `Mode::all()` — which
enables that crate's own true-peak and sample-peak detectors, neither of which
this crate ever read, since it reports its own 4x polyphase FIR true peak — and
re-derived all four gating measurements inside every `process` call. Because
`ebur128`'s momentary and short-term readers rescan their whole 400 ms / 3 s
window per call, that cost was independent of the block just ingested and
dominated small blocks. The mode is now `I | LRA | HISTOGRAM` and the gating
readers query the backend on demand. Measured as a paired A/B on one host, with
interleaved `--quick` runs per side:

| Case | Change |
| --- | ---: |
| Loudness meter `process`, stereo, 512-frame blocks | −92% per input sample |
| Loudness meter `process`, stereo, 4,096-frame blocks | −67% per input sample |

`HISTOGRAM` is load-bearing and must stay enabled: `I | LRA | HISTOGRAM` is
bit-identical to `Mode::all()` across integrated, short-term, momentary, and
range, while dropping `HISTOGRAM` shifts integrated loudness by a few
millibels. `narrowed_mode_matches_mode_all_bit_for_bit` pins that equivalence
against a level-stepped fixture chosen so loudness range is non-zero.

#### Real-valued FFT call sites (2026-08-13)

The spectrum analyzer, the AutoMix spectral-flux accumulator, and the FIR EQ's
linear-phase IR design all fed real-valued data through complex `rustfft`
transforms and then read only half the result. They now use `realfft`, which was
already a dependency for the convolvers and the spectral resampler, so no
dependency changed.

Measured as interleaved paired A/B `--quick` runs on one host. Each table keeps
an **unchanged** case as an in-run control, because host drift over these runs
was comparable to some of the effects being claimed:

| Case | Change | Role |
| --- | ---: | --- |
| Spectrum `analyze`, 1,024-point / 64 bins | −12.4% per input sample | changed |
| Spectrum `analyze`, 4,096-point / 96 bins | −23.9% per input sample | changed |
| Downmixer 5.1→stereo, 512 frames | +2.2% | control |
| Downmixer 7.1→stereo, 512 frames | +0.7% | control |
| FIR EQ regeneration, linear phase, 511 taps | −7.1% | changed |
| FIR EQ regeneration, linear phase, 1,023 taps | −14.2% | changed |
| FIR EQ regeneration, linear phase, 2,047 taps | −16.7% | changed |
| FIR EQ regeneration, minimum phase, 511/1,023/2,047 taps | +2.3% / +5.6% / +4.9% | control |

The minimum-phase control moved *against* the linear-phase result across the
same runs, which is the reason for reporting it: the linear-phase gain is larger
than, and opposite in sign to, the drift affecting untouched code beside it.

AutoMix is deliberately **not** claimed as an improvement. An isolated harness
puts the accumulator itself at 1.02–1.08x faster, but interleaved end-to-end
AutoMix runs came out at −4.1%, +1.0%, and +1.0% — decode dominates that case,
so the transform change does not surface. A single non-interleaved pair had
suggested a 4.7% regression, which the interleaved runs identified as host
drift.

The change also removes a per-hop allocation: the accumulator previously called
`rustfft`'s `process`, which allocates scratch on every call, once per 512-sample
hop for the whole analyzed window. This is offline analysis, not the callback
path, so it was never a realtime-safety violation.

`fir_design.rs` keeps its complex transforms. Its real-cepstrum factorization
exponentiates a complex spectrum between the transforms, so the intermediate is
genuinely complex-valued and carries the Hilbert phase in its imaginary part;
only the endpoints are real. Equivalence to the previous formulation is pinned
by `spectrum_analyzer_matches_legacy_reference`,
`linear_phase_ir_matches_complex_reference_formulation`, and
`spectral_flux_matches_complex_reference_formulation`, each of which keeps its
oracle expressed as a complex FFT. Those comparisons use explicit relative
tolerances rather than bit-equality, since the two transforms fold the same sums
in a different order; observed agreement is ~1e-16 relative for the `f64` paths.

#### Reusing FFT plans instead of rebuilding planners (2026-08-13)

`FftPlanner::new()` returns an *empty* cache, so planning a transform repeats a
prime factorization, an algorithm selection, and a twiddle-factor precomputation
every time. Three call sites built a fresh planner on every call. At 8192 points,
`plan_fft_inverse` measured **171.3 us** from a cold planner against **0.072 us**
from a warm one. A planner also shares work across directions: cold
`inverse(8192)` is 392 us and a following `forward(8192)` only 271 us, so the
previous split between `minimum_phase_prototype` and
`minimum_phase_from_log_magnitude` planned the same size twice from two cold
caches.

The affected path is not only setup. `FirEq::regenerate_ir` runs on every
`set_band`, `set_bands`, `set_sample_rate`, `set_num_taps`, and `set_phase_mode`
— i.e. once per EQ slider movement. Interleaved before/after, three reps, median
of 9 trials each:

| Case | Change | Role |
| --- | ---: | --- |
| `FirEq::set_band`, 255 taps, linear / minimum | −42% / −56% | changed |
| `FirEq::set_band`, 511 taps, linear / minimum | −47% / −40% | changed |
| `FirEq::set_band`, 1,023 taps, linear / minimum | −25% / −51% | changed |
| Linear-phase resampler setup (no minimum-phase work) | −2% | control |

Every EQ case improved in every rep, with per-rep deltas spanning −16% to −68%
and no sign flips, while the control stayed inside −2..−5%.

Resampler setup is **not** claimed as an improvement. Collapsing two cold
planners into one is real in principle, but per-rep deltas for `48->192k High`
were −24%, −20%, +9%, and setup spread reached over 50% in an earlier attempt;
this host cannot resolve that effect at the sample size used.

The plan cache is owned by its user rather than kept in a global or
thread-local. `FirEq::new(f64, usize)` is public API with a caller-chosen,
unbounded `num_taps`, so a process-wide cache keyed by transform size would be a
caller-driven unbounded memory growth path.

Caching a plan is a pure performance change: `rustfft` and `realfft` contain no
`Cell`/`UnsafeCell`, and `process(&self, ..)` keeps all mutable state in
caller-supplied buffers, so a plan is read-only and a cache-hit plan was measured
to produce **bit-identical** output. `repeated_regeneration_is_bit_identical_to_a_fresh_instance`
and `interleaved_tap_counts_do_not_cross_contaminate_cached_plans` pin that with
exact equality rather than a tolerance; the latter was verified to fail when the
cache ignores its size key.

Holding a plan does cost an auto trait: `FirEq` becomes `!UnwindSafe` /
`!RefUnwindSafe`, because `dyn Fft` and `dyn ComplexToReal` do not declare
`RefUnwindSafe`. This is a deliberate, reviewed narrowing of the public surface,
and it matches `SpectrumAnalyzer` and `FFTConvolver`, which already hold plans and
were already `!UnwindSafe`. The regenerated baselines contain exactly those two
lines per feature set and no other change.

Note for anyone re-measuring: this code path needs the rubato backend, i.e. the
default feature set or `--no-default-features --features rubato`. Adding `soxr`
(including via `--all-features`) routes around it entirely, because SoXR wins the
backend priority.

### Lifecycle & memory

`audio_lifecycle_memory_perf` separates setup, reset, finish/drain, dynamic
Convolver ownership operations, and a bounded repeated lifecycle. Its 13-case
matrix keeps equal-rate setup/finish and timer-quantized ownership operations
visible while gating only seven stable timing cases against a compatible
baseline. The final SoXR quick run measured 457.5 μs active resampler setup,
381.9 μs reset, and 66.8 μs finish/drain for an 8,192-frame stereo input;
equal-rate setup/finish measured 112.0/4.1 μs and remain report-only.
Short/long Convolver setup was 28.4/353.5 μs, and the exact 255-frame short-IR
tail drained in 8.1 μs. Its five 128-cycle soak trials completed 640
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

Rows on this page should be regenerated after changing the relevant processing
path.

---

## Behavioral Contracts

These sections document non-measurement contracts that the evidence above
depends on. They are not benchmark results; they are the behavior the
benchmarks and tests enforce.

### AutoMix analysis contract

AutoMix analysis schema version 3 converts spectral-flux lag using the actual
`sample_rate / 512` observation cadence and derives lag bounds from the
supported tempo range. Musical-key detection is not implemented or claimed,
so the serialized result has no key status or reserved key payload fields. A
future key contract requires a detector validated against an independently
labeled music corpus rather than pre-freezing an always-empty DTO shape.

### FFT convolution routing

`FFTConvolver` keeps the existing overlap-save path for impulse responses up to
4096 taps per channel, which covers the current FIR EQ tap counts. Longer IRs
route to a uniform 1024-frame partitioned tail with an overlap-save head so
room/reverb-length responses avoid one very large callback FFT. Older tail
spectral passes are accumulated through a deterministic frame-position
schedule while the partition fills; the newest pass and inverse FFT complete
the next tail block at the boundary. This keeps the result independent of
callback chunking and leaves only preallocated, bounded work on the realtime
path.

The routing and partition size remain exposed as
`PARTITIONED_CONVOLUTION_IR_THRESHOLD` and
`PARTITIONED_CONVOLUTION_PARTITION_SIZE`; use `audio_convolver_perf` and
`audio_fir_eq_perf` before changing either value.

### FIR EQ IR generation

`FirEq` designs a linear- or minimum-phase impulse response from 10 band gains;
the IR is then convolved (typically with `FFTConvolver`) to apply the EQ.
Generation is an offline/control-thread cost, not a per-sample one. On this
machine a 511-tap linear-phase design has a seven-trial quick median of ~37 μs;
minimum-phase is ~114 μs because of the extra cepstral phase shaping, and cost
scales with tap count (`audio_fir_eq_perf`). The generated response preserves
absolute band gain: a uniform +6 dB curve remains +6 dB. A one-tap design is
explicitly a pure scalar at the 1 kHz reference (flat 0 dB is `[1.0]`).

### Resampler routing & geometry contracts

Exact 2x `Linear + High` upsampling uses a dedicated 127-tap symmetric
half-band FIR. Other common ratios use rubato 4.0's synchronous FFT engine at
every quality tier: UltraHigh selects one FFT sub-chunk (a 2x longer internal
FIR) while Low through High use two sub-chunks (2026-07-25 routing change).
Only ratios whose reduced components would create pathological FFT blocks use
windowed sinc. The shared adapter removes each linear engine's leading delay.

For `PhaseResponse::Minimum` and `Maximum`, the pure-Rust backend instead
builds an exact spectral (FFT block-convolution) rational resampler during
setup from the same low-pass magnitude target: real-cepstrum spectral
factorization produces the causal minimum-phase kernel, its reversal produces
the maximum-phase kernel, and the kernel's complex spectrum is applied through
an overlap-save input FFT, an exact decimation alias fold, and an inverse
output FFT. The fold keeps all `down` alias terms per bin, so the engine
matches the previous time-domain polyphase convolution to < 1e-9
(regression-tested across ratios, tiers, and both phases). The engine accepts
only reduced rate components up to 1024; unsupported geometry returns a typed
initialization error instead of silently using linear phase. Its reported
algorithmic latency and finite tail preserve the actual causal response.
Tests cover phase-energy ordering, magnitude preservation, 20 kHz gain,
THD+N, alias rejection, arbitrary chunking, reset, drain, and no allocation
after setup. Both backends share the streaming cursor and terminal-reset
contract.

---

## Regression & Baselines

### Baseline compatibility

Baseline comparison rejects mismatched schema, probe, Rust target/compiler,
OS/architecture, CPU, Cargo profile, feature set, mode, conditions, or case
set; an unavailable required environment field is also rejected. Revision and
dirty state are recorded but may differ. Aggregate reports retain every trial
plus min/median/nearest-rank p95/max; the tail report retains every callback
and extends the distribution through p99.9. All include the complete build
environment. Benchmark reports record the compiled backend in the environment
`features` field (`resampler-soxr` / `resampler-rubato`) and in the
`algorithm` labels, so performance baselines recorded before backend labeling
are incompatible with new reports.

### Regression thresholds

The default gate allows exactly **10% median** regression and fails above it.
The active callback-chain defaults allow **10% median, 20% p99, and 30% p99.9**
regression. The three coverage probes also accept `--baseline` and the shared
10% median limit. Their case keys and conditions include fixture identity,
feature set, workload geometry, and backend, so a PCM fixture cannot be
compared with a different codec corpus and a SoXR lifecycle report cannot be
compared with a Rubato report.

### Pinned measurements

On Windows, the convolver's opt-in `--pinned` mode records the logical core in
report conditions and additionally enforces the machine-local 65536-tap,
6-channel, 64-frame p99/max callback gates. Pinned and unpinned reports are
baseline-incompatible.

---

## Reproducibility

### Workloads

Omit `--quick` for the full workload, or pass `--heavy` to performance probes
that advertise it. The quality bench uses quick/full only. Quick mode is the
routine shared-CI workload; full and heavy modes exist for deeper
machine-local evidence.

### JSON reports

The standardized evidence entry points write versioned JSON reports:

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

Baseline comparison example (callback tail, Windows pinned):

```bash
cargo bench --bench audio_callback_tail_perf -- --quick --enforce --pinned --pin-core 2 \
  --out target/bench-reports/callback-tail-baseline.json
cargo bench --bench audio_callback_tail_perf -- --quick --enforce --pinned --pin-core 2 \
  --baseline target/bench-reports/callback-tail-baseline.json \
  --out target/bench-reports/callback-tail-candidate.json
```

---

## CI Enforcement

GitHub's default-feature shared runner generates and uploads nine quick JSON
artifacts; the pure-Rust job additionally runs decoder, component, and
lifecycle-memory quick reports with `--no-default-features --features rubato`.
The fixture-driven gapless comparator remains outside CI. Unpinned
callback-tail runs act as report-integrity gates on shared CI. Neither job
imposes a cross-machine absolute nanosecond threshold.

---

## Scope & Limitations

This document does **not** measure:

- DAC latency
- driver latency
- CPAL/WASAPI buffer negotiation and device callback scheduling
- end-to-end play-button-to-sound latency
- network streaming latency
- cold filesystem performance
- subjective listening quality
- cross-machine absolute performance

None of these probes owns or opens an audio device. CPAL/WASAPI buffer
negotiation, device callback scheduling, driver latency, DAC latency, and
user-visible play-button-to-sound latency require a consuming-application
integration benchmark and remain outside this crate's evidence boundary.

Additional limits already stated inline: all timing is same-machine evidence
(likely to differ by CPU, compiler version, and load); scheduler noise remains
in raw distributions even under affinity/priority pinning; native allocator
accounting excludes allocations made outside Rust's global allocator;
single-machine results are not universal hardware guarantees.

---

## Detailed Measurements

The sections above carry the summary tables and all gate contracts. This
section retains the deeper per-run evidence: resampler Pareto updates, routing
comparisons, and offline-render measurements.

### Resampler performance & Pareto updates

#### 2026-07-27 adapter Pareto update

Stereo SoXR v2 replaces two native mono states plus deinterleave/reinterleave
scratch with one native interleaved state. Against the former project route,
steady/setup/reset/drain improved by 56.16/44.45/47.73/58.28% for 44.1-to-48
kHz and 39.42/33.68/44.30/49.69% for 48-to-44.1 kHz. Five pinned 15-trial
heavy reports compare v2 with raw stereo libsoxr under the same f64 HQ/Bits20
recipe. The median of the five per-run deltas is +1.73% forward and −1.94%
reverse, so both directions are classified as statistically tied rather than a
universal win. The reports retain one +11.11% reverse steady outlier, one
+4.27% forward outlier, and one +6.40% reverse drain outlier; none repeated in
the adjacent confirmations. Median-of-run setup/reset/drain deltas remain
between −3.48% and +0.70%.

The final complete 11-engine confirmation measured SoXR v2 / raw libsoxr at
8.569 / 8.632 ns/input-sample forward and 7.424 / 7.368 reverse. Those deltas
are −0.73% and +0.75%, preserving the tied conclusion. All 22 cases were valid,
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
−19.93 dB and −42.87 dB reverse alias attenuation versus −232.81 dB for the
Rubato route.

#### Half-band and FFT routing comparisons

Against a same-revision retained-FFT baseline, the 48-to-96 half-band route
reduced 128/256/512-frame `process_checked` medians from
36.104/14.354/17.667 to 5.849/5.807/6.026 ns/input sample (83.8%, 59.5%, and
65.9%). All cases passed consumed/produced and finite-output work validation.

In the 2026-07-25 same-machine paired quick matrix
(`audio_resampler_matrix_perf`, rubato backend, 512-frame stereo
`process_checked`), routing UltraHigh Linear onto the one-sub-chunk FFT
engine cut 44.1-to-48 kHz from 101.13 to 8.15 ns/input sample (~12.4x) and
48-to-96 kHz from 163.87 to 10.22 (~16x), with median setup falling from
5.82/6.26 ms to 0.16/0.20 ms; no other case regressed beyond run noise. In the
same matrix, the 48-to-96 kHz High/Minimum spectral route fell from 1098.5 to
15.5 ns/input sample (~71x) and 44.1-to-48 kHz High/Minimum from 604.0 to
191.4 ns/input sample (~3.2x); no Linear-path case regressed beyond run noise.

### Offline render evidence

`OutputRenderChain` deliberately requests UltraHigh; since the 2026-07-25
routing change, pure-Rust resampled offline rendering uses the one-sub-chunk
FFT route instead of sinc. In the 2026-07-25 quick probe, the active
44.1-to-48 kHz 4096-frame render measured 103.02 ns/input sample for a
one-second input and 82.92 for five seconds (0.91% and 0.73% realtime
factors), compared with 353.97 and 266.38 (3.12% and 2.35%) for the retired
UltraHigh sinc route in the 2026-07-22 quick probe.

---

## Exploratory Measurements

These measurements are useful for optimization guidance but are **not
release-quality evidence**: they carry no JSON artifact, no environment
identity, and no baseline contract, and they are not run in CI.

### Lock-free parameter reads (`audio_lockfree_params_perf`)

Reading the full set of cached parameters once per callback costs about
**7 ns** with the generation-based snapshot path, versus ~50 ns for a naive
split-atomic field-by-field read and ~83 ns for an unconditional `ArcSwap`
guard load — an ~86% to ~92% improvement.

The `ArcSwap` figure is a historical comparison against the crate that used to
back the control-side snapshot store. As of 2026-08-13 `arc-swap` is no longer a
dependency: the control side holds `Mutex<Arc<T>>`, which is never touched by the
audio callback, and the realtime path is unchanged.

`audio_lockfree_params_perf` is a machine-local exploratory probe, not a
report-backed evidence gate. It emits no JSON artifact and carries no
environment, case-key, or baseline identity, and it is not run in CI. Its
`--enforce` mode asserts a fixed 3% same-run improvement of the lock-free path
over its mutex reference from a single wall-clock sample, so a red result
there is a hint to re-measure rather than a traceable regression.

### Sub-microsecond callback tails

Sub-microsecond bypass tails remain report-only because a 100 ns timer tick
can look like a 50% relative change; their raw samples and missed deadlines
are not trimmed or hidden.

### Equal-rate lifecycle rows

Equal-rate resampler setup/finish (112.0/4.1 μs on the final SoXR quick run)
and timer-quantized Convolver ownership operations remain report-only; only
the seven stable timing cases gate against a baseline.