# DSP Lifecycle Timing and Tail Correctness

## Goal

Correct the remaining DSP timing, end-of-stream, tail, and sample-rate-domain
defects without weakening realtime guarantees. The selected design should
prioritize correct audio first, then minimize callback CPU, worst-case CPU,
steady-state memory, and offline temporary memory using measured evidence.

## What I Already Know

* Oversampled saturation uses symmetric 17-tap (2x) and 33-tap (4x) FIRs. Both
  introduce about four source frames of group delay, but the wet path is mixed
  with the current undelayed dry sample.
* `SaturationProcessor` reports default zero latency/tail and returns
  `Finished(0)`, so offline rendering neither compensates its delay nor drains
  its FIR state.
* Equalizer, Crossfeed, and DynamicLoudness retain IIR state but use the shared
  fixed finish path, return no samples, and inherit `TailSpec::None`.
* The existing unknown-tail RMS/hold/safety-cap driver is exercised by a test
  double, not by a production IIR processor.
* Callback `DspChain` composes `process`, `reset`, and sample-rate updates but
  does not expose chain-level finish, latency, or tail composition.
* Convolver sample-rate updates retag timing and re-arm lifecycle state without
  clearing the kernel's old-rate signal history.
* Public stage latency descriptors disagree with executable timing for
  Saturation, Convolver, and Resampler.
* Direct `FFTConvolver` APIs accept incomplete interleaved geometry and handle
  the trailing remainder differently between out-of-place and in-place paths.
* Current verification is green: 293 library tests pass; the quick enforced
  quality run passes 25/25 gates with no skipped EBU corpus checks.
* The full output chain still has a documented report-only final true-peak
  limitation after resampling/quantization (observed worst case -0.610 dBTP for
  a -1.0 dBTP limiter target).

## Constraints and Assumptions

* Audio correctness and deterministic lifecycle semantics are mandatory; a
  faster design that changes the intended transfer function is unacceptable.
* Realtime `process` and callback-facing `finish` must perform no allocation,
  deallocation, locking, logging, I/O, panic, or unbounded work.
* Setup/control paths may allocate and precompute buffers or coefficients.
* Offline work should stop at the configured energy/hold criterion rather than
  always rendering the full safety maximum.
* Existing public APIs may receive breaking corrections because the crate is
  still version 0.1.0, but unnecessary surface expansion should be avoided.

## Requirements

* Provide at least three technically feasible designs in Brainstorm, including
  explicit CPU, memory, latency, audio-correctness, and implementation-risk
  trade-offs.
* Select designs using measured callback median/p95/deadline utilization and
  allocated-byte evidence, not qualitative claims alone.
* On a compatible same-environment baseline, the 512-frame active callback
  chain may regress by at most 3% in median time and 5% in relative p95 deadline
  utilization. Prefer net improvement rather than treating those limits as a
  performance allowance.
* The isolated active Saturation 4x path must show a measured median CPU
  improvement from evaluate-once. Other compatible callback/offline cases that
  do not improve must remain inside budget and retain evidence explaining the
  limiting work before any optimization claim is made.
* Preserve source-rate and output-rate timing domains without per-stage
  rounding drift.
* Preserve audible finite and unknown effect tails through downstream limiter,
  resampling, noise shaping, and terminal quantization.
* Run exactly one logical PeakLimiter in the final floating-point rate domain:
  at device rate for callbacks and after optional SoXR for offline render, then
  NoiseShaper and terminal quantization.
* Enforce the user-facing final `-1.0 dBTP` ceiling after quantization using a
  deterministic internal guard derived from bounded noise-shaping, target bit
  depth, true-peak reconstruction, and f32 rounding error.
* Production IIR adapters share one finish lifecycle driver and may supply a
  processor-specific silence kernel only when it matches the ordinary
  zero-input oracle and improves a compatible benchmark.
* Keep production callback paths allocation-free with bounded per-block work.
* Reject malformed public interleaved geometry consistently.
* Every published Convolver kernel carries a non-zero sample-rate domain and is
  adopted only by a processor operating at that same rate.
