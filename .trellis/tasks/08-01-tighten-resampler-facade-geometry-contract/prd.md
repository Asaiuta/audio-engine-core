# Tighten the Resampler Facade Geometry Contract

1.0 release gate 7 of 9.

## Goal

Make the resampler facade truthful and checked before the 1.0 API freeze.
One-shot and streaming entry points must enforce the same interleaved geometry,
each public quality tier must resolve to a distinct backend recipe, and buffer
capacity must use explicit frame units, exact rational arithmetic, and typed
overflow instead of magic margins or error-swallowing `usize` helpers.

## What I Already Know

- `Resampler::new` is infallible, and `resample_parallel` bypasses all geometry
  validation when rates are equal.
- Unequal-rate one-shot input silently drops a trailing partial frame.
- `StreamingResampler` already rejects zero geometry and its `AudioBlockRef`
  boundary rejects incomplete frames.
- SoXR maps `Standard` and `High` to the same `Bits20` recipe despite exposing
  four distinct public presets and benchmark rows.
- Three public sizing helpers mix samples, frames, backend-step claims,
  unchecked float/arithmetic, and an unnamed 64-frame margin.
- `RateBoundary` uses the process-capacity helper as a finish bound even though
  the crate already models resampler latency and finite tail explicitly.
- This is a deliberate pre-1.0 public API correction; compatibility wrappers
  are not required when they would preserve misleading contracts.

## Research References

- [`research/current-resampler-facade-audit.md`](research/current-resampler-facade-audit.md)
  - current-tree evidence, dependency recipe identity, capacity callers,
    lifecycle coupling, alternatives, and verification impact.
- Archived maintainability audit:
  `.trellis/tasks/archive/2026-08/07-28-codebase-maintainability-audit/research/03c-resampler-modules.md`.

## Decision (ADR-lite)

**Context**: The public facade currently makes malformed one-shot input depend
on whether rates happen to match, exposes two names for one SoXR recipe, and
uses process estimates as both caller capacity and a complete-render safety
bound. Keeping wrappers would preserve ambiguous zero/error and sample/frame
semantics immediately before the API freeze.

**Decision**: Use the checked frame-domain approach. Make `Resampler::new`
fallible; validate one-shot input with the shared audio-block boundary before
bypass; map every SoXR quality tier distinctly; replace the three weak sizing
helpers with `process_output_capacity_frames(input_frames) -> Result<usize,
ResamplerError>`; centralize a named backend burst slack; and calculate
output-chain finish bounds from exact input duration plus declared
latency/tail.

**Consequences**: In-tree one-shot construction and capacity callers must be
migrated, and both public API snapshots change intentionally. Capacity is a
setup/provisioning hint rather than a progress guarantee; callers still obey
`ProcessProgress`. Complete render bounds come only from timing metadata.

## Requirements

- Make one-shot construction reject zero channels or sample rates with the
  existing typed `ResamplerError` classes.
- Validate complete interleaved frames before the equal-rate fast path and
  preserve the shared `AudioBlockError::IncompleteFrame` details through a
  typed resampler boundary.
- Keep empty input valid for valid geometry and preserve equal-rate samples
  exactly.
- Give `Low`, `Standard`, `High`, and `UltraHigh` distinct SoXR recipes:
  `Low`, `Medium`, `Bits20`, and `Bits28` respectively.
- Replace `max_output_len_for_input`, `max_output_samples_per_chunk`, and
  `input_frames_for_output_frames` with one public checked method whose input
  and output are per-channel frames.
- Use exact integer ceiling rate conversion and checked arithmetic. Overflow
  must return `ResamplerError::CapacityOverflow`, never saturate, wrap, panic,
  or become a legitimate zero in library code.
- Give any retained backend burst slack one named constant/helper and reuse it
  for public process provisioning, streaming scratch, and one-shot scratch.
- Keep `ProcessProgress` and backpressure authoritative; capacity does not
  promise that a backend always consumes all input for arbitrary caller state.
