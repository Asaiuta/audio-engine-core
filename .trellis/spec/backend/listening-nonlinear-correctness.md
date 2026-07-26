# Listening & Nonlinear DSP Correctness

> Executable contracts for saturation, final quantization/noise shaping, and
> Bauer headphone crossfeed. Read this with `realtime-safety.md` and the
> saturation quality-mode section of `quality-guidelines.md`.

## 1. Scope / Trigger

This spec applies when code:

* changes a saturation transfer, threshold, dry/wet/output-gain order, quality
  mode, or high-pass exciter path;
* changes noise-shaper dither, target bit range, error history, invalid-sample
  recovery, or adapter parameter publication;
* changes crossfeed topology, reference profile, parameter smoothing, reset,
  sample rate, or channel-layout behavior;
* changes the listening/nonlinear metrics in
  `benches/audio_quality_measurements.rs`.

These processors intentionally change samples. Compatibility with a defective
transfer or frequency direction is not a correctness oracle.

## 2. Signatures

Relevant direct and callback-facing signatures include:

```rust
Saturation::set_threshold(&mut self, threshold: f64)
Saturation::set_output_gain(&mut self, gain_db: f64)
Saturation::set_quality(&mut self, quality: SaturationQuality)
Saturation::process_with_channels(&mut self, samples: &mut [f64], channels: usize)
Saturation::process_with_channels_mix(
    &mut self,
    samples: &mut [f64],
    channels: usize,
    effect_weight: f64,
)
SaturationProcessor::process_with_events(
    &mut self,
    samples: &mut [f64],
    channels: usize,
    events: &[SaturationEvent],
) -> Result<ProcessProgress, ProcessError>
AtomicSaturationParams::set_armed(&self, armed: bool)

NoiseShaper::set_bits(&mut self, bits: u32)
NoiseShaper::set_curve(&mut self, curve: NoiseShaperCurve)
NoiseShaper::process_sample(&mut self, sample: f64, ch: usize) -> f64
NoiseShaperCurve::quantization_error_bound(self, bits: u32) -> f64

Crossfeed::set_mix(&mut self, mix: f64)
Crossfeed::set_cutoff(&mut self, cutoff_hz: f64)
Crossfeed::set_sample_rate(&mut self, sample_rate_hz: f64, cutoff_hz: f64)
Crossfeed::process(&mut self, samples: &mut [f64], channels: usize)

AtomicSaturationParams::set_quality(&self, quality: SaturationQualityValue)
AtomicCrossfeedParams::set_mix(&self, mix: f64)
AtomicCrossfeedParams::set_cutoff(&self, hz: f64)
AtomicNoiseShaperParams::set_bits(&self, bits: u32)
AtomicNoiseShaperParams::set_curve(&self, curve: NoiseShaperCurve)
PeakLimiterProcessor::new_with_output_guard(...)
PeakLimiterProcessor::output_ceiling_guard_db(&self) -> f64
```

## 3. Contracts

### Saturation transfer and gain order

Direct, 2x, 4x, and high-pass-exciter modes use one threshold transfer. For
input `x`, threshold `t`, fixed knee width `k = 0.05`, and the selected driven
base shape `s(x)`:

```text
e = abs(x) - t
e <= 0: y = x
e > 0:  p = min(e / k, 1)
        w = p^2 * (3 - 2p)
        y = x + (s(x) - x) * w
```

Because `w(0) = 0` and `w'(0) = 0`, the transfer is value- and
first-derivative-continuous at both positive and negative thresholds. Do not
put a different threshold branch into an oversampled or exciter path.

For a hard-bypassed processor, output is the bit-exact current input with zero
latency and no state work. An armed processor keeps a four-source-frame
timeline in every quality mode. Let `d` be delayed raw input, `g` delayed
input-gain output, and `r` the filtered nonlinear residual. The final order is:

```text
processed = (g + mix * r) * output_gain
output = d + effect_weight * (processed - d)
```

For Oversampled2x/4x, `r` is the decimated FIR of
`waveshaped(interpolated) - interpolated`; FIR history advances at every
oversampled phase, but the dot product is evaluated once per source frame.
Below threshold with unity gains the enabled output is bit-exact delayed dry
for every mix and chunking. Runtime effect-enable and quality automation uses
complementary smoothstep weights over 32 source frames, with sparse sorted
events borrowed for one process call. Output gain therefore applies below and
above threshold and at every mix value. Per-channel state is sized during
setup; hard bypass applies neither gain nor state work.

### Noise shaping and signed quantization

Every finite sample on a configured channel is quantized while enabled,
including exact zero and signals below -120 dBFS. There is no amplitude gate
and no silence-triggered history reset. TPDF is signal-independent; shaped
curves add the existing bounded error feedback before rounding.

