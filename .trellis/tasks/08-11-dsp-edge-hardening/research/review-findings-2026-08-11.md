# Review Findings 2026-08-11 — DSP Edge Cases

Source: DSP-filter deep-review agent report from the 2026-08-11 six-track
full-code review. A-1 (EQ Nyquist divergence) and A-2 (transistor fold-back)
were fixed in 1.0.1 and are omitted; A-3 (Equalizer::reset doc) was fixed as a
doc correction. Line numbers are pre-1.0.1.

## Confirmed defects (low)

### A-4 — `FirEq::new` does not validate its sample rate
`fir_eq.rs:72-91` vs `fir_eq.rs:97-103`. `FirEq::new(f64::NAN, n)` neither
errors nor produces NaN: `interpolate_gain(NaN)` falls into the final branch
and returns the last band's gain, silently designing a "flat IR at the 16 kHz
band's gain" — finite but wrong. The setter's guard rationale ("no valid
filter to fall back to") applies equally to construction. Signature constraint:
returns `Self`, fallible constructor is semver-major.

### A-5 — `Saturation::set_highpass_mode` leaves cross-mode state
`saturation.rs:467-473`. Switch resets oversampling state and source history
but not `delay_states[].delta` (Direct quality: the last 4 frames of old-mode
nonlinear residual leak into the first post-switch frames) nor
`hpf_states/prev_inputs` (re-enabling highpass uses a stale `x[n-1]` on the
first frame: one ~`α·Δx` impulse). Magnitude small (residual usually far below
signal); inconsistent with `set_quality`'s
`prepare_nonlinear_state_from_history` warm-up rigor.

### A-6 — `NoiseShaper::set_bits` hardcodes `(8..=32)`
`dsp.rs:243` vs `dsp.rs:197` and `lockfree_params.rs:102-104`. Values
currently agree, so no live fault; but `new_validated` **clamps** against the
published constants while `set_bits` **rejects** against literals — two
policies, two encodings; adjusting the constant forks them. Other modules
share constants deliberately; this is the leak.

## Design concerns

### B-1 — Oversampled alias floor bounded by linear-interp upsampling
`saturation.rs:966-988` (`advance_oversampled_state_fixed` upsamples by
adjacent-sample linear interpolation). Linear interpolation's image rejection
is a sinc² roll-off: a 15 kHz@44.1k first image gets only ~15 dB before the
nonlinearity intermodulates signal×image back in-band. The decimation side
measures -40…-80 dB (verified via DFT), so the real anti-aliasing benefit of
"Oversampled4x" is capped by the ~15-30 dB upsampling side. Adequate for the
default mix=0.2 warmth use; consistent with its own spec and tests. If drive
limits rise or the mode is promoted, switch to a polyphase FIR upsampler.

### B-2 — eq.rs lacks the denormal software fallback on exotic targets
`saturation.rs:872-882` and `crossfeed.rs:329-339` carry
`#[cfg(not(x86/x86_64/aarch64))] flush_subnormal_sample`; eq.rs's 20 biquads
(infinite tails — the exact case `runtime.rs:11` names) have none. On
wasm32/riscv, silent tails hit subnormal slow paths.

### B-3 — EQ smoothing fixed at 1024 samples, not rate-scaled
`eq.rs:90`. 23 ms @ 44.1k, 5.3 ms @ 192k. Crossfeed's `10 ms × fs`
(`crossfeed.rs:257-259`) is the correct precedent.

### B-4 — Spectrum magnitude/bin mapping systematically offset
`spectrum.rs:90-91`: `norm()/fft_size` without single-sided ×2 or Hann
coherent-gain (0.5) compensation ⇒ full-scale sine displays -12 dB (~0.867 on
the UI scale). `spectrum.rs:108,116-119`: `freq_per_bin = nyquist/(N/2-1)`
approximates `sr/N`, and `magnitudes[0]` is actually FFT bin 1 labeled 0 Hz —
about one bin of mapping error. Harmless for visualization; a trap for
measurement reuse.

### B-5 — FirEq extends the 31 Hz band gain to DC
`fir_eq.rs:294-297`. +12 dB low boost ⇒ +12 dB DC gain (linear-phase FIR),
amplifying DC offset; the IIR `Equalizer`'s peaking bands return to 0 dB at
DC, so the two EQs disagree at the bottom of the spectrum.

### B-6 — `Saturation::process()` two-channel default is an API footgun
`saturation.rs:637-640`. A mono buffer through `process()` is treated as
stereo: odd/even samples take separate filter state — subtly wrong output,
no error. Needs a loud doc warning or deprecation toward
`process_with_channels`.

## Style notes routed to `08-11-style-docs-cleanup`

`dsp.rs` dead `TAPS==9` branch + three forwarding aliases; `saturation.rs`
`use` block after 1100 lines of implementation; `OVERSAMPLING_2X_FILTER`
ulp-level coefficient asymmetry; `set_band_gain` re-taking `sample_rate` per
call; `STANDARD_BANDS` tuple readability.
