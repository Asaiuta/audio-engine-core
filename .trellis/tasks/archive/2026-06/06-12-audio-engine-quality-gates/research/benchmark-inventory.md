# Benchmark Inventory

Inventory of the six `harness = false` custom-main benches in `benches/`, with
method, the metric(s) each emits, the current README/observed value where one is
published, the threshold this task enforces (if any), and whether the metric is a
**gate** (fails the run via `--enforce`) or **report-only** (printed/serialized
evidence, never fails the run).

Margins are deliberately conservative: the quality-gates scope notes warn that
overly strict numeric thresholds are flaky across CPUs, compiler versions, and
debug/release settings. Where a metric is deterministic (bit-exact parity with a
reference library) the gate is tight; where it depends on float/FFT/timing it is
either loose or left report-only.

---

## 1. `audio_quality_measurements` (focus of this task)

Method: offline synthetic f64 signals -> this crate's processor modules ->
numeric analysis (least-squares sine fit, FFT band RMS, EBU R128 comparison,
4x FIR true-peak meter). No device, no network. `--quick` shrinks the analysis
frame count (65 536 vs 262 144).

| Metric | Method | Current value (README / observed) | Threshold | Gate vs report | Margin rationale |
| --- | --- | ---: | --- | --- | --- |
| Resampler THD+N 44.1k->48k | sine fit residual/signal | -187 dB | must be `<= -100 dB` | **gate** | Floor is ~-187 dB; -100 dB leaves ~87 dB of headroom for slower/debug builds and FFT/fit noise. |
| Passband max deviation 20 Hz-18 kHz | single-tone amplitude fit | 0.0013 dB | `<= 0.10 dB` | **gate** | SoXR VHQ is essentially flat; 0.10 dB is ~77x the observed deviation and a normal audio passband spec. |
| Resampler worst alias attenuation 96k->48k | folded-tone fit | -294.7 dB | `<= -100 dB` | **gate** | Observed ~-295 dB; -100 dB is a wide, config-robust stopband floor. |
| Limiter output margin to threshold | sample-peak ceiling | -1.00 dBFS (0.00 margin) | margin `<= 0.05 dB` | **gate** | Sample-peak ceiling is deterministic; 0.05 dB absorbs quantization without allowing a real overshoot. (Was 0.01; loosened slightly for cross-config float robustness.) |
| Limiter below-threshold THD+N | sine fit on transparent pass | -238.3 dB | report-only | report | Useful transparency evidence, but the exact figure shifts with fit window and build; not a stable gate. |
| Noise-shaper ear-band advantage | FFT band RMS, shaped vs TPDF | up to +34.9 dB | best advantage `>= 3.0 dB` | **gate** | Direction (energy moved out of the ear band) is the real invariant; magnitude varies with the random dither sequence, so the gate only asserts a clear, conservative minimum (was 6.0; 3.0 holds across seeds/builds). |
| LoudnessMeter integrated parity vs `ebur128` | same buffer, both meters | 0.000000 LU | `<= 1e-6 LU` | **gate** | The wrapper forwards to `ebur128`; parity is bit-deterministic, so a tight gate is correct. |
| LoudnessMeter momentary/short-term/LRA parity | same buffer, both meters | ~0 LU | `<= 1e-6 LU` | **gate** | Same deterministic-parity reasoning. |
| LoudnessMeter true-peak parity vs `ebur128` | 4x FIR vs reference | ~0 dB | report-only | report | Both use FIR true-peak but can differ by tiny amounts at config edges; kept report-only to avoid flakiness. |
| EBU 3341/3342 global loudness error | corpus expected-value | n/a (corpus absent here) | `<= 0.1 LU` when present | **gate when present**, else **skipped** | Tolerance from EBU spec; only enforced if reference vectors exist, otherwise reported as skipped (never a silent pass). |
| EBU LRA error | corpus expected-value | n/a | `<= 1.0 LU` when present | **gate when present**, else **skipped** | EBU LRA tolerance. |
| EBU max-momentary / max-short-term error | corpus expected-value | n/a | `<= 0.1 LU` when present | **gate when present**, else **skipped** | EBU spec tolerance. |
| EBU true-peak input error | 4x FIR vs corpus expected | n/a | `-0.4 .. +0.2 dB` when present | **gate when present**, else **skipped** | EBU 3341 true-peak tolerance band. |
| Full output-chain worst true peak | limiter->resampler->24-bit shaper->f32->meter | close to -1 dBTP | report-only | report | The documented limitation: sample-peak limiter is not an intersample-true-peak guarantee. Stays report-only until the true-peak limiter task proves otherwise. |

