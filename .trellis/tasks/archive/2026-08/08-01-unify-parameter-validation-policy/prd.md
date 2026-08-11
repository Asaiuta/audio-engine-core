# Unify the Parameter Validation Policy

1.0 release gate 6 of 9.

## Goal

Finish the pre-1.0 validation contract at the remaining loudness control and
measurement boundaries. Invalid values must not enter callback-facing state,
high-level configuration must reject atomically, and EBU R128 setup/processing
failure must not be represented as a usable meter with placeholder results.

## What I Already Know

- The original task description is partly stale. Commit `c30f1f7` already
  established `lockfree_params::sanitized` and aligned EQ, saturation, volume,
  FIR EQ, dynamic loudness, limiter, and their atomic publishers.
- `AtomicDynamicLoudnessParams::set_ref_volume_db` now updates under
  `SharedParams::update_if`; a concurrent regression test covers the former
  lost-update race.
- `LoudnessMeter::has_reliable_measurement` now rejects unavailable meters,
  but construction/channel-map/process errors are still suppressed.
- `AtomicLoudnessState` still exposes raw public atomics and permits invalid
  smoothing, album gain, preamp gain, and mode publication.
- `LoudnessNormalizer` still accepts unchecked `LoudnessConfig` and can leave
  its stored config, limiter, and atomic state inconsistent after a bad write.
- Gate 5 established that concrete control owners retain effect controls; this
  gate owns the value-validation and failure semantics of those controls.

## Research References

- [`research/current-validation-audit.md`](research/current-validation-audit.md)
  - current-tree audit, resolved original findings, residual loudness gaps,
    call-surface impact, and three feasible meter boundaries.

## Decision (ADR-lite)

**Context**: `LoudnessMeter` currently suppresses EBU R128 construction,
channel-map, and ingestion failures. Keeping an infallible process API would
also preserve silent truncation of incomplete interleaved frames, which is
inconsistent with the crate's checked audio-block boundaries.

**Decision**: Use Approach A. Both `LoudnessMeter` construction and processing
become fallible and return the existing typed `ProcessError` boundary. Store a
concrete EBU R128 backend, reject incomplete frame geometry, propagate failures
through `LoudnessNormalizer` and AutoMix, and remove the unavailable
placeholder state.

**Consequences**: This intentionally changes public signatures before the 1.0
API freeze. In-tree callers, tests, benches, public API snapshots, and rustdoc
must be migrated together. Setup may allocate and preserve backend diagnostics;
the processing path must remain allocation-free and use static typed errors.

## Requirements

- Treat the already-aligned EQ/saturation/volume/FIR/dynamic-loudness/limiter
  modules as a verified baseline. Do not rewrite them without a focused
  regression finding.
- Keep one mixed setter policy:
  - callback-adjacent infallible setters reject non-finite input and retain the
    prior value;
  - public fallible setters return `ProcessError::InvalidParameter` before any
    mutation;
  - finite values clamp only where a documented public domain exists;
  - internal core values may be wider than facade ranges.
- Make `AtomicLoudnessState` validation enforceable by hiding writable atomic
  fields, replacing direct reads with stable accessors, and removing the
  public raw `u8` mode publication path.
- Reject invalid target/album/preamp gains and smoothing updates without
  changing prior valid state. Zero smoothing time remains a valid immediate
  transition; negative/non-finite time and zero sample rate are invalid.
- Validate every affected `LoudnessConfig` field before constructing or
  reconfiguring `LoudnessNormalizer`. A rejected config/setter call must leave
  config, limiter, meter, gain state, and atomic publication unchanged.
- Do not invent arbitrary LUFS or gain limits where no public domain exists;
  require finiteness and reuse existing published bounds where applicable.
- Surface EBU R128 construction and channel-map failure through a typed crate
  boundary without exposing dependency display text as control flow.
- Under the selected meter policy, propagate all newly fallible operations
  through `LoudnessNormalizer`, AutoMix, benches, tests, and public API
  snapshots.
- Preserve realtime constraints: no new callback allocation, lock, logging,
  I/O, panic, or unbounded retry.

## Acceptance Criteria

- [x] Existing originally named direct DSP and atomic publisher tests still
      prove finite validation, range behavior, and all-or-nothing group writes.
- [x] `AtomicLoudnessState` has no public writable atomic fields and no public
      raw numeric mode setter.
- [x] NaN/infinity gain writes and invalid smoothing updates retain the prior
      valid atomic state bit-for-bit.
- [x] Invalid `LoudnessConfig` construction and reconfiguration return a typed
      error before any owner mutates.
