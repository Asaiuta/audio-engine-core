# Pipeline and processor-chain boundary audit

## Snapshot and validation

- Final source snapshot for this area: 2026-07-28 15:13 +08:00.
- `src/processor/output_chain.rs` mtime: 2026-07-26 17:01:45 +08:00.
- `src/processor/dsp_chain.rs` mtime: 2026-07-26 15:22:43 +08:00.
- The relevant `src/pipeline.rs` snapshot remained the 2026-07-28 14:59:58
  +08:00 version (101,451 bytes).
- `cargo test --all-features processor::output_chain::tests`: 23 passed.
- `cargo test --all-features processor::dsp_chain::tests`: 17 passed.

Scope:

- `DspChain` construction, processing, finish/reset, timing, and mutation;
- `OutputChainParams` / `OutputChainBuilder` callback and offline roles;
- canonical stage manifest and typed `OutputRenderChain` execution;
- convolver consumer ownership as it crosses builders and type erasure;
- the playback facade's use of `ChainFinishPolicy`.

## Verdict

The chain code is complex primarily because it preserves latency/tail and
unknown-tail termination across stateful processors while remaining bounded on
the callback. The tests support that complexity. The main debt is at the
construction boundaries: one invalid playback policy escapes build-time
validation, callback construction depends on an offline-only field, and the
generic chain admits processor capabilities it cannot actually schedule.

## Confirmed findings

### P1 — invalid drain policy is deferred from build time into the audio callback

**Category**: correctness defect; validation boundary in the wrong lifecycle
phase.

Evidence:

- `ChainFinishPolicy::validate` rejects non-finite/positive thresholds, zero
  hold, and a cap smaller than the hold (`src/processor/dsp_chain.rs:63-79`).
- That validator is private and is called only on the first
  `DspChain::finish_with_policy` (`src/processor/dsp_chain.rs:206-219`).
- `PlaybackConfig::with_drain_policy` stores any `ChainFinishPolicy`
  (`src/pipeline.rs:534-546`).
- `PlaybackConfig::validate` claims to reject invalid configuration before DSP
  state exists but never validates `drain_policy`
  (`src/pipeline.rs:548-632`).
- `PlaybackBuilder::build` therefore succeeds after validating the other
  fields (`src/pipeline.rs:1076-1084`). The invalid policy is first checked when
  callback-side drain reaches `finish_with_policy`
  (`src/pipeline.rs:1403-1415`).
- Current build-rejection tests do not include an invalid drain policy.

Consequence:

A bad preset can build successfully, enter production playback, and fail only
when the audio callback first processes a drain request. This contradicts the
facade's stated strict-build policy and moves a deterministic control/config
error onto the realtime lifecycle path.

Direction:

Expose crate-internal policy validation and include it in
`PlaybackConfig::validate`, while retaining defensive validation in
`finish_with_policy` for direct low-level callers.

### P2 — the callback builder requires and validates an offline-only source rate

**Category**: coupled responsibilities; false required input.

Evidence:

- `OutputChainParams` combines `source_sample_rate` and
  `output_sample_rate` with every callback/offline control handle
  (`src/processor/output_chain.rs:1329-1348`).
- `build_callback_chain` rejects a zero `source_sample_rate` at
  `src/processor/output_chain.rs:1379-1387`.
- Its immediately following comment says callbacks already receive device-
  domain audio and only the offline renderer owns the source/resampler boundary;
  the callback then uses only `output_sample_rate` (`:1388-1402`).
- `callback_chain_uses_the_device_output_rate` deliberately proves that a
  different source rate does not affect callback timing
  (`src/processor/output_chain/tests.rs:86-98`).

Consequence:

An advanced callback-only integration must invent and maintain an irrelevant
source-domain value, and can be rejected for that unused field. Changes to
offline resampling configuration unnecessarily propagate into callback
construction and tests.

Direction:

Separate common callback-stage handles from offline rate-boundary inputs, or
provide distinct callback/render parameter types that share an internal common
bundle.

### P2 — `DspChain` accepts any `StreamingProcessor` but only drives fixed in-place topology

**Category**: abstraction/capability mismatch.

Evidence:

- `DspChain::add` accepts every `P: StreamingProcessor`
  (`src/processor/dsp_chain.rs:150-153`).