* A Convolver sample-rate change succeeds even when no matching kernel is
  available. It immediately retires the old-rate signal state, enters an
  explicit dry-bypass waiting state, and exposes the awaited rate through
  telemetry until a matching kernel is adopted at a block boundary.
* Same-rate Convolver enable/disable and adoption from a dry waiting state use
  a complementary smoothstep of `ceil(0.005 * active_sample_rate_hz)` frames.
  The transition executes at most one convolution kernel and fuses dry/wet
  mixing with existing bounded processing scratch; general live dual-kernel
  replacement is excluded.
* Oversampled Saturation low-pass filters only the nonlinear residual and adds
  it to the four-frame delayed dry signal. FIR history advances at every
  oversampled phase but evaluates one dot product per source output.
* Saturation has separate setup/reset-time hard bypass and runtime effect-enable
  state. Hard bypass is zero-latency and performs no per-sample state work;
  armed-but-effect-disabled processing remains on the four-frame timeline and
  executes only the delay/history work needed for seamless reactivation.
* Saturation quality changes must start at an explicit source-frame offset and
  transition without a timeline jump, unbounded work, or callback allocation.
* Quality transitions use 32 source frames and complementary smoothstep weights
  whose sum remains one at every transition sample.
* Sample-accurate Saturation automation uses a caller-borrowed, sorted, sparse
  event slice whose offsets are relative to the current source block. Events
  carry quality or effect-enable changes; existing atomic parameters remain the
  block-start fallback when no event is supplied.
* A quality event during an active transition retargets from the current
  three-mode weight vector at that exact frame. Direct/2x/4x state slots are
  preallocated; only non-zero-weight paths execute. Same-offset events coalesce
  to the last value in slice order.

## Research References

* [`research/architecture-options.md`](research/architecture-options.md) -
  compares targeted, whole-vector in-place, and typed block-streaming render
  architectures with CPU, memory, and risk models.
* [`research/saturation-timing-and-cpu.md`](research/saturation-timing-and-cpu.md)
  - derives oversampling delay, identifies redundant FIR evaluation, and
  compares dry/wet alignment algorithms.
* [`research/tail-drain-state-machine.md`](research/tail-drain-state-machine.md)
  - defines allocation-free IIR drain, fixed-chain EOS propagation, and
  sample-rate boundary choices.
* [`research/measurement-plan.md`](research/measurement-plan.md) - defines
  compatible callback/offline baselines, memory accounting, and independent
  audio oracles.
* [`research/final-true-peak-options.md`](research/final-true-peak-options.md) -
  compares report-only, dual-limiter, and single relocated output-domain
  limiter designs, including quantization guard requirements.
* [`research/convolver-activation-transition.md`](research/convolver-activation-transition.md)
  - separates bounded dry/wet activation smoothing from expensive general
  dual-kernel replacement.

## Feasible Architecture Approaches

### Approach A: Targeted lifecycle patch

* Add correct production tails/latency, callback finish, rate reset, metadata,
  and geometry validation without changing the stage-at-a-time renderer.
* Normal callback overhead stays minimal.
* Offline processing keeps a full copy per fixed stage and about two full
  engine-owned program vectors during each stage, in addition to caller input.
* Lowest implementation risk, but it does not satisfy the best-offline-memory
  or best-memory-bandwidth objective.

### Approach B: One owned vector with in-place fixed stages

* Clone input once, process fixed stages in-place, append each stage's tail, and
  allocate another full vector only at a variable-I/O boundary such as SoXR.
* Removes the mandatory full-buffer copy performed by every fixed stage.
* Usually keeps one engine-owned full vector; source/destination coexist around
  resampling.
* Every stage still traverses the whole program separately, so long streams do
  not remain cache-hot.
* Moderate implementation risk and a strong cost/benefit compromise.

### Approach C: Typed block-streaming output pipeline (Recommended)

* Stream bounded blocks through all typed source stages in-place, optional
  SoXR, output stages, and terminal quantization before appending final output.
