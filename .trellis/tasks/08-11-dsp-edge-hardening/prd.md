# DSP Edge-Case Hardening

2026-08-11 full-code-review follow-up, batch 2 of 8. The review's verdict on
the DSP filter stack: reference-faithful implementations with excellent tests,
but "the tests live on the 48 kHz happy path" — both confirmed 1.0.1 defects
(EQ Nyquist divergence, transistor fold-back) sat in unswept parameter space.
This task clears the remaining edge-space findings.

## Goal

Close the confirmed low-severity defects and the parameter-space gaps in the
filter/saturation/spectrum stack so no known input regime produces
inconsistent, surprising, or silently wrong output.

## What I Already Know

- **eq.rs has no denormal fallback on non-x86/aarch64 targets**: `saturation.
  rs:872-882` and `crossfeed.rs:329-339` both carry the
  `#[cfg(not(x86/x86_64/aarch64))]` software `flush_subnormal_sample`
  fallback; the 20 biquads of the EQ — the stage with infinite tails, named by
  `runtime.rs:11` as the motivating case — have none. On wasm32/riscv a
  silent tail sinks into subnormal slow paths. Crate-internal consistency gap.
- **EQ smoothing is a fixed 1024 samples** (`eq.rs`, `EQ_SMOOTH_SAMPLES`):
  23 ms at 44.1 kHz but 5.3 ms at 192 kHz — parameter-change feel varies with
  sample rate. Crossfeed's `10 ms × fs` (`crossfeed.rs:257-259`) is the
  correct in-crate precedent.
- **FirEq extrapolates the 31 Hz band gain flat down to DC**
  (`fir_eq.rs:294-297`): a +12 dB low boost raises DC offset by +12 dB
  (linear-phase FIR has that literal DC gain), and diverges from the IIR
  `Equalizer`, whose peaking bands return to 0 dB at DC. Decide the DC target
  (taper to 0 dB is the obvious candidate) and document it.
- **`FirEq::new` does not validate its sample rate** (`fir_eq.rs:72-91` vs
  the guarded `set_sample_rate` at `:97-103`): `FirEq::new(f64::NAN, n)`
  silently designs a flat IR pinned to the last band's gain
  (`interpolate_gain(NaN)` falls through to the final branch). Constraint:
  `new` returns `Self`, so a fallible signature is a semver-major change —
  choose between an internal sanitize-with-documented-fallback now and a
  fallible constructor queued for 2.0.
- **Saturation `set_highpass_mode` leaves cross-mode state behind**
  (`saturation.rs:467-473`): resets oversampling state and source history but
  not `delay_states[].delta` (Direct-quality: last 4 frames of old-mode
  residual mix into the first post-switch frames) nor `hpf_states/
  prev_inputs` (first frame after re-enable uses a stale `x[n-1]`, one
  ~`α·Δx` impulse). Small magnitude; inconsistent with the rigor of
  `set_quality`'s `prepare_nonlinear_state_from_history` warm-up.
- **`Saturation::process()` defaults to two channels** (`saturation.rs:
  637-640`): a mono buffer through `process()` is interpreted as stereo —
  odd/even samples get separate filter state, producing subtly wrong output
  instead of an error. Legacy-compat; needs at least a loud doc warning,
  ideally deprecation toward `process_with_channels`.
- **`NoiseShaper::set_bits` hardcodes `(8..=32)`** (`dsp.rs:243`) while
  `new_validated` clamps against the published constants
  (`lockfree_params.rs:102-104`) — two policies, two literals, one future
  drift hazard. The crate's own spec forbids re-encoding published bounds.
- **Spectrum analyzer is uncalibrated** (`spectrum.rs:90-91`, `:108`,
  `:116-119`): magnitude lacks the single-sided ×2 and Hann coherent-gain
  (0.5) compensation ⇒ full-scale sine reads -12 dB, UI scale ~0.867;
  `freq_per_bin = nyquist/(N/2−1)` approximates the true `sr/N` and
  `magnitudes[0]` is actually FFT bin 1 ⇒ ~1 bin mapping offset. Harmless for
  visualization, a trap if ever reused as measurement. Minimum fix: document
  "not calibrated"; better: calibrate and keep a visual-compat scale factor.
- **Oversampled saturation's alias floor is bounded by linear-interpolation
  upsampling** (`saturation.rs:966-988`): sinc² roll-off gives a 15 kHz@44.1k
  image only ~15 dB rejection before the nonlinearity intermodulates it back
  in-band; the decimation FIR's measured -40…-80 dB is the strong half of an
  asymmetric pair. Fine for the default low-drive character use; document the
  ceiling now, swap in a polyphase FIR upsampler only if drive limits rise or
  the feature is promoted.

## Research References

- [`research/review-findings-2026-08-11.md`](research/review-findings-2026-08-11.md)
  — findings A-4, A-5, A-6, B-1…B-6 from the DSP-filter review report with
  derivations.

## Requirements

- Add the software denormal flush to eq.rs biquads under the same cfg gate as
  saturation/crossfeed; zero cost on x86/aarch64 (cfg'd out), covered by the
  existing unsupported-target test pattern in `realtime-safety.md`.
- Scale `EQ_SMOOTH_SAMPLES` from a time constant (~21 ms) at the configured
  rate; keep chunk-equivalence and bit-exact adoption tests green at 44.1/48/
  96/192 kHz.
- FirEq: pick and document the DC behavior; add a DC-gain assertion test.
  Sanitize or (2.0) validate construction rate; document whichever holds.
- Saturation: clear `delay_states[].delta` and HPF history on
  `set_highpass_mode`; add a first-frames continuity test for both switch
  directions. Add the mono-misuse warning to `process()` rustdoc (or
  deprecate in favor of `process_with_channels`).
- NoiseShaper: route `set_bits` through the published constants.
- Spectrum: document (or calibrate + document) magnitude and bin-frequency
  semantics; add a full-scale-sine expectation test pinning whichever
  contract is chosen.
- Saturation upsampling: document the alias ceiling in the quality-mode
  rustdoc and `docs/quality.md`; no algorithm change in this task.

## Out of Scope

- New saturation upsampler design (separate performance task if ever needed).
- EQ band layout, Q, or new filter types.
- Public API signature changes (semver-patch only; FirEq fallible `new` goes
  to the 2.0 list).

## Technical Notes

- Files: `src/processor/eq.rs`, `fir_eq.rs`, `fir_design.rs`,
  `saturation.rs`, `dsp.rs`, `spectrum.rs`, `runtime.rs` (cfg helper reuse).
- Specs: `realtime-safety.md` (denormal + hot-path rules),
  `dsp-state-correctness.md` (validated-kernel and published-constant rules),
  `listening-nonlinear-correctness.md` (saturation contracts).
- The 1.0.1 stability sweep test (`every_band_is_strictly_stable_across_
  sample_rates_and_extreme_gains`) is the pattern to extend for any new
  rate-dependent behavior.

## Implementation Plan

1. eq.rs denormal parity + rate-scaled smoothing (+ tests across rates).
2. Saturation mode-switch state clear + mono footgun docs.
3. NoiseShaper constant unification.
4. FirEq DC decision + construction guard decision (ADR-lite in this PRD).
5. Spectrum calibration decision + tests.
6. Documentation batch (upsampler ceiling, spectrum semantics), full matrix
   run.
