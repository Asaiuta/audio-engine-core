# Narrow the Processor and Chain Capability Model

1.0 release gate 5 of 9.

## Goal

Make processor capabilities and fixed-chain topology truthful before the 1.0
API freeze. Remove or narrow contracts that current implementations silently
ignore, reject invalid chain geometry at construction, and ensure the callback
facade exposes only source-rate inputs it actually uses.

## What I Already Know

- `StreamingProcessor::set_enabled` currently promises transparent bypass, but
  at least volume and resampler implementations do not honor that promise.
- `DspChain::add` accepts processors whose variable input/output geometry the
  fixed in-place chain cannot drive correctly.
- `DspChain::new` accepts a zero sample rate.
- `OutputChainParams` requires a `source_sample_rate` value that the callback
  path ignores.
- This is release gate 5 of 9 and may make deliberate pre-1.0 public API
  changes.

## Assumptions (Temporary)

- Capability truth is more important than retaining unused compatibility
  methods before the first release.
- The fixed in-place callback chain should reject unsupported processor
  geometry during setup rather than fail or reinterpret it during processing.
- Any bypass capability retained in a public trait must have executable tests
  for every implementation that advertises it.
- Offline render and callback chains may need distinct construction contracts
  if their supported geometry differs.

## Open Questions

None. The user chose to continue with the recommended interface-segregation
approach after the current-tree audit.

## Requirements (Evolving)

- Audit every `StreamingProcessor` implementation and consumer before changing
  the trait.
- Remove enable/bypass from the base streaming lifecycle and retain controls on
  concrete typed parameter/control handles that actually own the state.
- Prevent `DspChain` from accepting processors it cannot execute with its fixed
  in-place topology through a public `FixedInPlaceProcessor` refinement bound.
- Reject zero sample rates at the setup boundary with a typed error.
- Remove or give semantics to callback-facing source-rate parameters that are
  currently ignored.
- Preserve realtime constraints: no new callback allocation, locking, logging,
  I/O, panic, or unbounded capability dispatch.
- Update public API snapshots, tests, benches, docs, and Trellis specs for the
  selected breaking surface.

## Acceptance Criteria (Evolving)

- [x] The base streaming lifecycle has no unsupported enable/bypass operation;
      concrete control owners retain the existing effect controls.
- [x] Fixed `DspChain` construction rejects variable-I/O or otherwise
      incompatible processors before callback processing.
- [x] Zero sample-rate construction returns a typed error without panic.
- [x] Callback and offline output-chain builders expose only parameters they
      validate or consume.
- [x] Existing fixed-stage callback paths remain allocation-free and preserve
      process/finish/reset lifecycle semantics.
- [x] Both supported test and strict Clippy feature matrices pass.
- [x] Public API snapshots, rustdoc, packaging, and focused chain/lifecycle
      benchmarks pass.

## Definition of Done

- Base traits describe capabilities shared by all implementors.
- Chain admission and rate validation fail at setup with typed errors.
- Callback source-rate semantics are explicit and non-duplicative.
- New contracts are recorded in executable backend specs.
- Changes are committed coherently; nothing is pushed or archived without
  explicit user direction.

## Expansion Sweep

### Future Evolution

- Preserve a clear boundary for a future variable-I/O graph without making the
  fixed callback chain pretend to support it now.
- Keep capability discovery extensible without callback-time dynamic probing.

### Related Scenarios

- Callback and offline output chains must agree where they share a processor
  contract and intentionally differ where topology permits more.
- Builder, controller, direct `DspChain`, adapters, and benchmarks must use the
  same admission and sample-rate rules.

### Failure And Edge Cases

- Zero/changed rates, bypass during buffered tail state, variable progress,
  reset after bypass, finish/drain while disabled, and processors whose output
  frame count differs from input.
- Public implementors outside the crate and semver effects of sealing,
  splitting, or removing trait methods.

## Out of Scope (Temporary)

- Building a general audio graph or scheduler.
- Changing DSP algorithms or resampler quality.
- Adding device/backend integration to this app-agnostic crate.
- Gate 6+ parameter-validation and resampler-geometry work except where a
  narrow boundary dependency must be recorded.

## Technical Notes

- Likely code: `src/processor/traits.rs`, adapters and processor
  implementations, `src/processor/dsp_chain.rs`, `src/processor/output_chain.rs`,
  `src/pipeline.rs`, tests, benches, and public API snapshots.
- Likely specs: `.trellis/spec/backend/streaming-lifecycle.md`,
  `realtime-safety.md`, `error-handling.md`, and `quality-guidelines.md`.

## Research References

- [`research/current-capability-audit.md`](research/current-capability-audit.md)
  - current trait consumers, fixed-chain admission, rate invariants, and
    callback/render input alternatives.

## Decision (ADR-lite)

**Context:** `StreamingProcessor` is a public lifecycle object, but its
mandatory bypass methods describe effect controls, volume muting, and rate
geometry with one interface. `DspChain` is fixed in-place while accepting the
broader trait, and callback construction requires an offline-only source rate.

**Decision:** Use interface segregation. Remove `is_enabled`,
`supports_bypass`, and `set_enabled` from `StreamingProcessor`; add a
`FixedInPlaceProcessor` refinement trait for `DspChain::add`; make chain rate
constructors fallible; and pass source rate only to offline render builders.

**Consequences:** This is a deliberate pre-1.0 breaking change. Generic
streaming implementors become lifecycle-only, fixed callback admission is
visible in the type bound, zero-rate chains cannot be constructed, and
callback-only callers no longer provide an ignored field. A future variable-I/O
graph can introduce its own capability without widening the callback chain.