* Propagate EOS with a bounded stage-index state machine. Callback fixed-chain
  finish reuses caller output and needs no full scratch buffer.
* Temporary render memory beyond the unavoidable final output is bounded by a
  small block pool: about 16 KiB for two 512-frame stereo `f64` blocks or
  128 KiB for two 4096-frame stereo blocks.
* Eliminates per-stage full-program copies and keeps adjacent stage data in
  cache. It has the best expected long-render CPU and memory behavior.
* Highest implementation risk; must be delivered behind old/new parity,
  irregular-chunk, tail, rate-boundary, and allocation tests.

### Architecture Comparison

| Property | A: targeted | B: in-place vector | C: block streaming |
| --- | --- | --- | --- |
| Audio correctness after fixes | Full | Full | Full |
| Normal callback overhead | Minimal | Minimal | Minimal |
| Full-buffer copies | Per fixed stage | Initial/rate boundary | None per stage |
| Temporary memory vs duration | About 2x stream | About 1x; 2x at rate boundary | Bounded blocks |
| Long-render cache locality | Low | Medium | High |
| Implementation risk | Low | Medium | High |

The fully generic trait-object graph scheduler was considered and rejected for
the current scope: it would add capability negotiation, buffer-pool ownership,
and scheduler overhead to a fixed callback topology without a demonstrated
runtime graph requirement.

## Decision (ADR-lite): Render and EOS Architecture

**Context**: The current stage-at-a-time offline renderer is correct for the
finite tails it knows about, but allocates/copies a complete program buffer per
stage. The callback graph is fixed 1:1 and does not need a general variable-I/O
scheduler on its ordinary hot path.

**Decision**: Adopt Approach C, the typed block-streaming output pipeline.
Offline rendering will use bounded preallocated blocks, static typed stage
dispatch, and the canonical manifest order. Callback ordinary processing stays
fixed 1:1; callback EOS uses a bounded stage-index state machine and reuses the
caller output block to propagate each upstream finish result through downstream
stages. Do not introduce a fully generic runtime graph scheduler.

**Consequences**: Temporary offline memory becomes independent of input
duration apart from the unavoidable final result vector, and adjacent stages
can operate on cache-hot blocks. Implementation risk is higher, so migration
must be incremental and retain an old/new direct oracle until block, tail,
resampler, raw/compensated, and quality parity are proven. Normal callback
processing must not gain scheduler or buffer-pool overhead.

## Decision (ADR-lite): Saturation Nonlinear-Delta Filtering

**Context**: Filtering the complete wet signal colors even a below-threshold
linear input and creates magnitude/phase differences against an unfiltered dry
path. Filtering both dry and wet paths would align them but doubles FIR work and
still colors the nominal dry signal. The current 4x loop also evaluates its
33-tap FIR at all four oversampled phases although only the final decimated
value is used.

**Decision**: Filter only the nonlinear residual. At each oversampled phase,
form `delta_os = shaped(input_os) - input_os`, advance the existing FIR history,
and evaluate it once at the source-output phase. Produce
`delayed_dry + mix * lowpass(delta_os)`. For high-pass exciter mode, derive the
nonlinear residual from the high-pass branch and add it to the delayed full-band
input. Split FIR history update from evaluation so 4x performs at most one
33-tap dot product per source sample rather than four.

**Consequences**: Below threshold the residual is exactly zero, so the enabled
path is an exact four-frame delayed identity and partial mix cannot comb-filter
the linear program component. Persistent storage remains one FIR history plus a
four-frame dry-delay ring. Nominal 4x FIR work falls from 132 to 33 MACs per
source sample, with a bounded inactive-residual fast path permitted only if it
preserves pending tail and chunk determinism. This intentionally changes the
oversampled transfer, so fundamental gain, harmonic spectrum, alias rejection,
exciter response, and listening fixtures require independent validation.

## Decision (ADR-lite): Saturation Hard Bypass and Effect Enable