- Remove the output-chain dependency on process-capacity estimation. Reuse the
  existing checked duration/latency/tail finish model.
- Update all in-tree callers, rustdoc, changelog, public API snapshots, tests,
  and benchmark provisioning for the selected breaking surface.
- Preserve realtime constraints: no new process/finish allocation, lock,
  logging, I/O, panic, or unbounded retry.

## Acceptance Criteria

- [x] `Resampler::new` returns a typed error for zero channels/rates.
- [x] Equal-rate and unequal-rate one-shot calls reject the same incomplete
      interleaved frame with preserved sample/channel counts.
- [x] Valid equal-rate input remains bit-identical and valid empty input stays
      empty.
- [x] Every public SoXR quality tier resolves to a distinct pinned recipe.
- [x] The sole public process-capacity method uses frame units, exact rational
      ceiling conversion, checked arithmetic, and typed overflow.
- [x] The obsolete inverse/internal-step helpers are absent from both public
      API snapshots.
- [x] Repeated stateful processing across representative up/down/equal rates
      fits the named capacity contract under both supported feature matrices.
- [x] Streaming/one-shot scratch sizing shares the same named slack owner.
- [x] Output-chain finish capacity includes declared latency and finite tail;
      no process-capacity helper is used as a drain bound.
- [x] Existing duration, reset, terminal drain, no-allocation, and benchmark
      work-accounting tests remain green.
- [x] Rubato-only and all-feature tests, strict Clippy, rustdoc, packaging,
      public API snapshots, and focused resampler/lifecycle benchmarks pass.

## Definition of Done

- One-shot and streaming geometry fail consistently at typed boundaries.
- Quality names identify distinct resolved backend recipes.
- Public capacity units and failure semantics are unambiguous and checked.
- Process provisioning and complete-render timing are separate contracts.
- New executable contracts are recorded in backend specs.
- Changes are committed coherently; nothing is pushed or archived without
  explicit user direction.

## Expansion Sweep

### Future Evolution Included

- Preserve a backend-neutral frame-domain capacity API even if native backend
  chunk sizes change.
- Make a future nonlinear output-chain rate boundary rely on timing metadata,
  not an accidental linear-phase invariant.

### Related Scenarios Included

- One-shot, streaming, output-chain, lifecycle-memory, comparison benchmark,
  quality matrix, rustdoc, and public API baseline callers migrate together.
- Both supported feature matrices implement the same facade contract while
  retaining private backend scheduling.

### Failure And Edge Cases Included

- Zero geometry, empty input, incomplete frames, equal-rate bypass, extreme
  frame counts/rates, downsampling ceiling, repeated buffered blocks, and
  finite-tail drain bounds.

## Out of Scope

- Replacing SoXR or Rubato, changing their DSP algorithms, or claiming new
  performance/quality results beyond the corrected recipe identity.
- Avoiding backend construction for equal-rate streaming; setup-cost cleanup
  is independent of the Gate 7 correctness contract.
- Changing one-shot channel-divergence fallback without a reproducible backend
  divergence.
- Device, driver, DAC, or end-to-end playback latency claims.

## Technical Notes

- Primary implementation files: `src/processor/resampler/mod.rs`,
  `src/processor/resampler/soxr_backend.rs`, and
  `src/processor/output_chain.rs`.
- Supporting files: resampler/lifecycle benches, `CHANGELOG.md`, public API
  snapshots, focused tests, and backend specs.
- Reuse `AudioBlockRef`, `FrameDuration`, `TailSpec`, `ProcessProgress`, and
  the existing output-chain finish helper. Do not create a second geometry or
  timing model.
- The named slack is a process-burst provisioning contract, not semantic tail
  and not an internal backend-step identity.

## Implementation Plan

1. Centralize geometry and checked frame-capacity arithmetic; make one-shot
   construction/input validation fallible and update its tests/callers.
2. Replace public sizing helpers and migrate library/benchmark consumers.
3. Correct and test SoXR recipe resolution; repair output-chain finish bounds.
4. Update snapshots/docs/specs and run the complete Gate 7 validation matrix.
