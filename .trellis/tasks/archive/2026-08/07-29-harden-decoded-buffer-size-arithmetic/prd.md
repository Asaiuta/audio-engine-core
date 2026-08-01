# Harden decoded-buffer size arithmetic

## Goal

Make full-stream decoding reject untrusted or target-unrepresentable buffer
geometry before conversion, multiplication, reservation, or append, while
keeping the public diagnostics shape and the HTTP full-download budget policy
stable. Consolidate repeated sample/byte arithmetic where doing so creates one
auditable boundary rather than applying isolated overflow guards.

## Revalidation Verdict

Audit finding 7 is accurate in the current tree and is slightly broader than
the original wording. `StreamingDecoder::decode_all` casts container-derived
`raw_total_frames` to `usize`, repeats unchecked channel/sample-width
multiplication, reserves from the result, and checks incremental growth only
after `extend_from_slice`. `decode_memory_budget` also multiplies the resolved
MiB value without checking target capacity.

On a 32-bit target, 32,768 MiB cannot be represented as `usize`. More
importantly, a Rust `Vec` is limited to `isize::MAX` bytes, so even the 2,048
MiB default is one byte above the single-allocation ceiling. The effective
whole-MiB limit must therefore normalize to 2,047 MiB on such a target.

The builder's `staging_buffer_bytes` already demonstrates the required
`usize::try_from` plus `checked_mul` pattern. The existing HTTP full-download
path consumes one quarter of the same resolved budget and must continue to do
so.

## Requirements

- Introduce one decoder-private decoded-buffer size plan that derives checked
  interleaved sample and byte counts from frame/channel geometry and from
  incremental sample growth.
- Initial `decode_all` planning must use `usize::try_from` and checked
  arithmetic once, compare the validated byte count to the effective budget,
  and reuse the sample/byte values for reservation and diagnostics.
- Incremental decoding must borrow each decoded packet, validate
  `current + incoming` samples and bytes before mutating the destination, and
  reject over-budget growth before `extend_from_slice`.
- Fallible capacity reservation must map allocation/capacity failures into the
  existing `DecoderError::Decoder` model instead of adding a new public error
  enum or relying on infallible `Vec` growth.
- Resolve the memory budget against both the configured MiB bounds and the
  target's `isize::MAX` single-allocation ceiling. Keep
  `decode_memory_budget()` and `DecodeMemoryBudget` publicly compatible.
- Preserve the environment-source reporting semantics: a successfully parsed
  override reports the environment variable as its source even when clamped;
  a missing or invalid override reports `default`.
- Preserve HTTP non-Range download behavior, including its one-quarter share
  of the resolved decode budget.
- Preserve valid decode output, gapless trimming, cancellation, borrowed packet
  ownership, and fixed staging-buffer behavior.

## Acceptance Criteria

- [x] `u64::MAX` frame geometry and overflowing multi-channel geometry return a
      typed size error without panic or allocation.
- [x] Ordinary frame/channel geometry produces exact interleaved sample and
      byte counts from one size plan.
- [x] A simulated 32-bit default and maximum override both resolve to 2,047 MiB
      and a representable byte count.
- [x] An incremental packet that would cross the budget is rejected before the
      destination vector changes.
- [x] Initial and incremental reservations are fallible and allocation errors
      stay in `DecoderError::Decoder`.
- [x] Existing HTTP full-download tests retain the same quarter-budget policy.
- [x] Decoder-focused tests, both complete feature matrices, both strict
      Clippy matrices, rustfmt, diff check, and Trellis validation pass.
- [x] Final review records which broader refactors were adopted and rejected.

## Definition of Done

- All decoded-buffer sample/byte planning in `decode_all` has one checked
  implementation owner.
- Budget resolution is correct for the current target and is testable for a
  simulated 32-bit address space from a 64-bit host.
- The decoder correctness spec documents the allocation-boundary contract.
- Existing unrelated dirty work is preserved.
- No commit, push, or archive occurs without explicit user direction.

## Decision (ADR-lite)

**Context**: The defect appears in several arithmetic expressions, but those
expressions are all views of the same decoded-vector geometry. Patching each
operator independently would leave duplicated policy and a post-mutation
budget check.

**Decision**: Add a small decoder-private size plan for checked sample/byte
geometry, use a target-aware pure resolver inside diagnostics, and make
`decode_all` reserve and append through fallible preflight steps.

**Consequences**: The allocation boundary becomes directly unit-testable and
valid decoding retains the same public API. This adds a focused internal type
and helper rather than spreading conversions and error strings. It does not
generalize all allocation policy in the crate or redesign decode API variants.

## Out of Scope

- Merging `decode_next`, `decode_next_into`, `decode_next_borrowed`, and
  `decode_all` into a new public abstraction.
- Changing public `AudioInfo` mutability or the metadata/channel-layout model.
- Adding decoded-size-specific public `DecoderError` variants.
- Rewriting HTTP Range/full-download transport or changing its budget share.
- Treating this crate as 64-bit-only or adding cross-compilation infrastructure.
- Refactoring unrelated staging, seek, gapless, or codec logic.

## Technical Notes

- Primary code: `src/decoder/streaming.rs`, `src/diagnostics.rs`.
- Focused tests may live beside the private planning helpers; public decoder
  behavior remains covered by `src/decoder/tests.rs`.
- Contract update: `.trellis/spec/backend/decoder-correctness.md`.
- Existing pattern: `StreamingDecoderBuilder::staging_buffer_bytes`.
- Revalidation details and refactor decisions:
  `research/revalidation-and-refactor.md`.