**Context**: An enabled Saturation stage has a fixed four-source-frame timeline,
while the existing disabled path returns the current input at zero latency.
Changing directly between those timelines during a stream necessarily skips or
repeats program samples. Keeping every disabled instance delayed would permit
automation but would remove the useful zero-latency, near-zero-work hard bypass.

**Decision**: Split activation into two explicit states. Hard bypass is selected
only during setup or reset and has zero latency, no tail, and no per-sample DSP
state updates. An armed stage always reports and maintains four frames of
latency. Its runtime effect-enable control uses a 32-source-frame complementary
smoothstep between delayed raw input and the fully processed result, so disabling
also removes non-unit input/output gain effects. The existing borrowed sparse
Saturation event slice carries sample-offset effect-enable events as well as
quality events; the atomic snapshot remains the block-start fallback.

**Consequences**: Callers that need runtime on/off automation must arm the stage
before the stream and accept four frames of latency. When fully soft-disabled,
the callback performs only bounded delay/source-history maintenance and skips
the waveshaper and FIR dot product. Hard-bypass changes cannot be automated and
take effect only with lifecycle reset, which prevents timeline discontinuity.
The control API gains an explicit activation distinction, but latency metadata,
finish behavior, CPU cost, and telemetry are no longer ambiguous.

## Decision (ADR-lite): Saturation Quality Automation

**Context**: Direct saturation is zero-latency while the corrected 2x/4x paths
have about four source frames of delay. A host cannot reliably change graph
delay compensation sample by sample, and simply resetting FIR history at a
quality update creates a transition artifact.

**Decision**: Support sample-accurate runtime quality automation with a bounded
dual-path transition. While the streaming Saturation stage is enabled, report
and maintain the maximum four-frame latency so old and new quality paths share
one timeline. Start the transition at the requested source-frame offset and
use complementary weights that sum to one; do not use an uncorrelated-signal
equal-power law that would boost two nearly identical paths. A hard-bypassed
stage remains zero-latency; an armed but effect-disabled stage retains the
four-frame timeline without executing inactive quality paths.

**Consequences**: The streaming Direct path gains four frames of latency and
must drain them at EOS. During a transition both quality states execute, so CPU
temporarily rises and both states must be retained until the fade completes.
The transition and any pending FIR response must remain chunk-independent and
finish correctly near EOS. True arbitrary-sample automation cannot be carried
by the current once-per-callback atomic snapshot alone and therefore uses the
separate borrowed event contract below.

## Decision (ADR-lite): Saturation Automation Event Transport

**Context**: The existing atomic snapshot is read once per callback and cannot
identify an arbitrary frame offset. A queue can add timing/overflow semantics,
while a dense per-frame lane pays work and memory even when quality events are
rare.

**Decision**: Add a caller-borrowed sparse event slice for sample-accurate
Saturation automation. Events carry a source-frame offset relative to the
current block and either a quality or effect-enable value, are sorted by offset,
and are valid only for that process call. Same-offset events apply in slice
order and the last event for each control wins. The event-aware processing entry
routes events without heap ownership or atomic queue traffic. The existing
process entry remains the empty-event fast path and applies the ordinary atomic
snapshot at block start.

**Consequences**: Hosts that need arbitrary-sample automation must schedule
block-relative events. Invalid offsets/order return a typed error before DSP
mutation. No-event callback cost must remain equivalent within the agreed
baseline tolerance. Target quality state is prepared from a short preallocated
source-history ring at the event sample, avoiding continuous execution of all
quality modes; exact history length will be derived from measured effective
interpolation/FIR support.

## Decision (ADR-lite): Saturation Quality Transition Window

**Context**: Target quality state is warmed from recent source history, so the
transition window only needs to smooth the spectral/transfer change rather than
hide a cold FIR startup. A longer window raises overlap probability and dual-
path work; a very short window can expose a switching transient.

**Decision**: Start a 32-source-frame complementary smoothstep transition at
the exact event frame. Weights must sum to one so two highly correlated quality
paths do not receive the +3 dB gain of an uncorrelated equal-power crossfade.

