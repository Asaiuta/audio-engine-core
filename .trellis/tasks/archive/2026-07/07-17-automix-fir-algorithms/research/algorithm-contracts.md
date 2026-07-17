# AutoMix and FIR EQ Algorithm Contracts

## Current implementation findings

### AutoMix tempo cadence

* `SpectralFluxAccumulator` analyzes 1,024-sample Hann windows with a 512-sample
  hop. Its real observation rate is therefore `sample_rate / 512`: about
  86.1328 Hz at 44.1 kHz and 93.75 Hz at 48 kHz.
* `finalize_analysis` currently passes the unrelated 50 Hz envelope rate into
  `detect_bpm`, so a lag that is correct in spectral-flux samples is converted
  to the wrong BPM.
* The current fixed lag range 15..=55 encodes roughly 55..200 BPM only when the
  observation rate is 50 Hz. Once the real spectral cadence is supplied, the
  lag bounds must also be derived from the supported BPM range.
* The correct conversion is `bpm = 60 * observation_rate / lag`. Synthetic
  impulse-train fixtures can independently verify this at the 50 Hz envelope
  fallback and both common spectral-flux cadences.

### AutoMix key contract

* `AutomixAnalysis` exposes root, mode, confidence, and Camelot fields, but the
  only construction path always writes `None`; no key estimator or validation
  corpus exists in the repository.
* A lightweight FFT-to-chroma plus Krumhansl-Schmuckler profile correlation is
  feasible, but synthetic triads alone would not validate track-level key
  accuracy. A credible implementation also needs tuning handling, harmonic
  weighting/segmentation, confidence calibration, and an independently labeled
  corpus such as a MIREX/GiantSteps-style key dataset.
* Shipping such a synthetic-only estimator would turn an honest missing
  capability into an algorithm-quality claim that the repository cannot yet
  support. For this task, the versioned DTO should explicitly report key
  analysis as `unsupported`; nullable result fields remain empty and reserved
  for a future corpus-backed detector.

### FIR degeneracy, gain, and phase

* A one-tap FIR can only express a scalar `h[0]`. The existing linear-phase
  Hann expression divides by `num_taps - 1`, producing NaN for one tap.
* Define one-tap behavior as a pure scalar at the existing 1 kHz reference:
  0 dB produces the unit impulse `[1]`, and a uniform `g` dB curve produces
  `[10^(g/20)]`. A non-uniform curve necessarily collapses to its 1 kHz value.
* Both phase generators currently multiply the finished IR by the inverse
  1 kHz target gain. This makes every uniform boost/cut unity, contradicting
  the requested absolute band gains. Removing that normalization preserves
  the designed absolute magnitude; flat 0 dB remains unity without it.
* The minimum-phase tail taper is reversed: its weight is near zero just after
  the midpoint and rises to one at the final tap. A correct raised-cosine tail
  starts at one at the midpoint and monotonically reaches zero at the last tap.

## Selected validation oracles

* Tempo: deterministic impulse trains at multiple observation rates and BPMs;
  expected value comes directly from the fixture period, with a tolerance that
  accounts for integer-lag sampling.
* Key: serialization/API tests require an explicit `unsupported` status and
  absent key result fields; no accuracy claim is made.
* FIR one-tap and uniform gain: exact scalar/analytic dB-gain checks for both
  phase modes and positive/negative gain.
* FIR frequency response: independently compute `H(f) = sum h[n]e^(-jwn)` at
  representative ISO bands and compare measured dB with the requested curve.
* FIR time distribution: compare energy centroid of minimum- and linear-phase
  IRs; the minimum-phase centroid must be materially earlier. Test the taper
  endpoints and monotonic direction separately so the old reversed expression
  necessarily fails.
* Performance: migrate `audio_fir_eq_perf` onto the existing versioned JSON,
  raw-trial median/p95, compatible-baseline, and default 10% regression
  convention. Compare only compatible same-environment reports.

## Scope decision

This task fixes the known tempo and FIR defects and makes the key limitation
machine-readable. Implementing a real key estimator, importing/distributing a
labeled music corpus, changing the convolution engine, or redesigning the
10-band interpolation model are separate evidence-heavy tasks.
