# Current Validation Audit

Date: 2026-08-10

## Scope

This audit revalidates release gate 6 against the current tree before any
implementation. It covers the original task claims, the public loudness
control surface, the EBU R128 wrapper, and the callback-facing publication
rules in:

- `src/processor/lockfree_params.rs`
- `src/processor/eq.rs`
- `src/processor/saturation.rs`
- `src/processor/fir_eq.rs`
- `src/processor/dynamic_loudness.rs`
- `src/processor/loudness/limiter.rs`
- `src/processor/loudness/atomic_state.rs`
- `src/processor/loudness/normalizer.rs`
- `src/processor/loudness/meter.rs`
- `src/processor/automix_analysis.rs`

The controlling specs are `realtime-safety.md`,
`dsp-state-correctness.md`, and `error-handling.md`.

## Verdict

The original Gate 6 description is materially stale. Commit `c30f1f7`
already established the shared finite-and-clamp policy for the originally
named EQ, saturation, volume, FIR EQ, dynamic-loudness, and limiter controls;
it also repaired the reported dynamic-loudness lost update and prevented an
unavailable meter from claiming reliability.

The remaining release blocker is narrower but still real: the older loudness
control and measurement surface can bypass or suppress that policy. Gate 6
should finish that boundary instead of rewriting the already-correct DSP
modules or introducing a general parameter schema.

## Original Findings Already Covered

### Shared policy and direct DSP controls

- `lockfree_params::sanitized` rejects non-finite values and clamps finite
  values to the published domain.
- Atomic EQ, saturation, crossfeed, limiter, volume, noise-shaper, and dynamic
  loudness publishers use the shared policy where their values have a public
  bounded domain.
- `Equalizer` exposes fallible checked setters. It validates a whole band bank
  before mutation and returns `ProcessError::InvalidParameter` for non-finite
  gains or invalid indices.
- `Saturation`, `FirEq`, and `DynamicLoudness` use infallible reject/retain
  semantics for non-finite writes. Whole-group setters validate before
  mutation.
- `PeakLimiter` rejects non-finite threshold and release writes. Its core
  threshold intentionally does not apply the facade range because the
  true-peak guard legitimately drives the core below the public minimum.

These modules need focused regression verification, not another policy
rewrite.

### Concurrent dynamic-loudness publication

`AtomicDynamicLoudnessParams::set_ref_volume_db` now performs its comparison
and update inside `SharedParams::update_if`. The regression test
`reference_volume_writes_cannot_lose_a_concurrent_strength_update` exercises
20,000 concurrent writes and verifies sibling fields are preserved.

### Meter reliability bandage

`LoudnessMeter::has_reliable_measurement` now checks `is_available()` and
requires at least one sample even for degenerate rates. An unavailable meter
therefore no longer reports its placeholder values as reliable. This closes
the narrow symptom but not the suppressed-failure API described below.

## Remaining Gaps

### 1. `AtomicLoudnessState` can still be poisoned or bypassed

All seven atomic fields are public. Downstream code can store arbitrary bits,
including NaN and invalid raw mode values, without using a setter. That makes
any setter policy unenforceable.

The public helpers also disagree with the established policy:

- `new` and `set_smoothing` can publish a NaN or unstable coefficient from a
  non-finite/negative time or zero rate.
- `set_target_gain` converts a rejected value to 0 dB, mutating valid state
  instead of retaining it.
- `set_album_gain` and `set_preamp_gain` publish non-finite values directly.
- `set_mode(u8)` accepts invalid encodings even though
  `set_normalization_mode` already provides the typed boundary.

The fields must become private, typed/read-only accessors must replace direct
loads, and the raw numeric mode setter must stop being public. Callback-adjacent
infallible setters should reject non-finite input and retain the previous
value. Construction should validate geometry and smoothing before publishing
the first state.

### 2. `LoudnessNormalizer` accepts inconsistent configuration

`LoudnessConfig` is freely constructible and deserializable, so validation
must happen when it crosses into `LoudnessNormalizer`.

- `new` validates only channels and nonzero sample rate.
- `set_config` mutates the limiter, smoothing state, flags, and stored config
  in sequence. A bad field can leave those owners disagreeing.
- `set_target_lufs`, `set_album_gain`, and `set_preamp_gain` accept non-finite
  values without a typed rejection at this high-level boundary.
- Invalid target/reference values can later poison ReplayGain arithmetic even
  if an atomic setter rejects the final result.

