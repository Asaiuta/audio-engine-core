# Canonical Output Stage Orchestration Options

## Current duplication and constraints

`OUTPUT_STAGE_DESCRIPTORS` currently describes an intended order, but it is not the
execution source. The callback builder manually appends processors to `DspChain`, while
`OutputRenderChain` manually repeats its typed-field order in construction,
`process_pre_quantize`, full `render`, `reset_for_render`, and source sample-rate updates.
Tests compare some snapshots, but a metadata edit cannot change runtime traversal and a
runtime edit can silently leave metadata stale.

The stages are not fully homogeneous:

* Volume through PeakLimiter run at the source rate and are shared by callback/offline.
* Resampler is optional, changes frame count/rate, and is offline-only.
* NoiseShaper runs after the rate boundary and is shared by callback/offline under different
  rate assumptions.
* Quantize is an offline terminal transform, not a `StreamingProcessor` field.
* Meter is now a separate post-render analysis and must not appear in render traversal.

The callback already uses `Vec<Box<dyn StreamingProcessor>>`; offline keeps typed fields so
it can access Convolver control/reclamation and limiter telemetry directly. Any refactor must
preserve callback no-allocation behavior after construction and must not add per-block
allocation or dispatch to the offline path merely to unify syntax.

## Option 1 - Declarative stage manifest with generated traversals (recommended)

Define one private macro manifest containing ordered stage identity, rate domain, callback /
offline membership, and processor-field binding. Expand that manifest into:

* static render descriptors and callback/offline name views;
* callback `DspChain` construction order;
* offline fixed-stage process and full render traversal;
* reset and source-rate update traversal;
* parity tests over the generated identifiers.

Keep resampler as an explicit optional rate-boundary entry and quantize as an explicit
terminal entry in the same manifest vocabulary. Keep `OutputRenderChain`'s concrete typed
fields; operation-specific helper macros expand direct field calls in manifest order. The
callback retains its existing trait-object vector, while offline retains static dispatch.

Advantages:

* Runtime order and public metadata have one executable source.
* No new per-block allocation, virtual dispatch, downcast, or shared telemetry handle.
* Limiter and Convolver type-specific operations remain direct and auditable.
* Adding/removing/reordering a stage changes every traversal through one manifest edit.

Costs:

* Macro expansion is less obvious than plain handwritten calls.
* Optional/rate-changing/terminal stages need explicit categories rather than one uniform
  loop.
* Compiler errors from a malformed manifest can be more verbose, so the macro should stay
  local and small.

## Option 2 - Uniform trait-object stage container

Store offline processors in an ordered `Vec<Box<dyn StreamingProcessor>>` as well and drive
that vector as the execution source. Attach descriptors to entries. Resampler still needs
out-of-place/rate-transition handling, and Quantize remains a terminal operation unless the
trait contract is widened.

Advantages:

* The ordered container is directly the runtime execution order.
* Dynamic composition and future optional stages are straightforward.

Costs:

* Offline loses concrete type access; limiter result and Convolver reclamation require
  downcasting, external telemetry/control handles, or a wider stage interface.
* Adds heap allocation and virtual dispatch to an offline path that currently has typed
  fields, without improving audio quality.
* Does not fully unify resampler/quantize unless `StreamingProcessor` is expanded, increasing
  the blast radius beyond this task.

## Option 3 - Keep handwritten order and strengthen tests

Leave all production traversals handwritten. Add snapshot/parity tests and, where possible,
test-only tracing to compare callback/offline stage names against descriptors.

Advantages:

* Smallest production-code change and easiest local debugging.
* No macro or trait restructuring.

Costs:

* Metadata is still not the execution source.
* Every future stage edit must touch several lists; tests only detect the drift cases they
  instrument.
* A transparent or disabled stage can be omitted without signal-parity tests noticing, so
  this mitigates rather than fixes the structural finding.

## Recommendation

Choose Option 1. It addresses the actual drift mechanism while preserving the current
performance model: existing callback dynamic dispatch is unchanged, offline remains typed,
and every hot traversal expands to direct calls with no extra allocation. Keep the manifest
private to `output_chain` (or its extracted stage-plan module) and encode source-rate,
rate-boundary, output-rate, terminal-transform, and post-analysis as distinct roles instead
of pretending every node is interchangeable.