- `DspChain::process` unconditionally drives each stage with the same in-place
  block (`src/processor/dsp_chain.rs:168-188`).
- The public `StreamingProcessor` contract includes both in-place and
  out-of-place buffer modes (`src/processor/traits.rs:161-210`).
- `StreamingResampler` implements that trait but rejects unequal-rate in-place
  processing with `UnsupportedBufferMode`
  (`src/processor/resampler/mod.rs:1133-1148`).
- The offline output chain keeps the resampler outside `DspChain`, which is the
  correct scheduling model, but the public generic chain API does not encode
  that restriction.

Consequence:

Code can construct a type-correct `DspChain` containing a variable-rate
processor and discover only during audio processing that the chain cannot
drive it. The trait bound communicates broader compatibility than the chain
actually has, and timing composition also assumes one fixed chain rate.

Direction:

Express a fixed 1:1/in-place capability in the type or in a validated add path,
or narrow/document the accepted processor class at the API boundary. Do not
teach the callback chain to allocate variable-rate scratch merely to preserve
an overly broad bound.

### P2 — the chain sample-rate invariant is constructible but not enforced

**Category**: weak invariant; inconsistent validation and naming.

Evidence:

- `DspChain::new` and `with_capacity` accept any `u32`, including zero, return
  `Self`, and store the value; both parameters are oddly named
  `_sample_rate_hz` even though they are used
  (`src/processor/dsp_chain.rs:114-130`).
- `set_sample_rate(0)` is explicitly rejected before processor mutation
  (`src/processor/dsp_chain.rs:493-510`).
- `add` neither configures a processor to the chain rate nor validates its
  existing rate (`:150-153`).
- `latency` silently drops timing-conversion errors and returns zero for an
  invalid/zero chain rate (`:524-542`); `tail` falls back to `Unknown`.
- Tests cover `set_sample_rate(0)` but not construction with zero or a
  processor whose configured rate disagrees with the chain metadata.

Consequence:

The same nominal invariant is strict on mutation but optional at construction,
and the constructor's rate is metadata rather than an enforced processor
configuration. Timing can degrade to a plausible zero/unknown result instead
of exposing the invalid chain.

Direction:

Use a validated rate type/result constructor, or explicitly model an
unconfigured chain that must be configured before timing/processing. Rename
used parameters normally; an underscore prefix incorrectly suggests they are
intentionally ignored.

### P3 — cloneable builders advertise reuse while one hidden member is single-consumer

**Category**: ownership ergonomics; runtime-only constraint.

Evidence:

- Both `OutputChainParams` and `OutputChainBuilder` implement `Clone`, and
  build methods borrow `&self` (`src/processor/output_chain.rs:1329-1417`).
- Cloning retains the same `ConvolverControl`; only one simultaneously live
  direct/callback/render consumer can acquire its CAS lease
  (`src/processor/adapters/convolver/control.rs:85-98` and `:303-312`).
- The builder documentation warns about the constraint and tests verify the
  typed `ConsumerAlreadyActive` error.

Consequence:

The ordinary meaning of cloning/reusing a builder is weaker for this one
resource than for every other shared parameter. A caller must know to replace a
public field with a distinct control to build independent live chains.

Direction:

Prefer consuming build methods or an explicit `fork_with_new_convolver`
operation if independent chains are a supported use case. If sequential reuse
is the only goal, document that directly and reconsider `Clone` on the builder.

The underlying convolver lease is not the problem: it prevents heavy-kernel
ownership and destruction from becoming ambiguous on the audio thread.

### P3 — the lease-release test does not acquire the lease before failing

**Category**: misleading test name / evidence gap.

Evidence:

- `callback_build_failure_releases_convolver_consumer_lease` sets
  `source_sample_rate = 0`, observes build failure, then confirms a consumer can
  be created (`src/processor/output_chain/tests.rs:72-84`).
- `build_callback_chain` returns for that condition at
  `src/processor/output_chain.rs:1382`, before the manifest constructs the
  `ConvolverProcessor` at `:1025-1027`.

Consequence:

The test proves that an early pre-acquisition failure does not acquire the
lease; it does not prove that a failure after acquisition releases it. RAII and
the chain drop path appear correct, but this test's name overstates its
evidence.

Direction:

