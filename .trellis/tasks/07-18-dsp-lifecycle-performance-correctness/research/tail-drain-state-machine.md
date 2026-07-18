# Tail Drain and Chain Finish State Machine

## Processor-level production IIR drain

Equalizer, Crossfeed, and DynamicLoudness can emit their existing state by
processing zero input into the caller's output block. The shared adapter helper
should:

1. validate channel geometry;
2. lock the enabled/configuration decision on first finish;
3. fill only the requested caller-owned output prefix with zero;
4. run the existing processor kernel in-place;
5. return `NeedOutput` for an unknown tail until the owning chain/policy stops;
6. remain allocation-, deallocation-, lock-, log-, I/O-, and panic-free.

This adds no sample buffer to the processor. It reuses the finish output block.
The external policy remains responsible for energy threshold, hold duration,
and maximum duration; processor code should not duplicate render policy.

## Why one shared helper is preferred

The validation, lifecycle locking, zero fill, and progress rules are identical
for at least three adapters. A closure can invoke each concrete kernel while
keeping channel validation and terminal semantics centralized. Processor-
specific code remains responsible for declaring `Unknown`, `Finite`, or
`None` based on active configuration.

The helper must not hide different semantics:

* EQ/Crossfeed/DynamicLoudness: asymptotic IIR tail (`Unknown`).
* Oversampled full-band saturation: measured finite delay/FIR support.
* High-pass saturation: potentially unknown because it contains an IIR branch.
* Convolver: exact finite `ir_length - 1` tail, already implemented.
* Limiter: exact algorithmic delay, already implemented.

## Fixed callback-chain finish without scratch

For a fixed 1:1 chain, a chain finish call can use the caller's output block:

1. Keep `finish_stage` as the index of the earliest stage not yet drained.
2. Call that stage's `finish` directly into the caller output.
3. Reborrow the produced prefix and call ordinary in-place `process` on every
   downstream stage.
4. Return produced output immediately.
5. If a stage returns terminal zero, advance through zero-tail stages in the
   same call. The loop is bounded by the fixed stage count.
6. Once every stage is terminal, return stable `Finished(0)` until reset.

This produces the required topology:

```text
finish(stage i) -> process(i+1) -> ... -> process(last)
then finish(stage i+1) -> ...
```

It requires no ping-pong buffer for the current callback graph because every
downstream callback stage is fixed 1:1 and supports in-place processing.

## Unknown-tail detector placement

One reusable detector can belong to the chain/renderer finish driver. For an
unknown stage, inspect the samples after they have passed through downstream
fixed stages so the threshold reflects audible chain output. When the hold is
reached:

* discard the quiet hold from retained offline output;
* advance to the next stage without calling the stopped unknown stage again;
* later drain downstream stages normally;
* leave the stopped stage in finishing state until chain reset.

The detector state is constant-size. Work stops at the first valid hold rather
than rendering the safety maximum.

## Alternative designs

### Per-processor policy and detector

Each IIR processor could own its own threshold/hold/cap. This allows direct
finish to terminate independently, but duplicates policy, measures before
downstream gain, increases per-stage state, and couples algorithms to an
offline rendering decision. Not recommended.

### Precompute a finite analytical IIR bound

Pole radii and state magnitude can produce a conservative decay bound. This can
reduce detector work but is difficult to make tight for cascaded, multichannel,
time-varying filters and configurable downstream gain. A loose bound recreates
fixed-duration over-rendering; an incorrect bound truncates audio. Consider it
only as a future proven fast path, never as the initial correctness mechanism.

### Generic graph scheduler with buffer pool

Required for arbitrary variable-I/O callback graphs, but unnecessary for the
current fixed callback topology. It adds scheduler and buffer-pool complexity
to a path that can finish using caller storage alone.

## Sample-rate boundary

`ConvolverProcessor::set_sample_rate` must not re-arm lifecycle while preserving
old overlap/partition history. Feasible policies are:

1. Reset signal history and retain the discrete IR coefficients (lowest setup
   CPU/memory; the IR is explicitly interpreted in the new sample domain).
2. Publish rate-stamped kernels and reject/bypass a mismatched kernel until the
   control side supplies a matching IR (strong explicit correctness; small
   metadata cost and API change).
3. Retain original IR rate and resample/rebuild the IR on the control thread
   (best preservation of physical impulse duration; highest setup CPU and peak
   memory, and requires an original-rate contract).

At minimum, policy 1 is required to prevent cross-rate signal leakage. Policy 2
is preferable if impulse responses represent physical time rather than
sample-domain kernels. Policy 3 should be a separate control-side feature.

## Static timing metadata

A boolean `introduces_latency` cannot distinguish zero, fixed, and
configuration-dependent timing. Prefer one of:

* remove the boolean and require runtime `latency()` for executable timing; or
* expose a static capability enum such as `Zero`, `Fixed`, and
  `ConfigurationDependent` while keeping runtime values authoritative.

The capability enum is clearer for diagnostics and has no callback cost.

## Direct Convolver geometry

The public boundary should reject zero channels, incomplete IR frames,
incomplete audio frames, and mismatched input/output lengths consistently.
Validation can remain outside the FFT kernel so the adapter's already-validated
hot path does not repeat modulo/length checks inside channel loops.