For signed `N`-bit output, `scale = 2^(N-1)`, the integer result is clamped to
`[-scale, scale - 1]`, and the normalized result is in
`[-1.0, 1.0 - 1/scale]`. A non-finite input returns `0.0` and clears only that
channel's 5-/9-tap history; it must not poison another channel or later finite
samples.

The callback adapter compares snapshots field by field. Enabled/bit-depth
updates do not clear curve history. It calls `set_curve` only when the curve
actually changes, because a real curve transition deliberately starts with
clean feedback history.

The final floating-point limiter runs once in the output-rate domain. Its
internal threshold is derived from bounded downstream error rather than a
fixed audio-sized margin:

```text
guarded_linear = target_linear
    - (curve.quantization_error_bound(bits) + 0.5 * f32::EPSILON)
      * true_peak_reconstruction_l1_bound()
```

The derived guard is finite, positive, and monotonic (less headroom at higher
bit depth) for every supported noise-shaper curve and 8--32 bit setting. The
terminal noise shaper/quantizer follows that one limiter exactly once.

### Bauer crossfeed topology and state

The full reference is the libbs2b-style first-order low-pass cross path plus
same-channel high boost and overload-prevention gain:

```text
L_ref = gain * (highboost(L) + lowpass(R))
R_ref = gain * (highboost(R) + lowpass(L))
output = dry + mix * (reference - dry)
```

The fixed reference feed is 4.5 dB; the default cutoff is 700 Hz. `mix` means
dry-to-reference strength, not a raw cross-channel coefficient: zero is exact
bypass and one is the complete reference response. At the reference response,
low-frequency crossfeed is stronger than high-frequency crossfeed.

Mix and cutoff retarget over approximately 10 ms per sample and preserve filter
history. Cutoff redesign changes coefficient geometry but is not a new stream.
A sample-rate change enters a new rate domain: snap new coefficients, complete
the scalar target, and clear old-rate signal history. `reset` clears history and
finishes pending ramps. Mono and layouts with more than two channels are exact
bypasses.

All three processors remain allocation-, lock-, log-, I/O-, panic-, and
unbounded-work-free in steady-state callback processing.

## 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Saturation `abs(x) <= threshold` while enabled | Identity transfer before final dry/wet output gain |
| Saturation crosses either threshold | No value jump; left/right first derivative agrees within the deterministic gate |
| Saturation hard bypass | Bit-exact current input; zero latency and no stateful processing |
| Armed saturation effect disabled at runtime | Four-frame delayed timeline remains; only bounded delay/history work runs after the 32-frame fade |
| Saturation quality event | Starts at the exact block-relative frame; duplicate offsets apply in slice order |
| Unsorted/out-of-range saturation events | Typed `ProcessError::InvalidAutomation`; no DSP mutation |
| Saturation channel state undersized | Setup defect is detected; hot path does not resize |
| Finite noise-shaper input, including zero/-140 dBFS | TPDF/quantization remains active |
| Positive signed full scale | At most `1.0 - LSB`, never `+1.0` |
| Negative signed full scale | At least `-1.0` |
| Noise-shaper NaN or infinity on channel `c` | Output zero; clear only channel `c` history |
| Adapter updates only enabled or bits | Preserve existing curve history |
| Crossfeed mix is zero | Exact stereo bypass |
| Crossfeed channels are not two | Exact bypass |
| Non-stereo Crossfeed finish | `TailSpec::None` and immediate `Finished(0)` |
| Crossfeed mix/cutoff update | Preserve history and ramp to the target |
| Crossfeed sample-rate update or reset | Clear prior-stream/rate history and snap/finish parameter state |
| Final limiter guard | Derived from quantizer/dither/reconstruction bounds; never arbitrary audio headroom |

## 5. Good / Base / Bad Cases

* Good: Tape, Tube, and Transistor all use the shared smoothstep knee in direct,
  oversampled, and exciter processing; oversampled modes filter only the
  nonlinear residual and apply output gain once.
* Base: TPDF-only quantizes digital silence; individual output samples may be
  zero, but the stream is not bypassed by amplitude.
* Good: a cutoff update continues from the existing Bauer filter state while
  coefficients move to the new target over about 10 ms.
* Bad: return the input below a fixed dBFS threshold, because this disables
  dither exactly where quantization distortion is most exposed.
* Bad: clamp normalized noise-shaper output to `[-1, +1]`; signed PCM has one
  fewer positive code and requires `1 - LSB` as its upper bound.
* Bad: reset crossfeed on every mix/cutoff snapshot, because the discontinuous
  state change defeats parameter smoothing and can click.

## 6. Tests Required

* Saturation unit tests cover every type and quality mode, both threshold signs,
  non-zero output gain, high-pass mode, reset/rate changes, finite bounds, and
  no steady-state allocation.