**Consequences**: At 48 kHz the transition lasts about 0.67 ms (0.73 ms at
44.1 kHz). Both paths execute for at most 32 frames per non-overlapped event.
Tests must verify exact event start, continuous value/slope, chunk independence,
and EOS completion when an event occurs in the last 32 input frames.

## Decision (ADR-lite): Overlapping Saturation Quality Events

**Context**: Deferring an event breaks sample accuracy, while dropping a path
that still has non-zero weight can create a discontinuity. There are only three
quality modes, so the maximum number of concurrent algorithm states is bounded.

**Decision**: Represent transition state as complementary weights over
Direct/2x/4x. A new event begins at its exact frame and interpolates from the
current weight vector to the target mode's one-hot vector over 32 frames. Keep
all three state slots preallocated and execute only paths whose current or next
weight can contribute. Coalesce multiple events at one frame to the last event
in slice order.

**Consequences**: Path CPU can temporarily reach three active Saturation modes
under dense automation, but remains bounded to the fixed mode count and window.
No event allocates or constructs state. Source-history replay prepares a newly
activated slot before it contributes. Tests must cover repeated retargeting on
consecutive frames, all same-offset mode permutations, weight-sum invariants,
and random block boundaries.

## Decision (ADR-lite): Production IIR Tail Generation

**Context**: EQ, Crossfeed, and DynamicLoudness need the same channel
validation, finish locking, progress, and no-allocation behavior, but their
zero-input recurrences may have different optimization opportunities.

**Decision**: Use one shared fixed-1:1 finish lifecycle driver. It validates
geometry, locks the first-finish configuration, writes into caller-owned
output, and reports the common unknown-tail progress contract. Each processor
provides a statically dispatched silence closure: initially the ordinary DSP
kernel over zero input, optionally a specialized silence recurrence after
oracle parity and benchmark evidence. Keep threshold/hold/safety-cap policy in
the chain/renderer driver rather than duplicating it inside processors.

**Consequences**: Lifecycle semantics cannot drift between production IIR
adapters, no tail buffer is added, and optimized recurrence remains possible
without a per-sample virtual call. Direct callers of an unknown-tail processor
must use a policy-owning driver or reset after stopping; the processor itself
does not embed an arbitrary render threshold. Specialized kernels are rejected
unless random-state/output parity and no-allocation tests pass.

## Decision (ADR-lite): Convolver Kernel Sample-Rate Domain

**Context**: Retagging the processor while retaining overlap/partition history
leaks old-rate signal state. Retaining the same tap count at a new rate also
changes a physical IR's duration and frequency response without saying so.
Automatic high-quality IR resampling would require retained source samples and
potentially large control-side temporary memory.

**Decision**: Make Convolver publications rate-stamped. Publication includes a
non-zero `sample_rate_hz`; a processor only adopts a kernel whose rate equals
its active stream rate. A valid rate change clears old kernel signal history
and finish state. Do not automatically resample/rebuild IRs in this task.

**Consequences**: Realtime matching is an integer metadata check and does not
add sample-loop work. Callers/control code must build or obtain the correct IR
before changing streams. A future control-side resampling helper can be added
without changing the core ownership contract. Missing-matching-kernel behavior
is resolved separately below.

## Decision (ADR-lite): Missing Convolver Kernel at a New Sample Rate

**Context**: Requiring callers to publish a matching kernel before every stream
rate change makes setup transactional, but can prevent a valid device/rate
switch for a resource that is intentionally prepared asynchronously. Continuing
with the old-rate kernel would silently apply the wrong time/frequency mapping.

**Decision**: Accept the new stream rate immediately. If no matching
rate-stamped kernel is ready, detach the old-rate kernel and clear its signal and
finish history, then process the Convolver as an explicit dry bypass. Telemetry
must distinguish this waiting state and report the required `sample_rate_hz`.
Adopt a subsequently published matching kernel only at a block boundary; never
fall back to an old-rate kernel.

