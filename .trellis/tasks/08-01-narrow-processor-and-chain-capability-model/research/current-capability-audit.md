# Processor And Chain Capability Audit

## Scope

This note revalidates release gate 5 against the current tree after gates 1-4.
It covers the public streaming lifecycle trait, fixed callback-chain admission,
sample-rate construction, and callback/offline output-chain rate inputs.

## Current Boundary

### Streaming lifecycle and bypass

- `StreamingProcessor` is public, object-safe, and implemented by nine
  production types: seven adapters in `adapters.rs`, `ConvolverProcessor`, and
  `StreamingResampler`.
- The base trait requires `is_enabled` and `set_enabled` and supplies
  `supports_bypass`. `VolumeProcessor` and `StreamingResampler` return false
  from the capability query and keep `set_enabled` as a no-op.
- Repository-wide production search found no generic caller of
  `StreamingProcessor::{supports_bypass,is_enabled,set_enabled}`. Playback
  controls publish through concrete `Atomic*Params` handles or
  `ConvolverControl`; volume uses mute/gain semantics and resampling is graph
  geometry.
- Commit `c30f1f7` made the previous silent mismatch discoverable by adding
  `supports_bypass`, but it did not remove the unsupported operation from the
  base interface. Every external implementation still has to implement an
  enable predicate and setter even when the concept does not apply.

### Fixed callback chain

- `DspChain::process` passes one in-place block through every stage and owns no
  variable-I/O scratch storage.
- `c30f1f7` changed `DspChain::add` to return `Result` and rejects a stage whose
  `output_sample_rate_hz` changes the chain rate. This catches the ordinary
  unequal-rate resampler case before callback processing.
- The bound remains `P: StreamingProcessor`, so a same-rate buffering or other
  variable-I/O processor is still type-correct and can fail only when driven
  through `ProcessBuffers::InPlace`. Output-rate equality is narrower than the
  actual fixed in-place 1:1 admission contract.
- Rust commonly models optional behavior by refinement/marker traits rather
  than boolean capability probes on a broad base trait. The repository already
  follows the same ownership pattern for concrete control capabilities:
  `ConvolverControl` and `Atomic*Params` expose controls that are absent from
  the streaming lifecycle object.

### Sample-rate invariant

- `DspChain::new(u32)` and `with_capacity(usize, u32)` return `Self` and store
  zero unchanged. `add` later rejects every stage on a zero-rate chain, but an
  empty invalid chain can still process and report fallback timing.
- `set_sample_rate(0)` already returns
  `ProcessError::InvalidSampleRate`, so mutation is stricter than
  construction.
- `Default` silently selects 44.1 kHz even though sample rate is required
  topology. There are no production `DspChain::default()` consumers.

### Callback versus render inputs

- `OutputChainParams` combines common stage controls with both source and
  output sample rates.
- `build_callback_chain` uses only `output_sample_rate`, but rejects a zero
  `source_sample_rate` as malformed. A callback-only caller must therefore
  supply an irrelevant offline value.
- `OutputRenderChain` genuinely consumes both rates and owns the optional
  resampler. Current non-test render consumers are the output-render and
  quality benchmarks; the production playback pipeline builds only a callback
  chain and currently fills both rates with the device rate.

## Feasible Approaches

### A. Interface segregation and operation-owned rates (recommended)

- Remove `is_enabled`, `supports_bypass`, and `set_enabled` from
  `StreamingProcessor`. Keep effect controls on the existing concrete
  `Atomic*Params`/`ConvolverControl` APIs; do not add an unused generic bypass
  trait.
- Add public `FixedInPlaceProcessor: StreamingProcessor` as the explicit
  admission contract for `DspChain::add`. Implement it only for fixed 1:1
  stages, not `StreamingResampler`.
- Make `DspChain::{new,with_capacity}` return `Result`, reject zero immediately,
  and remove the arbitrary `Default` implementation.
- Remove `source_sample_rate` from `OutputChainParams`. Pass it only to
  `build_render_chain` / `build_render_chain_with_policy`, where it is consumed.

Benefits: impossible or irrelevant capabilities disappear from the base API;
fixed-chain compatibility is visible in trait bounds; invalid rate state is
not constructible; callback-only integrations no longer invent a source rate.

Trade-off: deliberate pre-1.0 break across trait implementations, chain
construction, render builder calls, tests, benches, and snapshots.

### B. Keep runtime capability queries

- Retain the current bypass methods and add another runtime I/O capability enum
  checked by `DspChain::add`.
- Make constructors fallible and stop validating source rate in callback build,
  but retain the combined params struct.

Benefits: smaller call-site migration and dynamic trait objects can be inspected
uniformly.

Trade-off: the base trait continues to require no-op operations, and callback
params still carry a false required field. Another capability query documents
the mismatch instead of narrowing it.

### C. Seal callback stages inside the crate

- Make chain addition crate-private or require a sealed fixed-stage trait, and
  expose only `OutputChainBuilder` for callback composition.
- Split callback and render parameter structs completely.

Benefits: strongest control over realtime topology and distinct builder inputs.

Trade-off: external users can no longer compose custom fixed stages in
`DspChain`; two public parameter types duplicate many control handles. No
current evidence justifies that restriction before 1.0.

## Recommendation

Choose approach A. It matches the archived audit's interface-segregation
direction, preserves public custom fixed-stage composition, and uses type-level
admission only where there is a real consumer (`DspChain`). It avoids adding an
unused `BypassableProcessor` abstraction: current enable controls already have
typed owners.

## Gate Boundary

- Do not redesign resampler geometry or algorithms; Gate 7 owns the resampler
  facade contract. Gate 5 only prevents a variable-I/O resampler from entering
  the fixed callback chain.
- Do not generalize parameter sanitization; Gate 6 owns that policy. Gate 5
  validates only the chain/output rates named by its audit findings.
- Do not build a general variable-I/O graph or add callback scratch allocation.