- [x] High-level target/album/preamp setters follow the selected fallible
      contract and cannot poison ReplayGain or callback arithmetic.
- [x] Meter backend and channel-map initialization failures are not converted
      into a usable placeholder meter.
- [x] Meter input geometry/backend failures follow the selected processing
      contract, and callers preserve their typed error class.
- [x] Reliability becomes true only after successful construction and one
      successfully consumed 400 ms momentary window.
- [x] Concurrent reference-volume and sibling-field updates remain covered and
      cannot overwrite one another.
- [x] Steady-state meter/normalizer processing stays allocation-free.
- [x] Rubato-only and all-feature tests, strict Clippy, rustdoc, packaging,
      public API snapshots, and focused parameter/loudness benchmarks pass.

## Definition of Done

- Public and callback-adjacent validation semantics are explicit and tested.
- No writable public field can bypass the loudness publication policy.
- Initialization/configuration/processing failure cannot be mistaken for
  success or a meaningful measurement.
- Realtime publication and processing remain bounded and allocation-free.
- New executable contracts are recorded in backend specs.
- Changes are committed coherently; nothing is pushed or archived without
  explicit user direction.

## Expansion Sweep

### Future Evolution Included

- Preserve typed control accessors so a future UI/control layer can build on a
  stable boundary without exposing unchecked DSP storage.
- Keep private validated kernels available for already-checked callback data.

### Related Scenarios Included

- Constructor defaults, deserialized configs, direct setters, atomic state,
  AutoMix, benchmark fixtures, and public API snapshots must agree.
- Reset/reconfiguration must preserve the last valid configuration and must
  not revive a rejected value.

### Failure And Edge Cases Included

- NaN, positive/negative infinity, signed zero, zero/negative smoothing,
  invalid raw mode attempts, invalid channel/rate geometry, incomplete
  interleaved frames, channel-map/backend failure, and concurrent partial
  publication.
- Internal limiter thresholds derived below the facade minimum remain valid.

## Feasible Approaches

### A. Fallible meter construction and processing (Selected)

- `LoudnessMeter::new/with_layout -> Result<Self, ProcessError>`.
- Store a concrete EBU R128 backend; abort on channel-map failure.
- `LoudnessMeter::process -> Result<(), ProcessError>`; reject incomplete
  frames and propagate backend ingestion failure without processing-path logs.
- Propagate through normalizer analysis methods and a typed AutoMix variant.
- Remove the unavailable placeholder state and `is_available` API.

This is the most truthful pre-1.0 contract and matches the existing checked
audio-block/process APIs. It has the widest, but bounded, signature impact.

### B. Fallible meter construction only

- Make setup/channel-map fallible and store a concrete backend.
- Keep `process` infallible and retain a documented truncation/precondition
  policy.

This changes fewer signatures but keeps meter geometry inconsistent with the
rest of the loudness processing surface.

### C. Compatibility-oriented availability state

- Keep infallible constructors and retain explicit unavailable/error state.
- Add diagnostics while leaving processing infallible.

This minimizes source changes but still permits constructing an object that
cannot perform its advertised operation. It is not recommended before the API
freeze.

## Out of Scope

- Gate 7 resampler facade geometry or algorithm changes.
- A general parameter reflection/schema/newtype framework.
- UI behavior, device/backend integration, or new loudness algorithms.
- Arbitrary new LUFS/gain product ranges without an existing documented
  domain.
- Reworking already-correct direct DSP modules without new failing evidence.

## Technical Notes

- Primary implementation files:
  `src/processor/loudness/atomic_state.rs`,
  `src/processor/loudness/normalizer.rs`,
  `src/processor/loudness/meter.rs`, and
  `src/processor/automix_analysis.rs`.
- Supporting files: `src/config.rs`, public exports/API snapshots, focused
  benches/tests, and backend specs.
- Reuse `ProcessError`, `AudioBlockRef`, existing published parameter bounds,
  and private validated-kernel patterns. Do not expose `ebur128::Error`.
- `ebur128` 0.1.10 returns typed errors from construction, channel-map setup,
  and interleaved frame ingestion; mapping belongs at the crate boundary.

## Implementation Plan

1. Close `AtomicLoudnessState` bypasses and add validated/read-only accessors.
2. Make `LoudnessNormalizer` configuration and direct high-level setters
   validate atomically through `ProcessError`.
3. Make `LoudnessMeter` construction/process fallible and migrate normalizer,
   AutoMix, tests, and benchmarks.
4. Update public API snapshots/spec contracts and run the full release-gate
   verification matrix.