## 2. `audio_callback_chain_perf`

Method: rebuilds the lock-free `DspChain` (EQ, saturation, crossfeed, convolver
slot, volume, dynamic loudness, limiter), best-of-N timing across buffer sizes.

| Metric | Current value | Threshold | Gate vs report |
| --- | ---: | --- | --- |
| `ns_per_sample` per scenario/frame | ~18 ns (no convolver), ~28 ns (convolver) | none (timing); `--enforce` only asserts finite/positive at one point | report (timing); validity check only |

Timing numbers are machine-specific and intentionally not numeric gates.

## 3. `audio_resampler_streaming_perf`

Method: `StreamingResampler` across rate ratios and three API paths, best-of-N.

| Metric | Current value | Threshold | Gate vs report |
| --- | ---: | --- | --- |
| `ns_per_input_sample` | ~7.9 ns (44.1k->48k) | none; `--enforce` asserts finite timing + output produced | report (timing); validity check only |

## 4. `audio_convolver_perf`

Method: `FFTConvolver` vs a bench-local legacy baseline, median of trials.

| Metric | Current value | Threshold | Gate vs report |
| --- | ---: | --- | --- |
| `process_into` / `process_inplace` ns/sample, speedup % | ~10 ns | `process_inplace <= process_into * 1.25` under `--enforce` | report + one relative gate |

The only gate is a *relative* regression guard (inplace vs into), which is
config-robust because both paths run on the same machine in the same run.

## 5. `audio_fir_eq_perf`

Method: FIR IR regeneration cost + end-to-end apply via `FFTConvolver`.

| Metric | Current value | Threshold | Gate vs report |
| --- | ---: | --- | --- |
| `ns_per_regen`, `ns_per_sample` | ~31 us regen, ~9.6 ns apply | none; `--enforce` asserts finite/positive | report (timing); validity check only |

## 6. `audio_lockfree_params_perf`

Method: cached generation-snapshot reads vs legacy split-atomic and ArcSwap-guard
baselines.

| Metric | Current value | Threshold | Gate vs report |
| --- | ---: | --- | --- |
| `ns_per_read`, improvement % | ~7 ns, ~86-92% improvement | `steady improvement >= 3.0%` under `--enforce` | report + one relative gate |

The gate is a *relative* improvement floor (3%), far below the observed ~86%, so
it survives noisy CPUs while still catching a real regression of the snapshot
mechanism.

---

## Summary

- Hard numeric gates live only where the metric is deterministic parity
  (`ebur128` loudness) or has enormous headroom (resampler THD+N, alias floor,
  passband deviation, limiter ceiling).
- Timing benches expose values but gate only on validity or *relative*
  regressions, never absolute ns thresholds, because those are CPU/compiler/build
  specific.
- The full output-chain true-peak probe and limiter below-threshold THD+N stay
  report-only, matching the README's visible limitation note.
- EBU corpus checks are gates only when the reference vectors are present;
  otherwise they are reported as skipped, never silently passed.
- This task wires the `audio_quality_measurements` gate/report split explicitly
  into the JSON evidence and the `--enforce` diagnostics (metric name +
  measured-vs-threshold), so README values are traceable to a named gate.
