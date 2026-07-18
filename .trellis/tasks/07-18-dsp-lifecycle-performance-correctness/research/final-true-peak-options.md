# Final Output True-Peak Architecture

## Confirmed local behavior

The executable offline order is currently:

```text
source DSP -> source-rate PeakLimiter -> SoXR -> output-rate NoiseShaper
           -> f32 terminal quantization -> post-render true-peak meter
```

The limiter uses a 10 ms lookahead plus the four-times true-peak detector FIR
delay. It is allocation-free after construction. The quick full-chain probe has
measured a worst final peak of `-0.610 dBTP` for a `-1.0 dBTP` limiter target,
so the source-rate limiter is not an end-to-end ceiling.

SoXR operates in floating point and does not require pre-limited `[-1, 1]`
input. Noise shaping performs the target word-length quantization and must stay
after all gain-changing DSP. The final f32 cast is terminal.

## Comparable output-chain patterns

* Mastering/export chains put sample-rate conversion before the final ceiling
  processor and put dither/word-length reduction last. A limiter before SRC
  cannot constrain reconstruction peaks created by SRC.
* Realtime playback chains limit at the actual device/output rate near the end
  of the floating-point graph. This makes the detector observe the same rate
  domain that reaches the converter.
* Dual-stage broadcast/mastering chains sometimes retain an upstream dynamics
  limiter and add a final guard limiter. This protects both domains but pays two
  lookaheads and can apply two release envelopes.
* A fixed source-ceiling margin is used as a compatibility workaround, but it
  spends loudness without proving a bound for every ratio, phase, and input and
  is not true-peak conformance.

## Option A: Keep the source limiter and report-only final peak

* No additional CPU, state, latency, or topology work.
* Preserves the current sound and callback/offline relationship.
* Keeps a known `0.390 dB` observed violation and cannot claim a final ceiling.
* Appropriate only if final conformance remains explicitly out of scope.

## Option B: Add a second output-rate guard limiter

* Retain the existing source-rate limiter, then run a second true-peak limiter
  after SoXR and before NoiseShaper.
* Provides a final-domain ceiling while preserving upstream limiting behavior.
* Approximately doubles limiter detector/envelope work at equal rates and adds
  another 10 ms lookahead plus detector delay. Persistent rings scale with
  `output_rate * lookahead_seconds * channels`.
* Two release envelopes can increase pumping or transient attenuation. This is
  justified only if the source-rate limiter has a separate dynamics purpose.

## Option C: Relocate the single limiter to the final float domain (recommended)

* Callback processing still runs one limiter at its device rate. Offline render
  runs the same logical limiter after optional SoXR and before NoiseShaper.
* It observes all floating-point resampling peaks without duplicating limiting,
  lookahead, or release behavior.
* Equal-rate CPU and memory remain approximately unchanged. Resampled offline
  work and ring size scale with output rather than source rate; for upsampling
  this costs more than the current limiter but much less than two limiters.
* Source-to-output parity must be defined by rate domain: equal-rate callback
  and offline order remains identical, while unequal-rate render intentionally
  moves limiting after the rate boundary.
* Unknown-tail trimming and finish propagation must include the relocated
  limiter's output-rate latency and complete lookahead drain.

## Quantization guard

A limiter immediately before NoiseShaper still needs a tiny deterministic guard
because dither, error-feedback quantization, and the terminal f32 cast occur
after it. Do not use an arbitrary audio-sized margin. Derive the guard from:

* selected bit depth and the NoiseShaper's bounded feedback/error state;
* the signed quantizer bounds and TPDF range;
* terminal f32 rounding;
* the true-peak reconstruction FIR's absolute coefficient sum.

Validate the analytical bound against adversarial phase/frequency sweeps and
the EBU true-peak corpus. The user-facing target remains `-1.0 dBTP`; the small
internal guard is implementation headroom, not a changed target.

## Recommendation

Choose Option C. It is the only design here that closes the final-domain defect
with one limiter, one lookahead, and one gain envelope. Keep Option A only for an
explicitly scoped deferral; use Option B only if a distinct upstream dynamics
contract is later demonstrated.
