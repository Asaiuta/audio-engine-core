# Migrate remaining real-valued FFT call sites to realfft

## Goal

Finish the migration started for `OverlapSaveConvolver` (commit `80d6b07`). Audio
is real-valued, so a complex FFT over it carries an identically-zero imaginary
half. `realfft` is already a dependency and already used by
`PartitionedConvolver`, `OverlapSaveConvolver`, and `SpectralNonlinearResampler`.

Three call sites still feed real data through `rustfft` complex transforms and
then read only half the result. No new dependency, no new algorithm, no public
signature changes.

## Survey: every remaining `rustfft` call site

| Site | Real-valued? | Verdict |
| --- | --- | --- |
| `spectrum.rs:79` `SpectrumAnalyzer::analyze` | yes — `Complex::new(s*w, 0.0)`, reads `fft_buffer[1..n/2]` | **migrate** |
| `automix_analysis.rs:394` `SpectralFluxAccumulator::process` | yes — `Complex32::new(s*w, 0.0)`, reads `frame[0..n/2]` | **migrate** |
| `fir_eq.rs:236` `generate_linear_phase_ir` | yes — builds explicit Hermitian spectrum, IFFTs, takes `.re` | **migrate** |
| `fir_design.rs:47/68/74` `minimum_phase_from_log_magnitude` | **no** | **exclude** |
| `polyphase_backend.rs:280/346` | not surveyed for this task | **out of scope** |

`fir_design` is excluded on inspection, not omission: the cepstral
factorization applies `value.exp()` to a *complex* spectrum between the
transforms, so the intermediate genuinely has a non-zero imaginary part. Its
real-input/real-output endpoints could in principle use `realfft`, but the middle
cannot, and the mixed plan would be less clear than what is there now.
`fir_eq::generate_minimum_phase_ir` delegates to it and so is also untouched.

## Measured evidence (this machine, release, paired in-process A/B)

Probes asserted numeric agreement alongside timing, so these are not timing-only
claims.

### `spectrum.rs` pattern — f64 forward, read bins `1..n/2`, preallocated scratch

| N | complex | real | speedup |
| ---: | ---: | ---: | ---: |
| 512 | 2502 ns | 1706 ns | **1.47x** |
| 1024 | 4237 ns | 3314 ns | **1.28x** |
| 2048 | 9490 ns | 7532 ns | **1.26x** |
| 4096 | 20453 ns | 15336 ns | **1.33x** |
| 8192 | 41389 ns | 34683 ns | **1.19x** |

The probe asserted the summed magnitudes agree to a relative `1e-9`.

### `automix` pattern — f32 forward, complex side using allocating `process()`

| N | complex | real | speedup |
| ---: | ---: | ---: | ---: |
| 1024 | 2246 ns | 1859 ns | **1.21x** |

### `fir_eq` linear-phase IR generation — Hermitian build + IFFT

| taps | complex | real | speedup | rel. max diff |
| ---: | ---: | ---: | ---: | ---: |
| 255 | 1814 ns | 1260 ns | **1.44x** | 2.8e-17 |
| 511 | 4164 ns | 2548 ns | **1.63x** | 6.9e-17 |
| 1023 | 21618 ns | 5063 ns | **4.27x** | 6.7e-17 |
| 2047 | 21118 ns | 11041 ns | **1.91x** | 5.0e-17 |

Agreement is at float-rounding level (~1e-17 relative), **not bit-exact**. This
matters for the test strategy below.

### Secondary finding: `automix_analysis` allocates per FFT

`SpectralFluxAccumulator::process` calls `self.fft.process(&mut self.frame)`.
`rustfft`'s plain `process` allocates its scratch on every call. This is offline
analysis, not the audio callback, so it is not a realtime-safety violation — but
it is a per-hop allocation in a loop that runs once per 512-sample hop across a
whole track. The `realfft` migration replaces it with
`process_with_scratch` over a preallocated buffer, removing the allocation as a
side effect. `fir_eq.rs:236` and `fir_design` likewise use allocating `process`,
but they are setup-time and already documented as such.