The constructor and public high-level setters should validate all affected
fields before mutation and return `ProcessError::InvalidParameter`. Zero
smoothing time is a legitimate immediate transition; negative or non-finite
time is invalid. Gate 6 should not invent arbitrary LUFS/gain limits where the
crate has no documented public domain. Existing published bounds, such as the
limiter facade range, remain the single source of truth where applicable.

### 3. `LoudnessMeter` suppresses setup and processing failures

`LoudnessMeter` stores `Option<ebur128::EbuR128>` and returns `Self` from both
constructors. `EbuR128::new` failure is logged and converted to `None`, and
`set_channel_map` failure is ignored. Readers then expose placeholder values
and callers must remember to query availability.

`process` also returns `()`:

- an incomplete interleaved frame is silently truncated;
- `add_frames_f64` errors are logged and discarded;
- no caller can distinguish a successful no-op from rejected input.

The local `ebur128` 0.1.10 API already returns a small typed error enum from
construction, channel-map setup, and frame ingestion. The crate should map it
at the public boundary rather than expose the dependency type or parse its
display text.

### 4. Callers currently cannot propagate meter failure

- `LoudnessNormalizer::process` already returns `ProcessError`, so streaming
  meter failures can flow through it without a new error family.
- The normalizer's offline `analyze_track`, `calculate_gain`, and
  `calculate_gain_with_mode` currently return bare `f64`; full propagation
  makes these methods fallible too.
- `analyze_automix` already returns module-owned `AutomixError`; it can add a
  typed loudness-analysis variant carrying `ProcessError`.
- Benchmarks and unit tests use valid geometry and can unwrap outside measured
  loops. The steady-state no-allocation assertion must include the fallible
  `process` call.

## Recommended Policy

Use the mixed policy already documented by the repository:

1. Infallible callback-adjacent/atomic setters reject non-finite writes and
   retain the previous valid value. Finite values clamp only when a published
   domain exists.
2. Public high-level constructors and setters return a typed error and perform
   no mutation on rejection.
3. Internal DSP values may use a wider domain than their facade. Do not apply
   facade bounds to private algorithm adjustments.
4. Multi-field writes validate first and publish once, or perform the decision
   and mutation under the existing writer serialization boundary.
5. Private validated kernels keep callback code allocation-free, lock-free,
   log-free, panic-free, and bounded.
6. Do not add a generic parameter schema/newtype framework without a real
   consumer.

## Feasible `LoudnessMeter` Boundaries

### A. Fallible construction and processing (recommended)

- Constructors return `Result<Self, ProcessError>` and store a concrete
  `EbuR128`.
- Channel-map failure aborts construction.
- `process` returns `Result<(), ProcessError>`, rejects incomplete frames, and
  maps backend ingestion failure without logging on the processing path.
- Normalizer analysis methods and AutoMix propagate the typed failure.
- `is_available` and the unavailable placeholder state disappear; reliability
  means only "enough successfully consumed audio".

This is the strongest and most truthful pre-1.0 contract. It changes more
signatures, but all in-tree call sites are bounded and the crate is explicitly
before its API freeze.

### B. Fallible construction only

- Constructors and channel-map setup become fallible and the backend becomes
  concrete.
- `process` stays infallible and continues truncating incomplete frames (or
  otherwise treats geometry as a documented precondition).

This removes the unavailable state with fewer signature changes, but leaves
measurement input behavior inconsistent with `AudioBlockRef`, `PeakLimiter`,
and `LoudnessNormalizer`.

### C. Compatibility-oriented availability/error state

- Keep infallible constructors and store an explicit error/availability state.
- Add a query for the setup failure and retain infallible processing.

This minimizes source breakage but still creates an object that cannot perform
its advertised operation. It is a weak fit for the current typed-error and
pre-1.0 release-gate direction.

## Verification Impact

Required focused evidence:

- invalid atomic/config writes preserve the prior bit patterns;
- high-level setters reject before any owner mutates;
- public atomic fields and raw numeric mode publication are absent from API
  snapshots;
- meter construction rejects invalid geometry and channel-map/backend setup;
- incomplete process geometry is typed and leaves measurement state unchanged
  under Approach A;
- normalizer and AutoMix preserve the meter error class;
- meter and loudness processing remain allocation-free after setup;
- Rubato-only and all-feature tests, strict Clippy, rustdoc, packaging, public
  API snapshots, and focused loudness/parameter benchmarks pass.
