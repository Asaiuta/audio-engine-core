# Current Algorithm Audit Summary

## Source-Backed Findings

- Resampling uses SoXR VHQ/polyphase wrappers and is already a strong choice for high-quality sample-rate conversion.
- Loudness measurement follows EBU R128 through the `ebur128` crate, with a local 4x FIR true-peak detector modeled after libebur128-style interpolation.
- Final dither/noise shaping uses SoX-derived coefficients with TPDF dither and stability-oriented safeguards.
- IIR EQ, FIR EQ, crossfeed, saturation, FFT convolution, and spectrum analysis are classic DSP implementations. They are useful and maintainable, but not automatically "industry-leading" in every case.
- The limiter path is the clearest gap: it is a 10 ms lookahead sample-peak limiter and should not be described as a strict true-peak output guarantee.
- Saturation is direct nonlinear waveshaping without oversampling, so high-frequency or strongly driven content can create aliasing products.
- The convolver uses overlap-save FFT convolution and is appropriate for short/medium IRs; long room IRs need partitioning to avoid excessive per-callback work.

## Upgrade Implications

- Add objective gates before strengthening claims.
- Keep existing strong foundations, especially SoXR and EBU R128 measurement.
- Focus feature work on areas where implementation and evidence can change the verdict: true-peak limiting, saturation aliasing, and long-IR convolution.
- Preserve realtime safety and allocation discipline in every DSP hot path.

## Evidence Policy

- A README claim should be backed by current benchmark output, a unit/integration test, or an explicit limitation note.
- Missing external corpora, such as EBU reference files, should produce a skipped/limited result rather than a silent pass.
- If a benchmark is report-only, do not treat it as a conformance gate.
