# Reject invalid EQ band index at public boundaries

## Goal

Audit finding 9: `PlaybackParameters::set_eq_band_gain_db` returns `Ok(())` for
a band index outside `0..EQ_BANDS` while the layer below silently discards the
write. A `Result`-returning control API must not acknowledge an edit that never
reached DSP state. Make the EQ band-index contract truthful at every public
entry point that can receive an index, and stop the raw equalizer from turning
a rejected gain into permanently poisoned filter history.

## Revalidation verdict (2026-07-29)

Finding 9 is **accurate and not stale** in the current tree.

- `src/pipeline.rs:698-704` — validates only `gain_db` finiteness through
  `checked_parameter`, forwards `band` unchecked, and always returns `Ok(())`.
- `src/processor/lockfree_params.rs:500-503` — `AtomicEqParams::set_band_gain`
  returns without publishing when `band >= EQ_BANDS`, with no rustdoc stating
  that policy.
- `src/pipeline.rs:2228`, `:2292` — existing negative tests only use band `0`;
  there is no invalid-index test anywhere.

Two related defects were confirmed on the same control path during
revalidation, one layer below the cited evidence:

- `src/processor/eq.rs:111-114` — the raw public `Equalizer::set_band_gain`
  applies the identical silent `return` for `band_idx >= EQ_BANDS`.
- `src/processor/eq.rs:115` — it then applies `gain_db.clamp(-15.0, 15.0)`.
  `f64::clamp` passes `NaN` through, so `BiquadSection::peaking_eq` builds
  `NaN` coefficients and the biquad history is poisoned permanently. This is
  the exact failure mode `ProcessError::InvalidParameter` documents. The
  facade path is protected because `AtomicEqParams` sanitizes first; direct
  users of the exported `Equalizer` — including the crate's headline doctest at
  `src/lib.rs:12-23` — are not.
