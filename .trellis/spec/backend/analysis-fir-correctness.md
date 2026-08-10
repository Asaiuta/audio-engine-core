# AutoMix Analysis and FIR Design Correctness

> Executable contracts for AutoMix tempo output and FIR-EQ impulse-response
> design. Read this before changing `automix_analysis.rs`, `fir_eq.rs`, or the
> FIR performance evidence path.

## 1. Scope / Trigger

This spec applies when code changes:

* spectral-flux window/hop geometry, tempo lag search, or `AutomixAnalysis`;
* the analysis schema or any future musical-key capability;
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

AnalysisWindowPlan::new(
    mode: AutomixAnalysisMode,
    track_frames: Option<u64>,
    window_frames: u64,
) -> AnalysisWindowPlan;

decode_segment(
    decoder: &mut StreamingDecoder,
    meter: &mut LoudnessMeter,
    segment: &mut AnalysisSegment,
    skip_frames: u64,
    take_frames: u64,
    cancel_token: Option<&DecodeCancelToken>,
) -> Result<(), AutomixError>;

#[non_exhaustive]
pub enum AutomixError {
    Canceled,
    Decoder(DecoderError),
    TailSeekPastStart { planned_frame: u64, realized_frame: u64 },
}

pub struct AutomixAnalysis {
    pub version: u32, // current schema: 3
    // timing/loudness/mix fields; no reserved key fields
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

Analysis schema version 3 has no key-status or key-payload fields because no
key detector runs. Do not reserve an always-empty capability model or add a
detected/low-confidence state from synthetic chords alone. A future schema may
add a coherent key result only with independently labeled music-corpus
evidence, explicit tuning and harmonic/segmentation policy, and calibrated
confidence behavior.

### Bounded analysis interval ownership

AutoMix plans head and tail in integer frames. `AudioInfo::total_frames` is the
authoritative track length when present; a finite positive duration converted
to frames is only the fallback. The head is
`[0, min(track_frames, window_frames))`. Full mode adds a tail whenever the
known track extends beyond the head, with
`tail.start = max(head.end, track_frames - window_frames)` and
`tail.end = track_frames`. The two intervals never overlap. Unknown or invalid
track length remains bounded head-only because an end-relative seek is not
defined.

`StreamingDecoder::seek` is coarse. After seeking, compute preroll from
`current_frame()` and skip it before analysis. A decoder position after the
planned start is an error rather than permission to analyze the wrong interval.
For every packet, apply leading skip and trailing take bounds once to the
interleaved frame range; only that selected slice may reach `LoudnessMeter`,
RMS/low/vocal envelopes, or spectral flux.

`AnalysisSegment.start_time` owns the absolute timeline origin. Silence,
vocal, and energy-profile placement reuse it; they must not reconstruct a tail
origin from the declared duration and a feature-vector length because 20 ms
accumulators intentionally omit incomplete blocks. `frames_analyzed` records
the realized selected frame count and is the duration fallback when container
duration is unavailable.

### Declared duration is untrusted input

`AudioInfo::duration_secs` and `AudioInfo::total_frames` come from container
metadata that no one has verified. They also size `energy_profile`, which is a
whole-track vector at `ENERGY_PROFILE_RATE` slots per second, so an absurd
declared value asks for an allocation proportional to it — and `vec![0.0; n]`
aborts rather than returning an error.

A declared duration is therefore accepted only when finite, positive, and no
greater than `MAX_DECLARED_DURATION_SEC` (24 hours). An implausible value is
**discarded, not clamped**: clamping would report a confident 24-hour timeline
the file never supported, while discarding falls back to the duration actually
measured from decoded head evidence. `build_energy_profile` enforces the same
ceiling itself, because it is the allocation site and must not depend on every
present and future caller having filtered first.

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
| Analysis version 3 has no key estimator | No key status or key payload fields are serialized |
| Known track ends at or before one window | Head covers the available frames; no tail |
| Full track is just over one window or exactly two windows | Tail starts at `head.end`; no overlap or uncovered suffix |
| Full track exceeds two windows | Tail is the final full window; the middle gap remains intentionally unanalyzed |
| Coarse seek lands before the tail start | Skip exact preroll frames before every metric |
| Coarse seek reports a frame after the planned tail start | Return a named analysis error; do not shift the interval silently |
| Decoder packet crosses a skip/take boundary | Slice once, then give every metric the identical selected frames |
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
* Base: a flat/short track returns no tempo, and schema v3 makes no key-analysis
  claim or reservation.
* Good: with a 60-second window, 61/120/121-second tracks plan tails at
  `[60,61)`, `[60,120)`, and `[61,121)` seconds respectively.
* Base: Head mode or unknown track length analyzes only the bounded head.
* Bad: decode a tail only when `duration > 2 * window`, seek the last full
  window for shorter tracks and overlap the head, or feed a complete final
  packet to loudness before truncating the other metrics.
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
* Serialization asserts schema version 3 and the absence of `key_status`,
  root/mode/confidence, and Camelot payloads.
* Pure planner tests cover at/below one window, just above one window, exactly
  two windows, above two windows, Head mode, and unknown length; every planned
  head/tail pair is disjoint.
* Packet-boundary tests place both leading skip and trailing take inside one
  packet and assert loudness frame count plus every feature accumulator count
  describe the selected slice.
* End-to-end PCM WAV fixtures at just above one window, exactly two windows,
  and above two windows contain a known final silent suffix and assert absolute
  fade-out, cut-out, and mix-center positions. A separate segment-origin test
  uses a vector length that cannot reconstruct the declared tail start.
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

if duration > 2.0 * window {
    decoder.seek(duration - window)?; // skips valid shorter tails
}
meter.process(&packet); // other metrics later truncate this packet
let tail_start = duration - tail.envelope.len() as f64 / ENVELOPE_RATE;

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

let plan = AnalysisWindowPlan::new(mode, track_frames, window_frames);
if let Some(tail) = plan.tail {
    let preroll = tail
        .start
        .checked_sub(decoder.current_frame())
        .ok_or(AutomixError::TailSeekPastStart {
            planned_frame: tail.start,
            realized_frame: decoder.current_frame(),
        })?;
    let selected = &packet[frame_start * channels..frame_end * channels];
    analyzer.process(selected, meter, segment); // every metric sees this slice
}

fill_energy_profile(
    &mut profile,
    &tail.envelope,
    tail.start_time,
    ENVELOPE_RATE,
    profile_rate,
);

// Preserve the absolute designed magnitude; no inverse-reference scaling.
self.cached_ir = designed_ir;

let progress = (index - midpoint) as f64 / (last - midpoint) as f64;
let weight = 0.5 * (1.0 + (PI * progress).cos());
```
