# AutoMix Analysis and FIR Design Correctness

> Executable contracts for AutoMix tempo/key output and FIR-EQ impulse-response
> design. Read this before changing `automix_analysis.rs`, `fir_eq.rs`, or the
> FIR performance evidence path.

## 1. Scope / Trigger

This spec applies when code changes:

* spectral-flux window/hop geometry, tempo lag search, or `AutomixAnalysis`;
* musical-key fields or their serialization contract;
* FIR tap normalization, windowing, phase conversion, or band interpolation;
* `audio_fir_eq_perf`, its JSON schema, CI invocation, or documented FIR
  timing values.

AutoMix and FIR generation are offline/control-thread work. They may allocate,
but they must not move FFT design or report construction into an audio callback.

## 2. Signatures

```rust
pub fn detect_bpm(
    values: &[f32],
    observation_rate_hz: f64,
) -> (Option<f64>, Option<f64>, Option<f64>);

#[non_exhaustive]
pub enum AutomixKeyStatus {
    Unsupported,
}

pub struct AutomixAnalysis {
    pub version: u32,                 // current schema: 2
    pub key_status: AutomixKeyStatus, // serialized as "unsupported"
    pub key_root: Option<i32>,
    pub key_mode: Option<i32>,
    pub key_confidence: Option<f64>,
    pub camelot_key: Option<String>,
    // ... timing/loudness/mix fields unchanged
}

FirEq::new(sample_rate_hz: f64, num_taps: usize) -> FirEq
FirEq::set_bands(&mut self, gains_db: &[f64; 10])
FirEq::set_phase_mode(&mut self, mode: FirPhaseMode)
FirEq::get_ir(&self, channels: usize) -> Vec<f64>
```

```bash
cargo bench --bench audio_fir_eq_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]
```

## 3. Contracts

### Tempo observation domain

`SpectralFluxAccumulator` uses a 1,024-sample FFT and a 512-sample hop. The
spectral observation rate is always `sample_rate / 512`; the 50 Hz constant is
only the RMS-envelope rate. The hop has one source-of-truth constant shared by
buffer overlap and tempo conversion.

`detect_bpm` converts lag with:

```text
bpm = 60 * observation_rate_hz / lag
```

Lag bounds are derived from the supported approximately 55..200 BPM range, not
fixed sample counts. Normalized autocorrelation chooses the shortest qualifying
local harmonic peak so an integer-aligned 60 BPM multiple does not hide a
non-integer 120/180 BPM fundamental. Invalid rates, insufficient input, and
flat energy return no result rather than a fabricated tempo.

### Key capability honesty

Analysis schema version 2 reports `key_status = "unsupported"`. While that is
the status, root/mode/confidence/Camelot fields are all null. Reserved root
encoding is 0=C through 11=B; reserved mode encoding is 0=major, 1=minor.

Do not add a `Detected` or low-confidence status from synthetic chords alone.
A real detector requires independently labeled music-corpus evidence, tuning
and harmonic/segmentation policy, and calibrated confidence behavior.

### FIR magnitude and phase

* A one-tap FIR is the pure scalar at the existing 1 kHz reference. Flat 0 dB
  is `[1.0]`; uniform `g` dB is `[10^(g/20)]`; a non-uniform curve necessarily
  collapses to its 1 kHz value.
* Multi-tap frequency sampling preserves absolute requested magnitude. Never
  multiply the completed IR by the inverse 1 kHz target gain: doing so erases
  every uniform boost or cut.
* The minimum-phase real-cepstrum path keeps its `1/N` inverse-FFT scaling. Its
  raised-cosine tail is unity through the midpoint, then monotonically decays
  to exactly zero at the final tap. A zero-then-rising tail is reversed.
* FIR design and IR allocation remain setup/control work. `FFTConvolver` owns
  the callback apply path and its routing thresholds remain unchanged unless
  separate convolution evidence approves a change.

### FIR performance evidence

The report follows the shared schema/environment/baseline rules in
`quality-guidelines.md`. It contains nine stable quick cases: linear/minimum
regeneration at 511/1023/2047 taps and linear apply at those three tap counts.
Each case retains seven raw quick trials plus min/median/nearest-rank-p95/max.
Regeneration compares ns/regeneration; apply compares ns/sample. The case key
identifies the unit-bearing kind, phase, taps, frames/channels, and strategy.