- The same line re-encodes `EQ_BAND_GAIN_DB_MIN`/`EQ_BAND_GAIN_DB_MAX` as
  `-15.0`/`15.0` literals (audit finding 17's duplicated-range pattern).

## What I already know

- `ProcessError` is `#[non_exhaustive]` and already owns
  `InvalidParameter { processor, parameter, message }`, documented as "a
  control-thread parameter write was rejected before it could reach DSP state,
  for example a non-finite value that would poison filter history". No new
  error type is needed.
- The `Atomic*Params` family is infallible **by design**: the prior facade task
  recorded an ADR that `lockfree_params` sanitizes centrally (a rejected write
  keeps the previous snapshot) and the facade layers typed `ProcessError`
  rejection on top. About fifteen setters share that policy.
- `EqProcessor::sync_params` calls `Equalizer::set_all_bands` from inside
  `process()` — i.e. on the audio thread (`src/processor/adapters.rs:215-227`).
  Its gains come from `AtomicEqParams` and are therefore already finite and
  clamped. Making `set_all_bands` fallible must not push a `Result` onto the
  callback path.
- Audit finding 6 (`07-29-harden-raw-dsp-geometry-boundaries`) established the
  house pattern for exactly this shape: checked public shells returning
  `ProcessError`, with crate-private `*_validated` kernels that adapters call
  after their own validation. Its Out of Scope explicitly deferred
  "non-finite or out-of-range control values from later audit findings" —
  which is this task.
- Call sites of the raw `Equalizer` mutators outside `eq.rs`:
  `src/lib.rs:18` (doctest), `src/processor/adapters.rs:199`, `:224`, `:288`,
  `benches/audio_quality_measurements.rs:1648`,
  `examples/equalizer_curve.rs:25`.
- `benches/audio_lockfree_params_perf.rs:202` uses
  `AtomicEqParams::set_band_gain` with `i % EQ_BANDS`, so it is unaffected as
  long as that method stays infallible.
- The crate is pre-1.0 and the CHANGELOG already documents that minor bumps may
  break the public API.

## Requirements

- `PlaybackParameters::set_eq_band_gain_db` rejects a band index outside
  `0..EQ_BANDS` with `ProcessError::InvalidParameter` and publishes nothing.
- The raw public `Equalizer::set_band_gain` and `set_all_bands` return
  `Result<(), ProcessError>` and reject an out-of-range band index and a
  non-finite gain before touching coefficients, target gains, or smoothing
  counters.
- `set_all_bands` is failure-atomic: it validates every gain before applying
  any, so a single bad entry cannot leave a partially updated bank.
- One crate-private validator owns the band-index rejection policy so both
  fallible boundaries report the same `parameter` name and message.
- The clamp range in `eq.rs` uses `EQ_BAND_GAIN_DB_MIN`/`EQ_BAND_GAIN_DB_MAX`
  instead of `-15.0`/`15.0` literals. Values and behaviour are unchanged.
- Valid-input coefficients, target gains, crossfade timing, `Equalizer::process`
  output, and the `EqProcessor` stage's audio output stay bit-identical.
- The realtime `EqProcessor` path calls crate-private `*_validated` kernels, so
  the callback gains no `Result`, no validation cost, and no allocation.
- `AtomicEqParams::set_band_gain` keeps its infallible family policy but
  documents that an out-of-range band or non-finite gain publishes nothing and
  keeps the previous snapshot.
- `src/lib.rs` doctest, `examples/equalizer_curve.rs`, and
  `benches/audio_quality_measurements.rs` compile against the fallible API.

## Acceptance Criteria

- [x] `set_eq_band_gain_db(EQ_BANDS, g)` and any larger index return
      `ProcessError::InvalidParameter` and leave `eq_band_gains_db()` unchanged.
- [x] `Equalizer::set_band_gain` returns a typed error for an out-of-range band
      and for a non-finite gain, and its coefficients/target gains/smoothing
      state are unchanged after each rejection.
- [x] `Equalizer::set_all_bands` rejects a bank containing any non-finite gain
      without applying the finite entries that precede it.
- [x] A rejected raw gain can no longer produce `NaN` output from
      `Equalizer::process`.
- [x] Both fallible boundaries report the same `parameter` identity for an
      invalid band index.
- [x] `AtomicEqParams::set_band_gain` still no-ops for an invalid band and its
      documented policy is asserted by a test.
- [x] Existing EQ output/smoothing/parity tests pass unchanged.
- [x] `cargo fmt --all -- --check`, strict Clippy, and the full test suite pass
      on both `--all-features` and `--no-default-features --features rubato`.
- [x] CHANGELOG records the breaking raw-`Equalizer` signature change; the
      final review records adopted and rejected broader refactors.

## Definition of Done

- No public EQ entry point accepts an out-of-range band index and reports or
  implies success.
- A non-finite gain cannot reach `BiquadSection::peaking_eq` through any public
  path.
- One crate-private validator owns band-index policy; one kernel owns the
  clamp.
- Checked public shells and crate-private kernels have explicit names and
  documented preconditions.
- Existing unrelated dirty work remains untouched; no commit, push, or archive
  without explicit direction.

## Technical Approach

1. Add `pub(crate) fn validate_eq_band_index(processor, band)` next to
   `EQ_BANDS` in `src/processor/lockfree_params.rs`, returning
   `ProcessError::InvalidParameter { parameter: "eq band index", .. }`.
2. `src/pipeline.rs`: call it from `set_eq_band_gain_db` before the gain check,
   and document the rejection in the method's rustdoc and the
   `PlaybackParameters` type-level value contract.
3. `src/processor/eq.rs`: split each mutator into a checked public shell and a
   `pub(crate) *_validated` kernel. The kernel owns the clamp using the
   published constants; the shells own index/finiteness rejection.
4. `src/processor/adapters.rs`: `EqProcessor::new`, `sync_params`, and
   `set_sample_rate` call `set_all_bands_validated`, since their gains come
   pre-sanitized from `AtomicEqParams`.
5. Update the `AtomicEqParams::set_band_gain` rustdoc; update the crate doctest,
   example, benchmark, tests, CHANGELOG, and the EQ spec contract.

## Decision (ADR-lite)

**Context**: The same out-of-range band index is silently discarded at three
public layers, while only the top layer promises a `Result`. The layer in the
middle (`Atomic*Params`) has a deliberate, uniform, documented infallible
sanitize policy shared by roughly fifteen setters.

**Decision**: Make the two boundaries that can meaningfully report an outcome
fallible — the facade and the raw `Equalizer` — using the existing
`ProcessError::InvalidParameter` variant and the checked-shell / private-kernel
pattern already established for raw DSP geometry. Leave the `Atomic*Params`
family's infallible policy intact and document it instead.

**Consequences**: `Equalizer::set_band_gain` and `set_all_bands` become
breaking source changes in a pre-1.0 crate, affecting one doctest, one example,
and one benchmark. Direct raw-EQ users must handle rejection instead of
receiving silent no-ops or NaN-poisoned filters. The atomic parameter family
stays uniform, so a future P2 §3 task can still convert it as one coherent
change rather than inheriting a half-converted type.

## Out of Scope

- Making the `Atomic*Params` family fallible for non-finite values
  (audit P2 §3 — a separate, much larger boundary task).
- `Equalizer::new` channel/sample-rate geometry validation, and any
  `set_band_gain` sample-rate validation: the same class as audit finding 6,
  but `eq.rs` was outside that task's scope. Recorded as a known remaining gap.
- Replacing `band: usize` with a typed `EqBand`, or changing band frequencies,
  Q, smoothing length, or coefficient design.
- Audit finding 10 (paired saturation publication) and every later finding.
- Committing, pushing, or archiving anything without explicit direction.

## Technical Notes

- Primary code: `src/pipeline.rs`, `src/processor/lockfree_params.rs`,
  `src/processor/eq.rs`, `src/processor/adapters.rs`.
- Call-site fallout: `src/lib.rs`, `examples/equalizer_curve.rs`,
  `benches/audio_quality_measurements.rs`.
- Contracts: `.trellis/spec/backend/error-handling.md`,
  `.trellis/spec/backend/realtime-safety.md`,
  `.trellis/spec/backend/dsp-state-correctness.md`.
- Source finding:
  `.trellis/tasks/07-28-codebase-maintainability-audit/research/01-public-api-and-control-boundaries.md`.