**Consequences**: Device/rate changes remain non-blocking and audio continues
without applying an invalid IR, but the convolution effect can disappear for a
bounded or unbounded preparation interval and reappear when the new kernel is
published. The realtime path gains only bounded state/rate checks and must not
allocate, lock, log, or destroy kernel storage. Tests must cover rate-change and
publication races, dry output while waiting, truthful telemetry, and clean
first-block adoption with no old-rate history leakage.

## Decision (ADR-lite): Convolver Dry/Kernel Activation Transition

**Context**: Block-boundary ownership adoption is realtime-safe but an abrupt
change between dry input and a non-trivial IR response can click. The current
overlap-save and partitioned engines both provide a zero-algorithmic-latency
head path, so dry and wet signals share one input timeline. Running both old and
new kernels would improve arbitrary IR replacement but temporarily doubles the
most expensive stage.

**Decision**: For same-rate enable/disable and adoption from an explicit dry
waiting state, apply a complementary smoothstep whose length is exactly
`ceil(0.005 * active_sample_rate_hz)` frames. Run at most one kernel: fuse dry/wet mixing into
the bounded convolution chunk path and reuse existing scratch. Once a new stream
rate is active, retire the old-rate kernel immediately; never continue it for a
fade. A later matching new-rate kernel fades in from dry. General live
old-kernel/new-kernel replacement crossfade is out of scope.

**Consequences**: Normal steady-state convolution CPU and ownership memory do
not increase; only a bounded multiply/add envelope runs during activation.
Same-rate transitions and new-kernel arrival avoid discontinuous output. An
unannounced sample-rate boundary can still remove the old effect immediately,
because wrong-rate processing is forbidden; callers that need a fade-out must
coordinate it before changing rates. Transition progress, telemetry, finish,
and random chunking must remain deterministic.

## Decision (ADR-lite): Final Output-Domain True-Peak Limiting

**Context**: The current source-rate limiter precedes SoXR, NoiseShaper, and
terminal f32 quantization. Those downstream transforms can create new sample and
intersample peaks; the current quick full-output probe has measured
`-0.610 dBTP` for a `-1.0 dBTP` target. Adding a second limiter would close the
ceiling but duplicate detector CPU, lookahead latency, state, and release gain
envelopes.

**Decision**: Keep exactly one logical PeakLimiter and execute it in the final
floating-point rate domain. Callback chains run it at the actual device rate.
Offline chains run source-domain DSP, optional SoXR, the output-rate
PeakLimiter, NoiseShaper, and terminal f32 quantization in that order. Derive a
small internal ceiling guard from the selected NoiseShaper bit depth and bounded
feedback/error state, TPDF range, terminal f32 rounding, and true-peak FIR
coefficient bound. Promote the final quantized `-1.0 dBTP` probe from report-only
to an enforced quality gate.

**Consequences**: Equal-rate callback/offline topology remains equivalent and
still pays one limiter. Unequal-rate renders intentionally change limiting
behavior because gain control now observes resampling peaks. Limiter CPU and
ring storage scale with output rather than source rate during resampling, but
there is no second lookahead, detector, or release envelope. Finish propagation,
tail trimming, raw/compensated timing, descriptors, and telemetry must account
for the limiter in the output-rate domain. SoXR may receive samples above the
nominal ceiling, which is valid for its floating-point processing path.

## Decision (ADR-lite): Performance Regression Budget

**Context**: The task adds correctness work on some paths while removing
redundant FIR evaluation and full-program stage copies on others. The project
default 10% median allowance is too loose for a performance-focused DSP change,
while fixed improvement percentages across different CPUs and compilers can
reward benchmark noise or block required correctness work.

**Decision**: Use the strict practical budget. Against a compatible
same-environment baseline, the 512-frame active callback-chain median may
regress by no more than 3%, and relative p95 deadline utilization by no more
than 5%. Realtime process and callback finish remain strictly allocation-,
deallocation-, lock-, and unbounded-work-free. The isolated active Saturation
4x median must improve. Seek net improvement for the callback chain and long
offline renders, but do not require an arbitrary cross-platform percentage.
Normalize changed rate-domain work per output frame and also report total
render realtime factor.