## Decision (ADR-lite)

**Context.** Same class of change as `80d6b07`, three independent sites, each
with existing test coverage to gate against.

**Decision.** Migrate the three real-valued sites to `realfft`. Keep
`fir_design` on `rustfft` and add a comment recording why, so this is not
re-litigated. Storage shrinks from `n` complex bins to `n/2 + 1` at each site.

**Correctness gates, per site:**

- `spectrum.rs` already has `spectrum_analyzer_matches_legacy_reference`, an
  oracle test against an inline reimplementation of the original complex-FFT
  algorithm (`legacy_analyze`). That oracle is the gate. It must keep passing,
  and it must **stay** on the complex formulation — rewriting the oracle to use
  `realfft` too would make it self-confirming.
- `automix_analysis.rs` — spectral flux feeds tempo/section detection. Gate on
  the existing analysis tests plus a new direct equivalence check of the flux
  sequence against the complex formulation.
- `fir_eq.rs` — gate on the existing FIR EQ correctness tests plus a direct
  comparison of the generated IR against the complex formulation.

**Tolerance policy.** Because agreement is ~1e-17 relative and not bit-exact,
new equivalence tests compare with an explicit relative tolerance and a stated
justification, following the precedent in `df1346a` (convolver tail assertions
tolerating FFT rounding). Tests must not silently use a loose absolute epsilon.

**Consequences / risks.**

- `spectrum.rs`'s `assert_no_alloc` test asserts on private fields
  (`fft_buffer`, `fft_scratch`) that this change renames/retypes. The test must
  be updated to the new field set, and must keep asserting that a rejected call
  mutates nothing.
- `SpectrumAnalyzer::analyze` reads `fft_buffer[1..fft_size/2]`, deliberately
  skipping DC and stopping below Nyquist. The `realfft` output is
  `fft_size/2 + 1` long with DC at `0` and Nyquist at the end, so the slice
  bounds must be re-derived rather than copied.
- `fir_eq` currently builds the negative-frequency half explicitly. With a real
  inverse transform that half must **not** be written; `realfft` also requires
  the imaginary parts of the DC and Nyquist bins to be zero, and mutates its
  input buffer.
- Bench baselines in `docs/quality.md` for the spectrum cases (5.05 ns/sample at
  1,024 points) and FIR EQ regeneration will move.

## Scope

**In scope**

- `src/processor/spectrum.rs` — `SpectrumAnalyzer` to `realfft`
- `src/processor/automix_analysis.rs` — `SpectralFluxAccumulator` to `realfft`,
  with preallocated scratch
- `src/processor/fir_eq.rs` — `generate_linear_phase_ir` to a real inverse
  transform
- `src/processor/fir_design.rs` — comment only, recording why it stays complex
- New equivalence tests for the automix and FIR EQ sites
- Fresh bench runs + `docs/quality.md` updates for numbers that move

**Out of scope**

- `fir_design.rs` transform changes (genuinely complex mid-pipeline)
- `polyphase_backend.rs` (not surveyed; separate task if warranted)
- Replacing `rustfft` as a dependency — `fir_design` and `polyphase_backend`
  still need it, so the dependency count does not change
- Any public API change

## Completion criteria

1. `cargo test --all-features` and `cargo test --no-default-features --features rubato` pass.
2. `spectrum_analyzer_matches_legacy_reference` still passes, with its oracle
   still expressed as a complex FFT.
3. New test: automix spectral flux matches the complex formulation within a
   stated relative tolerance.
4. New test: FIR EQ linear-phase IR matches the complex formulation within a
   stated relative tolerance.
5. `spectrum.rs`'s allocation test updated to the new fields and still proving a
   rejected call mutates nothing.
6. `assert_no_alloc` steady-state tests still pass.
7. `tests/public_api.rs` unchanged (all three types' surfaces are unaffected).
8. `cargo clippy --all-features --all-targets` and `cargo fmt --check` clean.
9. Affected `docs/quality.md` numbers restated from a fresh measured run, with
   the host-noise caveat applied as in `80d6b07`.
