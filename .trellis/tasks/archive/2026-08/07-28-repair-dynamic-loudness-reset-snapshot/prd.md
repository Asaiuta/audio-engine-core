# Repair dynamic loudness reset snapshot adoption

## Goal

Keep a logically new stream consistent with the still-published dynamic
loudness controls. Reset must clear signal and smoother history, re-arm the
streaming lifecycle, and then rebuild the direct processor from the adapter's
already-adopted snapshot even when no new control generation is published.

## Revalidation verdict

The 2026-07-28 audit finding is accurate in the current tree.
`DynamicLoudness::reset` clears smoother targets, the loudness factor, applied
gains, and active bands. `DynamicLoudnessProcessor::reset` delegates to it but
keeps `cached_generation` unchanged. The next process call therefore sees no
new atomic generation and does not restore `cached.volume` or
`cached.strength`, leaving the new stream persistently uncompensated.

## Requirements

- Make construction, changed-generation adoption, and reset reuse one private
  path that applies `cached.volume` and `cached.strength` to the direct DSP.
- Reset the direct processor first, then reapply the adapter's cached snapshot,
  then re-arm `FixedLifecycle`.
- Do not force an atomic reload or alter `cached_generation`; reset must use the
  exact snapshot already adopted at the preceding block boundary.
- Preserve the callback boundary: no allocation, lock, logging, I/O, or panic.
- Keep enabled/bypass behavior, sample-rate behavior, public parameter types,
  telemetry schema, and `DynamicLoudness` public methods unchanged.

## Acceptance Criteria

- [x] With a published non-unity volume and strength, a used-then-reset adapter
      produces bit-identical output to a fresh adapter built from the same
      snapshot, without another parameter publication.
- [x] The reset path retains the cached generation and restores the same direct
      loudness factor, strength, and smoother targets as fresh construction.
- [x] Applying cached controls has one implementation owner used by constructor,
      generation sync, and reset.
- [x] Reset remains allocation-free and re-arms processing after terminal or
      prior-stream state.
- [x] Focused tests, both supported strict Clippy/test matrices, rustfmt, diff
      check, and Trellis validation pass.
- [x] Final review records adopted and rejected broader refactors.

## Definition of Done

- One private method owns snapshot-to-DSP adoption.
- Regression coverage fails against the pre-fix reset behavior.
- DSP state-correctness spec records reset/snapshot retention.
- Existing unrelated dirty work remains untouched.
- No commit, push, or archive occurs without explicit user direction.

## Decision (ADR-lite)

**Context**: The direct DSP intentionally owns signal/smoother state but does
not store the adapter's published volume snapshot.

**Decision**: Reapply the adapter's cached snapshot after direct reset and
centralize that adoption in the adapter.

**Consequences**: Reset/fresh behavior becomes deterministic without adding a
second volume owner to `DynamicLoudness` or rereading control state mid-boundary.
The retained generation remains truthful because no new snapshot was adopted.

## Out of Scope

- Changing direct `DynamicLoudness::reset` semantics or storing volume in the
  direct DSP.
- Redesigning dynamic-loudness parameters, telemetry, smoothing, filters, or
  compensation curves.
- Changing sample-rate update behavior, enable/bypass semantics, or public APIs.
- Addressing later audit findings about raw processor geometry or parameter
  validation.

## Technical Notes

- Primary code: `src/processor/adapters.rs`,
  `src/processor/adapters/tests.rs`.
- Direct reset evidence: `src/processor/dynamic_loudness.rs`.
- Contracts: `.trellis/spec/backend/dsp-state-correctness.md`,
  `.trellis/spec/backend/streaming-lifecycle.md`, and
  `.trellis/spec/backend/realtime-safety.md`.