**Consequences**: A within-budget result without net improvement is acceptable
only with raw compatible evidence and an explanation of the remaining dominant
work; it cannot be described as a speedup. Benchmark tuning may continue after
correctness is green, but must not replace the selected transfer function or
weaken lifecycle behavior. The project-wide 10% gate remains a fallback for
unrelated cases, not the acceptance threshold for this task's primary case.

## Acceptance Criteria

* [ ] Oversampled Saturation below threshold is bit-exact to the independently
      four-frame-delayed input for all wet mixes and irregular chunkings.
* [ ] Saturation partial dry/wet mix, high-pass exciter response, harmonic
      spectrum, and alias rejection meet independent numerical and listening
      oracles after nonlinear-delta filtering.
* [ ] The optimized 4x path advances FIR history at every phase but evaluates
      no more than one 33-tap dot product per source frame, and benchmarked
      callback CPU does not regress against the compatible baseline.
* [ ] Saturation latency metadata and measured impulse position agree for every
      quality, activation, and effect-enable combination.
* [ ] Hard-bypassed Saturation is bit-exact, zero-latency, tail-free, and does
      no per-sample state work; changing hard-bypass state requires setup/reset.
* [ ] Armed soft-disable/enable events start at the requested frame, preserve a
      continuous four-frame timeline, fade the complete processed difference
      over 32 frames, and skip waveshaper/FIR work after soft-disable settles.
* [ ] A quality event at every tested frame offset, including the first and
      last frame of irregular callback blocks, starts at exactly that source
      frame.
* [ ] Empty, unsorted, duplicate-offset, and out-of-range event slices have
      explicit deterministic behavior and never allocate on the callback.
* [ ] Dense overlapping automation never executes more than three quality
      paths, keeps weights finite and summing to one, and remains equivalent
      across block partitions.
* [ ] Saturation finish drains its finite FIR response without allocation.
* [ ] Production EQ, Crossfeed, and DynamicLoudness tails reach the configured
      RMS/hold stop and remain block-size independent.
* [ ] Every specialized IIR silence kernel matches ordinary zero-input
      processing for randomized valid states and chunk partitions within the
      algorithm's defined numerical tolerance.
* [ ] Callback end-of-stream preserves limiter delay and convolution/effect
      tails through downstream stages, or the public contract explicitly
      delegates EOS to a typed owner with equivalent tests.
* [ ] `DspChain` exposes composed latency/tail and bounded stage-index finish;
      callback finish reuses caller output and performs no whole-chain scratch
      allocation.
* [ ] Sample-rate changes do not leak old-rate convolution history.
* [ ] A Convolver never processes a kernel whose stamped rate differs from the
      active stream, including publication/rate-change races and deferred
      retirement backpressure.
* [ ] A rate change without a matching Convolver kernel succeeds, produces the
      documented dry-bypass output, reports the awaited rate, and adopts a later
      matching publication only on a block boundary without old-rate history.
* [ ] Same-rate Convolver dry/kernel activation lasts exactly
      `ceil(0.005 * active_sample_rate_hz)` frames, is continuous and
      chunk-independent, runs no more than one kernel, and adds no callback
      allocation or unbounded scratch.
* [ ] A sample-rate boundary never runs the old-rate kernel to complete a fade;
      telemetry and tests distinguish this correctness-first boundary from the
      later clickless new-rate kernel fade-in.
* [ ] Executable latency/tail values and public stage descriptors agree.
* [ ] Invalid interleaved convolution geometry is rejected consistently.
* [ ] Equal-rate callback and offline chains execute the same single-limiter
      order; unequal-rate offline render executes exactly one PeakLimiter after
      SoXR and before NoiseShaper/terminal quantization.
* [ ] Final quantized output remains at or below the user-visible `-1.0 dBTP`
      target within the defined meter tolerance for adversarial phase/frequency
      sweeps and the available EBU true-peak corpus.