* Quality gates include `saturation_threshold_transfer_jump`,
  `saturation_threshold_first_derivative_mismatch`, and the existing
  `saturation_oversampled4x_alias_reduction` plus the wanted-signal
  `saturation_oversampled4x_fundamental_delta` gate.
* Saturation adapter tests cover exact event-frame starts, 32-frame smoothstep
  continuity, overlapping three-mode weights, hard-bypass setup/reset, and
  finite delay/FIR drain.
* Noise-shaper tests cover exact silence, -140 dBFS, signed full-scale and
  overload inputs, every curve, NaN/infinity, channel-local recovery, quantizer
  grid/bounds, unchanged-curve adapter updates, and no steady-state allocation.
* Quality gates include `noise_shaper_low_level_changed_fraction`,
  `noise_shaper_stress_peak`, and `noise_shaper_stress_non_finite_outputs`.
* Crossfeed tests cover the independent 4.5 dB DC reference, low-vs-high
  direction, zero-mix and non-stereo bypass, chunk-independent ramps,
  preserved-state adapter comparisons, sample-rate/reset isolation, and no
  steady-state allocation.
* Quality gates include the Bauer DC error, low-vs-high separation, first-frame
  mix delta, preserved-history delta, and a reset-simulation control that proves
  the continuity test would catch the old behavior.

## 7. Wrong vs Correct

### Wrong

```rust
if sample.abs() < 1.0e-6 { return sample; }
let output = quantized.clamp(-scale, scale) / scale;

crossfeed.set_cutoff(new_cutoff);
crossfeed.reset();

let output = if dry.abs() <= threshold { dry } else { shape(dry) * output_gain };
```

### Correct

```rust
let quantized = (sample * scale + feedback + tpdf).round();
let output = quantized.clamp(-scale, scale - 1.0) / scale;

crossfeed.set_cutoff(new_cutoff); // ramp coefficients; retain signal history

let residual = decimate(waveshaped(interpolated) - interpolated);
let processed = (delayed_dry + mix * residual) * output_gain;
let output = delayed_raw + effect_weight * (processed - delayed_raw);
```

## 8. Hybrid Nonlinear-Phase Resampling Engines (2026-07-26)

Nonlinear phases (`Minimum`/`Maximum`) on the pure-Rust route share one exact
rational kernel but select one of two execution engines from the reduced
interpolation factor `up = to_rate / gcd(from_rate, to_rate)`:

* `up <= 16` uses `SpectralNonlinearResampler`; `up > 16` uses
  `ContiguousPolyphaseResampler`. Both enforce reduced `up` and `down` no
  greater than 1024 plus the shared coefficient-bank bound. Unsupported
  geometry is rejected and never falls back to a Linear engine.
* The spectral engine is overlap-save: forward real FFT at `Nin = 2·nin`
  (`nin = down·s >= taps_per_phase`, which guarantees no circular aliasing),
  one precomputed fold `Y[k] = scale · Σ_{m<down} H[k + m·Nout_full] ·
  X_ext[(k + m·Nout_full) mod Nin_full]` (the exact multirate decimation
  identity), inverse real FFT at `Nout = 2·nout` (`nout = up·s`).
  `scale = up / (down·Nout_full)` folds interpolation gain, alias average, and
  inverse-FFT normalization exactly once — never rescale elsewhere.
* The contiguous engine keeps planar channel history and reversed contiguous
  phase coefficients. Its retained head is
  `taps_per_phase - 1 + ceil((down - 1) / up)`, which covers the maximum
  rational lag at a chunk boundary. Every history offset and window end uses
  checked arithmetic and returns a static callback-safe backend error on an
  impossible range; it never relies on unsigned wrap or a hot-path panic.
* The kernel comes from the shared design
  (`design_linear_prototype` → `minimum_phase_prototype`; Maximum = reversed);
  `latency_frames` (phase peak) and `finish_extension_frames` ((L−1)/down)
  formulas are shared with the retired polyphase oracle and asserted equal.
* Both engines pace output from cumulative integer arithmetic and are eligible
  for the adapter's `prefix_budget_direct` path. Neither may over-emit versus
  `round(processed_real_input · up/down)`, and drain/reset retain the existing
  causal latency and finite-tail contract.
* Construction selects the stereo contiguous dot-product function once.
  AVX2 uses four independent multiply/add lanes without FMA so its reduction
  order is bit-equal to the scalar four-accumulator kernel; callback processing
  performs no feature detection. Mono and non-stereo layouts use the scalar
  contiguous kernel.
* `PolyphaseResampler` is retained `#[cfg(test)]`-only as the parity oracle;
  changes to either production engine must keep max error `< 1e-9` across
  representative ratios, tiers, and both phases. Tests additionally require
  timing equality, mono/stereo bit equality, scalar/AVX2 bit equality,
  irregular chunking, finite drain, reset isolation, and process/finish
  no-allocation after setup.
