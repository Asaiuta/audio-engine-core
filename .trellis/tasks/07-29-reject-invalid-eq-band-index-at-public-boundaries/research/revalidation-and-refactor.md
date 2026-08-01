# Finding 9 revalidation and refactor review

## Current evidence (2026-07-29)

- `src/pipeline.rs:698-704` (pre-change) returned `Result<(), ProcessError>`,
  validated only `gain_db` finiteness through `checked_parameter`, forwarded
  `band` unchecked, and always returned `Ok(())`.
- `src/processor/lockfree_params.rs:500-503` (pre-change)
  `AtomicEqParams::set_band_gain` returned without publishing when
  `band >= EQ_BANDS`, with no rustdoc stating that policy.
- `src/pipeline.rs:2228` and `:2292` covered non-finite and out-of-range *gains*
  with band `0` only. No invalid-index test existed anywhere in the repository.
- The finding is therefore accurate and not stale. `src/pipeline.rs` and
  `src/processor/lockfree_params.rs` carried unrelated dirty facade work that
  was preserved; `src/processor/eq.rs` and `examples/equalizer_curve.rs` were
  clean before this task.

Two same-class defects were confirmed one layer below the cited evidence:

- `src/processor/eq.rs:111-114` (pre-change) applied the identical silent
  `return` for `band_idx >= EQ_BANDS` on the exported raw `Equalizer`.
- `src/processor/eq.rs:115` applied `gain_db.clamp(-15.0, 15.0)`. `f64::clamp`
  passes `NaN` through, so `BiquadSection::peaking_eq` built `NaN` coefficients
  and the band's history stayed poisoned for the rest of the stream. Reachable
  from the crate's headline doctest; the facade path was protected only because
  `AtomicEqParams` sanitizes first.
- The same line re-encoded `EQ_BAND_GAIN_DB_MIN`/`_MAX` as literals — audit
  finding 17's duplicated-range pattern on the exact line being repaired.

## Adopted refactors

1. `pub(crate) validate_eq_band_index(processor, band)` in `lockfree_params.rs`
   is the single owner of band-index rejection, so the facade and the raw
   `Equalizer` emit the identical `InvalidParameter { parameter: "eq band
   index" }` for the same mistake. The bound is not re-encoded at call sites.
2. `PlaybackParameters::set_eq_band_gain_db` validates the index before the gain
   and propagates the typed error, so `Ok` now means the edit reached the
   callback. The type-level value contract documents that an out-of-range
   *index* is rejected rather than clamped.
3. The raw `Equalizer` follows finding 6's established shape: checked public
   shells (`set_band_gain`, `set_all_bands`) over crate-private
   `set_band_gain_validated` / `set_all_bands_validated` kernels. The kernel owns
   the clamp; the shells own index and finiteness rejection.
4. `set_all_bands` validates the whole bank before applying any band, so a
   rejected write cannot leave a partially updated equalizer.
5. The clamp uses `EQ_BAND_GAIN_DB_MIN`/`_MAX`. Values are unchanged (-15/+15),
   so this removes a duplicated range owner with no behaviour change.
6. `EqProcessor::new`, `sync_params`, and `set_sample_rate` call
   `set_all_bands_validated`. `sync_params` runs inside `process()` on the audio
   thread and its gains come pre-sanitized from `AtomicEqParams`, so the callback
   gained no `Result`, no validation cost, and no allocation.
7. `AtomicEqParams::set_band_gain` rustdoc now states the previously undocumented
   no-op policy and names the facade as the reporting layer.
8. `dsp-state-correctness.md` gained a "Band-index addressing and control
   rejection" contract, matrix rows, required tests, and wrong/correct examples,
   so a future edit cannot re-derive the silent-discard behaviour.

## Rejected broader refactors

