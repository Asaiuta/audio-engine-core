# Finding 7 revalidation and refactor review

## Current evidence

- `src/decoder/streaming.rs:481-498` converts container-derived
  `raw_total_frames` with `as usize`, then separately multiplies frames,
  channels, and `size_of::<f64>()` for budget checking, sample capacity, and
  logging. Debug builds can panic and optimized builds can wrap before the
  budget comparison.
- `src/decoder/streaming.rs:503-513` calls `decode_next_into`, which extends the
  destination, before checking the resulting byte count. A rejected packet has
  already mutated and may already have grown the vector.
- `src/diagnostics.rs:31-47` clamps the configured MiB value and then performs
  unchecked `usize` multiplication. The configured maximum is not
  representable in bytes on 32-bit targets.
- `Vec<T>` capacity is bounded by `isize::MAX` bytes. On a 32-bit target the
  current 2,048 MiB default is 2,147,483,648 bytes, one byte above that limit;
  the largest safe whole-MiB budget is 2,047 MiB.
- `StreamingDecoderBuilder::staging_buffer_bytes` already uses
  `usize::try_from` followed by checked frame/channel/sample-width
  multiplication, so the crate has an established checked-size pattern.
- `src/decoder/source/http.rs` uses the same resolved budget and divides it by
  four for a non-Range full download. Target normalization should flow through
  that existing consumer without changing the policy.

## Adopted refactors

1. Add one private value object that carries both checked interleaved sample
   count and byte count. This removes three repeated calculations and makes
   ordinary and overflow geometry directly testable.
2. Add a pure diagnostics resolver parameterized by the target allocation
   ceiling. Production passes `isize::MAX`; tests pass `i32::MAX` to prove
   32-bit behavior without requiring a cross toolchain.
3. Make `decode_all` consume `decode_next_borrowed`, preflight checked growth,
   call `try_reserve_exact`, and only then append. This preserves the existing
   zero-copy staging ownership and makes a budget rejection failure-atomic.
4. Use the existing `DecoderError::Decoder` category and keep public
   diagnostics fields/constants stable.

## Rejected broader refactors

- Do not merge the four public decode entry points. Their ownership trade-offs
  are intentional and unrelated to the arithmetic defect.
- Do not redesign `DecoderError`. A more granular allocation/limit taxonomy may
  be useful later, but it would widen compatibility scope without improving
  this boundary's correctness.
- Do not move HTTP download code or alter the quarter-budget rule. It is a
  separate trust-boundary implementation and already consumes the shared
  resolved limit.
- Do not make a crate-wide generic byte-size utility. The decoded-vector plan
  is domain-specific, has one production owner, and a generic abstraction would
  expose more API than reuse evidence justifies.
- Do not change metadata mutability, gapless accounting, seek behavior, or
  fixed packet staging. Those concerns have separate contracts and tasks.

## Expected validation

- Pure size-plan tests for exact geometry, `u64::MAX`, channel multiplication,
  and sample-width multiplication.
- Pure budget tests for missing/configured values and simulated 32-bit limits.
- Failure-atomic append test proving the destination is unchanged when the next
  packet exceeds the limit.
- Full supported Clippy/test matrices to catch feature-gated HTTP and decoder
  interactions.

## Final review and evidence

- The size plan remained private to `decoder::streaming`; no generic crate-wide
  byte-size abstraction or public API was added.
- `decode_all` now reuses the plan for initial diagnostics/reservation and uses
  borrowed packets with checked, failure-atomic destination append.
- Initial and incremental capacity changes call `try_reserve_exact`, so
  capacity/allocation failures stay in `DecoderError::Decoder`.
- The diagnostics resolver is target-parameterized and production passes
  `isize::MAX`. A review pass corrected the native-target test itself so it
  also remains valid when actually compiled on 32-bit.
- Existing HTTP Range and full-download tests passed without transport changes;
  the shared effective budget continues to flow into that boundary.
- `cargo check --all-targets --all-features` passed.
- Both strict Clippy commands passed with `-D warnings`.
- `cargo fmt --all -- --check`, focused `git diff --check`, and Trellis context
  validation passed.
- `cargo test --all-features`: 422 library, 20 benchmark-support, 25
  resampler-support, 3 Windows runtime, and 6 doctests passed; one native-shim
  prerequisite test was ignored as documented.
- `cargo test --no-default-features --features rubato`: 455 library, 20
  benchmark-support, 25 resampler-support, 3 Windows runtime, and 6 doctests
  passed; the same native-shim prerequisite test was ignored.