Trigger a deterministic failure after the convolver stage has been added (for
example a later sample-rate setup failure), or rename the test to the behavior
it actually covers.

### P3 — canonical-order machinery still has multi-site stage-registration fan-out

**Category**: maintenance hotspot, not current ordering defect.

Evidence:

- `output_stage_manifest!` centralizes the intended semantic order
  (`src/processor/output_chain.rs:723-833`).
- Callback construction still needs per-stage arms in `add_callback_stage!`
  (`:1002-1055`).
- Offline construction separately declares concrete fields and constructors
  (`:1450-1536`), while additional macros separately handle processing, reset,
  source-rate changes, finish, and timing.
- `OutputStageId` even declares `PeakLimiter` before `Resampler`
  (`:900-913`) although the canonical manifest and execution order place
  `Resampler` first (`:793-810`). This has no runtime effect but weakens the
  visual signal of what is canonical.

Consequence:

Adding or moving a stage still requires coordinated edits across the manifest,
callback constructor arms, offline fields/constructor, and role-specific macro
arms. Tests catch order divergence, but compiler errors and review effort remain
spread across many sites.

Direction:

Keep the manifest because it provides real parity value, but document the
complete stage-addition checklist and reduce residual duplicated declarations
where Rust's heterogeneous stage types permit it.

### P3 — finish-state reset bookkeeping is repeated in several transitions

**Category**: local duplication / drift risk.

`DspChain` clears the same finish fields in `reset` (`src/processor/dsp_chain.rs:470-490`),
`set_sample_rate` (`:493-521`), and `clear` (`:598-610`), with related subsets
also repeated when advancing finish stages (`:271-278`, `:345-350`, and
`:370-375`). The values currently agree, and tests are strong, but a future
field addition must be remembered at every reset/advance site. A small internal
state object or helper would make the lifecycle invariant easier to preserve.

## Important non-findings / justified complexity

### The output-stage manifest has real value

The macro machinery is not ornamental metaprogramming. It derives stage
descriptors, callback count, callback construction order, and multiple offline
stage traversals from one ordered list. Tests assert callback order, offline
order, callback/offline shared-order parity, irregular-chunk equivalence, and
sample parity before quantization. The residual fan-out above is a maintenance
cost, but deleting the manifest would remove a useful consistency mechanism.

### The finish state machine is required by the streaming contract

The chain must drive each upstream tail through all downstream processors,
observe energy before terminal noise shaping, protect downstream finite delay,
honor output backpressure, and bound unknown/infinite tails. The focused tests
cover each of these interactions and allocation-free steady-state processing.
Its complexity is contract-driven; only duplicated transition bookkeeping is a
cleanup candidate.

### Runtime convolver leasing is protecting realtime ownership

The lease makes one audio consumer authoritative while allowing cloneable
control-side publishers. Drop ordering releases heavy ownership before the
lease and tests cover direct/callback/render conflicts. Replacing it with an
unrestricted `Arc` would risk destruction on the audio thread, so the lease
should not be removed merely to make builders simpler.

### Best-effort reset/sample-rate updates are deliberate

`DspChain::reset` and `set_sample_rate` attempt every stage and return the first
error. Trellis streaming-lifecycle spec lines 148-150 explicitly require this
policy so one failing stage does not prevent cleanup/reconfiguration attempts
on later stages. The lack of rollback is a consequence to document, not an
accidental loop structure.

## Follow-up question, not yet a confirmed defect

`ChainFinishPolicy::default` uses 12,000 hold frames and 1,440,000 maximum tail
frames (`src/processor/dsp_chain.rs:88-91`). These equal the offline defaults of
250 ms and 30 seconds only at 48 kHz. At other rates the wall-clock behavior
changes. The type explicitly uses frame units, so this may be an intentional
fixed-work policy; the repository does not currently state that decision or
test cross-rate default behavior. The final report should keep this as a design
question unless a product-time contract is found elsewhere.

## Test gaps exposed by this review

- invalid `ChainFinishPolicy` rejected by `PlaybackBuilder::build`;
- a callback build failure after convolver lease acquisition;
- `DspChain::new(0)` / `with_capacity(_, 0)` behavior;
- adding an unequal-rate `StreamingResampler` to `DspChain` and receiving an
  early construction error rather than a process-time surprise;
- explicit cross-rate semantics for the default callback drain policy.