Quick timing windows must be long enough that normal scheduler/frequency noise
does not dominate the 10% same-environment gate. Shared CI without a baseline
enforces finite timing, complete trials, expected IR length, finite output, and
overlap-save routing; absolute nanoseconds remain report-only.

## 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Spectral flux at 44.1/48 kHz | Convert with ~86.1328/~93.75 Hz, never 50 Hz |
| Observation rate is zero, negative, NaN, or infinite | `(None, None, None)` |
| Input is short or has negligible positive-flux energy | No fabricated BPM |
| Analysis version 2 has no key estimator | `unsupported` plus four null payload fields |
| One tap, flat 0 dB | Exact finite unit impulse |
| One tap, uniform +/-6 dB | Scalar error `<= 1e-12` against `10^(g/20)` |
| Multi-tap uniform +/-6 dB | Response error `<= 1e-9 dB` at representative probes |
| Minimum-phase taper endpoint | Midpoint 1, final tap 0, monotonically non-increasing |
| FIR apply uses current 511/1023/2047 taps | `ConvolutionStrategy::OverlapSave` |
| Compatible baseline median exceeds default 10% | `--enforce` fails and names the case |
| No baseline on shared CI | Work/schema gates pass; timing stays report-only |

## 5. Good / Base / Bad Cases

* Good: a 120 BPM fixture analyzed at 44.1 kHz uses `44_100 / 512` and lands
  within the declared 2% integer-lag tolerance.
* Base: a flat/short track returns no tempo, and schema v2 explicitly reports
  key analysis as unsupported.
* Good: a uniform +6 dB curve measures +6 dB in both linear- and minimum-phase
  modes; minimum-phase energy is materially earlier than linear-phase energy.
* Bad: passing the envelope's 50 Hz rate for spectral flux, because it turns a
  correct 120 BPM period into roughly 70 BPM at 44.1 kHz.
* Bad: normalizing every IR by inverse 1 kHz gain or accepting a minimum-phase
  window that rises toward the final sample.
* Bad: claiming musical-key accuracy from a synthetic triad unit test or
  enforcing absolute shared-runner timing without a compatible baseline.

## 6. Tests Required

* Tempo fixtures cover 60/120/180 BPM at 50 Hz, `44_100/512`, and `48_000/512`
  with at most 2% relative error, plus invalid-rate, short, and flat inputs.
* `finalize_analysis` has a regression that would produce ~70 BPM if it reused
  50 Hz for a 44.1 kHz spectral fixture.
* Serialization asserts schema version 2, `key_status = "unsupported"`, and
  null root/mode/confidence/Camelot payloads.
* FIR tests cover one-tap flat/non-uniform/uniform gain in both phase modes,
  independent DFT response probes, taper direction/endpoints, and energy
  centroid ordering.
* `audio_fir_eq_perf --quick --enforce --out ...` must deserialize with nine
  unique cases and seven raw trials per distribution. A compatible-baseline
  run must exercise the default 10% comparison.
* Run both feature test/Clippy matrices, rustfmt, rustdoc, and package
  verification after changing the public analysis schema or FIR design.

## 7. Wrong vs Correct

### Wrong

```rust
// Spectral frames do not arrive at the 50 Hz envelope rate.
let tempo = detect_bpm(&head.spectral_flux, ENVELOPE_RATE);

// Erases a uniform requested gain.
let normalization = 10.0_f64.powf(-gain_at_1khz / 20.0);
ir.iter_mut().for_each(|sample| *sample *= normalization);

// Starts near zero and rises toward one at the last tap.
let weight = 0.5 * (1.0 + ((last - index) as f64 / half as f64 * PI).cos());
```

### Correct

```rust
let spectral_rate = sample_rate as f64 / SPECTRAL_HOP_SIZE as f64;
let tempo = detect_bpm(&head.spectral_flux, spectral_rate);

// Preserve the absolute designed magnitude; no inverse-reference scaling.
self.cached_ir = designed_ir;

let progress = (index - midpoint) as f64 / (last - midpoint) as f64;
let weight = 0.5 * (1.0 + (PI * progress).cos());
```
