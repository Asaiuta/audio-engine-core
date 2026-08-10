# Current Resampler Facade Audit

Date: 2026-08-10

## Scope

This audit revalidates release gate 7 against the current tree before
implementation. It covers one-shot and streaming geometry, public capacity
helpers, backend quality resolution, and the output-chain finish bound in:

- `src/processor/resampler/mod.rs`
- `src/processor/resampler/soxr_backend.rs`
- `src/processor/output_chain.rs`
- `src/processor/traits.rs`
- resampler callers under `benches/`
- both committed public API snapshots

The controlling specs are `realtime-safety.md`, `streaming-lifecycle.md`,
`error-handling.md`, and `quality-guidelines.md`.

## Verdict

All three Gate 7 findings remain live. The one-shot facade stores invalid
geometry and validates only after its equal-rate bypass; unequal-rate input
silently floors an incomplete final frame. The SoXR backend still maps
`Standard` and `High` to the same `Bits20` recipe. The streaming facade still
exports three `usize` sizing helpers whose names mix samples, frames, internal
steps, estimates, and finish bounds while their arithmetic uses float casts,
unchecked multiplication/addition, and an unnamed 64-frame margin.

The DSP engines do not need redesign. The smallest coherent correction is a
fallible facade constructor, shared block geometry validation, a single
checked rational frame-domain capacity contract, distinct SoXR recipes, and a
finish bound derived from the existing latency/tail model.

## Current Findings

### One-shot geometry disagrees with streaming geometry

- `Resampler::new` returns `Self` and stores zero channels or rates.
- `resample_parallel` returns `input.to_vec()` for equal rates before checking
  rates, channels, or frame completeness.
- Unequal-rate input uses `input.len() / channels`; deinterleaving then ignores
  the final `input.len() % channels` samples.
- `StreamingResampler` rejects zero geometry at construction, and
  `AudioBlockRef` returns `AudioBlockError::IncompleteFrame` for the same
  malformed interleaved input.

Gate 7 should make `Resampler::new` return `Result<Self, ResamplerError>`, use
one shared resampler-geometry validator, and route one-shot input through the
existing `AudioBlockRef` boundary before the equal-rate fast path. A typed
`ResamplerError` variant may transparently carry `AudioBlockError`; no second
incomplete-frame parser is needed.

### SoXR quality identity is duplicated

The locked `soxr` 0.6.0 dependency exposes distinct `QualityRecipe` values:
`Low`, `Medium`, `Bits20` (`high()`), and `Bits28` (`very_high()`). The current
mapping uses `high()` for both `Standard` and `High`, even though Rubato and the
benchmark matrix present those as distinct tiers.

The resolved mapping should be:

| Public tier | SoXR recipe |
| --- | --- |
| `Low` | `Low` |
| `Standard` | `Medium` |
| `High` | `Bits20` / `high()` |
| `UltraHigh` | `Bits28` / `very_high()` |

A backend-local exhaustive test should pin this mapping. Once every public
tier resolves distinctly, existing requested-quality benchmark labels again
identify distinct SoXR recipes; no alias metadata is required.

### Capacity helpers mix units and error semantics

- `max_output_len_for_input(input_samples) -> usize` accepts interleaved
  samples, floors incomplete input, uses floating-point rate arithmetic, and
  adds `channels * 64` without naming what is being bounded.
- `max_output_samples_per_chunk() -> usize` claims one backend step but always
  reuses the 16,384-input-frame SoXR adapter layout. Rubato's native fixed step
  is 1,024 frames. It also converts layout errors into zero.
- `input_frames_for_output_frames() -> usize` has no current caller and adds a
  second unexplained 64-frame margin.
- The same float conversion and margin occur in one-shot scratch sizing and
  `streaming_buffer_layout`; later byte multiplication is checked, but the
  frame calculation is not.

Replace the three public helpers with one explicit setup-time contract:

```text
process_output_capacity_frames(input_frames) -> Result<usize, ResamplerError>
```

The result is per-channel frames, not interleaved samples. It uses exact
integer ceiling rate conversion, checked addition, and one named backend-burst
slack owned by the facade. Backpressure remains authoritative. The same helper
must size reusable SoXR scratch and one-shot scratch so the margin has one
owner and one testable meaning. Callers perform explicit checked or deliberate
saturating frame-to-sample conversion according to whether they are library
code or benchmark-only provisioning.

The unused inverse helper and the falsely named internal-step helper should be
deleted rather than renamed into unsupported promises.

### Output-chain finish uses the wrong contract

`RateBoundary::finish_frame_limit` treats the process-capacity estimate for all
previous input plus one render block as a hard drain bound. That estimate does
not include `StreamingResampler::latency()` or `tail()`. Current construction
uses linear phase, so both resolved backends report duration-aligned output,
but the boundary accepts a general `StreamingResampler` and would fail for a
future nonlinear configuration whose finite tail exceeds the old margin.

`output_chain.rs` already owns a checked `finish_frame_limit` implementation
that converts input duration and includes declared latency/tail. The rate
boundary should reuse it with the actual resampler timing instead of deriving
a second estimate from a public process-capacity helper.

## Expansion Sweep

### Future evolution included

- Keep the public capacity method backend-neutral and frame-domain so a future
  backend can change its private native step without changing caller units.
- Keep latency/tail as the authoritative complete-render model, allowing a
  future nonlinear output-chain boundary without another sizing API break.

### Related scenarios included

- One-shot equal-rate and unequal-rate paths must reject identical malformed
  geometry.
- SoXR and Rubato feature matrices must expose the same public facade and pass
  the same capacity/geometry tests even though their private steps differ.
- Benchmarks, lifecycle-memory accounting, output-chain rendering, rustdoc,
  changelog, and public API snapshots must migrate together.

### Failure and edge cases included

- Zero channels/rates, empty input, incomplete interleaved frames, equal-rate
  bypass, extreme rate/count overflow, downsampling that rounds to one frame,
  repeated stateful blocks, and finite nonlinear tails.

## Feasible Approaches

### A. Checked frame-domain facade (selected)

- Make the one-shot constructor fallible.
- Reuse `AudioBlockRef` validation before bypass.
- Replace all three weak public helpers with one checked frame-domain process
  capacity API.
- Name and centralize backend burst slack.
- Derive finish bounds from latency/tail.

This is the clearest pre-1.0 contract and removes rather than preserves
ambiguous surface area.

### B. Compatibility wrappers around the old helpers

Keep the old signatures and add checked methods alongside them, with the old
methods saturating or returning zero. This reduces immediate source breakage
but preserves ambiguous names and makes overflow indistinguishable from a
legitimate zero capacity.

### C. Backend-specific public capacity types

Expose resolved backend step sizes and pending-state models. This could be
more exact, but it leaks private scheduling and is unnecessary because
callers already honor backpressure.

## Out of Scope

- Replacing the SoXR or Rubato DSP engines.
- Removing equal-rate backend construction; that is a separate setup-cost
  cleanup, not required for facade correctness.
- One-shot multi-mono divergence fault injection and policy changes; current
  Gate 7 evidence does not demonstrate a reachable divergence.
- New resampling algorithms, quality tuning beyond the distinct SoXR mapping,
  or new performance claims.

## Verification Impact

- Focused unit tests for fallible one-shot geometry and equal/unequal residual
  frame parity.
- Exact/overflow capacity tests plus repeated stateful process calls proving
  the named capacity consumes the supplied block and bounds produced frames.
- Exhaustive SoXR recipe mapping test.
- Output-chain finish-bound regression using a finite-tail resampler contract.
- Both supported feature test and strict Clippy matrices, rustfmt, rustdoc,
  packaging, public API snapshots, and focused resampler/lifecycle benchmarks.

