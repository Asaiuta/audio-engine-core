# Listening DSP Audit

## Scope

This audit covers the listening-feature DSP named in the PRD:

- `src/processor/eq.rs`
- `src/processor/fir_eq.rs`
- `src/processor/dynamic_loudness.rs`
- `src/processor/crossfeed.rs`
- `src/processor/adapters.rs`
- `src/processor/lockfree_params.rs`
- `benches/audio_quality_measurements.rs`

The shared feature-upgrade audit classifies IIR/FIR EQ, crossfeed, saturation,
and FFT convolution as classic useful DSP, not automatically industry-leading.
This task should therefore close source-backed gaps and add objective evidence,
not strengthen claims broadly.

## Current Behavior

### IIR EQ

- `Equalizer` is a fixed 10-band peaking-EQ bank using RBJ-style biquad sections
  at 31 Hz through 16 kHz with `Q = 1.41`.
- Parameter changes build a target filter bank and crossfade current/target
  outputs for `EQ_SMOOTH_SAMPLES = 1024` frames, then copy only coefficients
  into the current bank so filter state is preserved.
- The settled stereo path has a specialized fast loop, while mono/multichannel
  use the generic per-frame path.
- Existing tests cover per-channel allocation, reset, and fast-path parity, but
  there is no quality-bench metric for EQ target-response accuracy.

### FIR EQ

- `FirEq` generates an impulse response from the same 10 standard bands.
- Linear phase mode builds a magnitude response, IFFTs it, extracts a centered
  odd-length IR, applies a Hann window, and normalizes around 1 kHz.
- Minimum phase mode uses a cepstral method with explicit IFFT normalization,
  then extracts the leading taps and applies an end fade.
- Existing tests verify flat output, bass boost shape sanity, interpolation,
  and the minimum-phase normalization regression, but no benchmark currently
  reports FIR response error.

### Dynamic Loudness

- `DynamicLoudness` is a 7-band ISO-226-inspired compensation EQ:
  low shelf at 40 Hz, peaking bands from 100 Hz to 8 kHz, and high shelf at
  12 kHz.
- Volume maps to a loudness factor from reference `-15 dB` to full
  compensation at `-40 dB`; user strength scales the band targets.
- Band gains are smoothed over roughly 50 ms. Coefficients are advanced per
  `BLOCK_SIZE = 64` frame chunk and only rebuilt when the gain changes by at
  least `GAIN_UPDATE_EPSILON_DB`.
- The hot path uses preallocated per-channel filter banks and smoothers.
- Existing tests cover coefficient caching, sample-rate rebuilds, smoothing,
  identity paths, reset, and denormal flushing. The quality benchmark does not
  currently show the intended low-volume spectral compensation.

### Crossfeed

- `Crossfeed` implements a Bauer-style stereo crossfeed:
  `L_out = L + mix * HPF(R)` and `R_out = R + mix * HPF(L)`.
- The crossfeed path is a second-order Butterworth high-pass around the cutoff
  frequency. Mono and multichannel input are passthrough.
- Direct use of `Crossfeed::set_mix` changes mix without resetting filter
  history. Direct `set_sample_rate` recalculates coefficients and resets state.
- `CrossfeedProcessor::sync_params`, however, calls `set_sample_rate(...)` on
  every lock-free parameter generation change. This means mix-only or
  enabled-only updates unnecessarily clear the HPF history, which can create a
  discontinuity in a realtime parameter change even though cutoff did not
  change.
- Existing tests cover stereo effect, mono passthrough, disabled passthrough,
  coefficient high-pass shape, low-frequency attenuation, and denormal
  flushing. They do not cover adapter-level mix-change continuity or
  allocation-free parameter changes.

## Source-Backed Gaps To Close In This Task

1. Avoid unnecessary crossfeed filter-state resets on mix/enabled parameter
   changes. Only cutoff or sample-rate changes should recalculate HPF
   coefficients and reset history.
2. Add adapter-level tests for crossfeed parameter-change continuity and
   no steady-state allocation through `process`.
3. Add quality-benchmark evidence for listening DSP:
   - IIR EQ target gain accuracy at selected bands.
   - Crossfeed crosstalk effect on hard-panned stereo plus low-frequency
     attenuation from the HPF path.
   - Dynamic-loudness spectral compensation at low volume versus reference
     volume.

## Explicit Non-Goals

- Do not redesign all EQ filters or replace the current crossfeed model without
  separate psychoacoustic reference material and new acceptance thresholds.
- Do not add public tunables unless the existing lock-free snapshot model is
  extended deliberately.
- Do not make README or docs claims stronger than the benchmark evidence.