- **Do not make the `Atomic*Params` family fallible.** Its infallible
  sanitize-and-keep-previous policy is a deliberate ADR from
  `07-28-playback-facade-robustness-and-lifecycle-command-channel` shared by
  roughly fifteen setters. Converting only `AtomicEqParams` would fragment a
  uniform family contract; converting all of it is audit P2 §3 and belongs in
  one coherent task.
- **Do not replace `band: usize` with a typed `EqBand` enum.** It would remove
  the invalid state space, but the gains are already exposed as
  `[f64; EQ_BANDS]` and UI code iterates `0..EQ_BANDS`; the ergonomic cost
  outweighs the benefit now that the index is rejected.
- **Do not add a new `ProcessError` variant for an invalid index.**
  `InvalidParameter` already documents exactly this case, and finding 6's task
  established that no competing error enum should be introduced.
- **Do not add `debug_assert!` preconditions to the `*_validated` kernels.**
  They are reachable from the audio thread and `realtime-safety.md` forbids
  panics there; the documented precondition plus crate-private visibility
  matches the existing `process_validated` kernels.
- **Do not validate `sample_rate` in `Equalizer::set_band_gain` or
  `Equalizer::new`.** A zero/non-finite sample rate does produce garbage
  coefficients, but that is finding 6's geometry class in a module finding 6 did
  not cover. Recorded below as a known remaining gap rather than silently
  widening this task.

## Known remaining gaps (not fixed here)

- `Equalizer::new(channels, sample_rate)` still accepts zero channels and a
  zero/non-finite sample rate, producing `NaN`/infinite coefficients. Same class
  as audit finding 6; `eq.rs` was outside that task's scope.
- `Saturation` and `Crossfeed` raw setters were not reviewed; audit P2 §3 still
  owns the whole non-finite raw-setter sweep.
- Audit findings 1-8 changed public APIs without CHANGELOG entries. This task
  added its own entries; the earlier omissions are a separate cleanup.

## Final validation evidence

- `cargo test --all-features --lib processor::eq::tests` passed all 13 tests,
  including the five new ones.
- `cargo test --all-features --lib band_index` passed the three new
  boundary tests across `eq.rs` and the playback facade.
- `cargo fmt --all -- --check` passed after one rustfmt pass.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo clippy --all-targets --no-default-features --features rubato --
  -D warnings` passed.
- `cargo test --all-features` passed 430 library, 20 benchmark-support, 25
  resampler-support, 3 Windows deployment, and 6 doctests; the native-shim
  prerequisite test was the single expected ignore.
- `cargo test --no-default-features --features rubato` passed 463 library, 20
  benchmark-support, 25 resampler-support, 3 Windows deployment, and 6 doctests;
  the same single expected ignore.
- `cargo run --example equalizer_curve` printed the expected
  `output peak = 0.5000`, proving the fallible example path still runs.
- `git diff --check` reported only the pre-existing LF-to-CRLF working-copy
  warnings.
- No benchmark binary was executed, so this task makes no timing, regression,
  device, driver, DAC, or end-to-end latency claim.

## Regression value of the new tests

Each new test fails against the pre-change source:

- `out_of_range_eq_band_index_is_rejected_instead_of_acknowledged` — the facade
  previously returned `Ok(())`, so `unwrap_err()` panics.
- `out_of_range_band_index_is_rejected_without_touching_state` — the raw setter
  previously returned `()`.
- `non_finite_band_gain_is_rejected_and_cannot_poison_filter_history` — the
  clamped `NaN` previously reached the coefficients and every later sample came
  out `NaN`.
- `whole_bank_write_is_rejected_atomically_when_any_gain_is_non_finite` — the
  first nine finite gains were previously applied before the `NaN` entry.
- `checked_and_validated_band_writes_produce_identical_state` — new invariant
  guarding the callback kernel against drift from the public shell.
- `low_level_eq_setter_no_ops_for_an_invalid_band_index` — pins the deliberately
  retained infallible family policy so a later sweep is an explicit decision.