* [ ] The internal limiter guard is derived from documented quantizer/dither/
      reconstruction bounds and validated for every supported bit depth and
      NoiseShaper curve; it is not an arbitrary audio-sized headroom margin.
* [ ] Realtime process/finish paths pass no-allocation/no-deallocation tests on
      a newly created audio thread after setup.
* [ ] Performance reports include compatible before/after callback and offline
      baselines, median, p95, deadline utilization, peak temporary bytes, and
      steady-state bytes.
* [ ] Offline transient Rust memory, excluding the unavoidable final result,
      is bounded by the documented fixed block pool and does not grow with input
      duration or fixed-stage count; no fixed stage materializes a program-size
      intermediate vector.
* [ ] Compatible 512-frame active-chain median regression is at most 3%, and
      relative p95 deadline-utilization regression is at most 5%; the isolated
      active Saturation 4x median is lower than baseline.
* [ ] Any compatible primary case without a net improvement includes raw trial
      evidence and a dominant-cost explanation; reports never label a
      within-budget regression as an optimization.
* [ ] Library tests, strict Clippy matrices, rustfmt, docs, and quick/full audio
      quality gates pass.

## Definition of Done

* Tests cover impulse position, dry/wet phase alignment, exact finite drain,
  unknown-tail termination, reset/rate isolation, invalid geometry, and random
  chunk equivalence.
* Callback CPU and allocation evidence meet the agreed performance budget.
* Offline tail generation avoids safety-cap-sized precomputation/allocation.
* Public docs and timing metadata describe actual executable behavior.
* New lifecycle and performance contracts are captured in `.trellis/spec/`.

## Technical Approach

Implement a typed block-streaming `OutputRenderChain` with bounded block pools
and a stage-index EOS state machine, while keeping the ordinary callback path a
static fixed-1:1 chain. Consolidate fixed-stage finish behavior in one lifecycle
driver, make timing/tail metadata executable, and keep all realtime ownership
and automation state preallocated. Apply the selected Saturation,
rate-stamped-Convolver, and final output-domain limiter algorithms behind direct
oracles before removing stage-at-a-time render code.

## Implementation Plan

1. Capture compatible baselines and add failing timing, geometry, EOS, tail,
   rate-domain, true-peak, and allocation oracles.
2. Add shared fixed-1:1 finish lifecycle plus chain-level latency/tail/EOS
   composition and canonical executable stage metadata.
3. Implement Saturation nonlinear-delta filtering, evaluate-once FIR, fixed
   armed latency, sparse events, bounded three-mode transitions, and finish.
4. Implement rate-stamped Convolver publication, waiting telemetry, old-rate
   retirement, and single-kernel dry/active smoothing.
5. Migrate offline rendering to bounded typed blocks and relocate the single
   limiter after optional SoXR with the derived quantization guard.
6. Run parity, corpus, full quality, strict lint/test, CPU, and memory gates;
   optimize measured bottlenecks and update public docs plus Trellis specs.

## Out of Scope (Explicit)

* Replacing SoXR, rustfft, or libebur128.
* Changing the established EQ target curves, Bauer crossfeed response, dynamic
  loudness target curve, or noise-shaper coefficients without a separate
  independent algorithm finding.
* Automatic control-side IR resampling or rebuilding.
* General old/new Convolver dual-kernel crossfades.
* A generic automation event transport for DSP stages other than Saturation.
* A fully generic runtime graph scheduler or arbitrary graph topology.
* GPU DSP, planar buffers, or generalized sample formats.

## Technical Notes

* Primary code: `src/processor/saturation.rs`, `adapters.rs`,
  `adapters/convolver.rs`, `dsp_chain.rs`, `output_chain.rs`, `traits.rs`, and
  `convolver.rs`.
* Required project specs: realtime safety, streaming lifecycle, DSP state
  correctness, nonlinear/listening correctness, and quality guidelines.
* Existing benchmark entry points: `audio_callback_chain_perf`,
  `audio_quality_measurements`, and focused processor benchmarks.
