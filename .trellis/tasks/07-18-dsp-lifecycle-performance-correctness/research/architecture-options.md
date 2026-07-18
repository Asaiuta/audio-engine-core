# Architecture Options for DSP Lifecycle Completion

## Scope

This note compares execution architectures for fixing processor tails, timing,
callback end-of-stream, and offline rendering. Audio correctness and realtime
safety are hard constraints. CPU and memory claims below are cost models until
the measurement plan produces compatible before/after reports.

## Current execution facts

* Callback processing is a fixed 1:1 `DspChain` of eight trait-object stages.
  It is already allocation-free after setup and measured at a small fraction of
  a 48 kHz callback deadline.
* Callback `DspChain` has no chain-level finish/timing state machine.
* Offline rendering is typed/static, but `drive_offline_stage` allocates a new
  full-length `Vec` for every stage. The old stage input and new stage output
  coexist while each stage runs.
* Fixed adapters use the out-of-place branch offline, which copies every input
  frame before applying the stage DSP.
* A one-hour 48 kHz stereo `f64` stream is about 1.382 GB. While a fixed stage
  runs, the current renderer may hold the caller input, the previous stage
  vector, and the next stage vector (roughly 4.15 GB before allocator slack).
* The final returned vector is unavoidable with the current public API; full
  intermediate vectors are not.

## Comparable patterns

* VST3 and CLAP expose tail length/tail samples as an explicit processor
  lifecycle property rather than inferring it from static stage descriptions.
* JUCE's dry/wet and oversampling components expose wet-path latency and use a
  delay line to align the dry path.
* Pull/push audio graphs normally preallocate a small block pool and propagate
  EOS through the graph; they do not materialize one complete program buffer
  per node.
* This repository already follows the same principles at smaller boundaries:
  caller-owned callback blocks, explicit consumed/produced progress, SoXR
  drain, and preallocated Convolver ownership slots.

Reference entry points:

* JUCE `dsp::DryWetMixer::setWetLatency`
* JUCE `dsp::Oversampling::getLatencyInSamples`
* VST3 `IAudioProcessor::getTailSamples`
* CLAP `clap_plugin_tail`

## Option A: Targeted lifecycle patch on the current renderer

### How it works

* Implement production saturation/IIR finish and correct timing metadata.
* Add a bounded callback-chain finish state machine.
* Keep the existing stage-at-a-time, out-of-place offline renderer.
* Reset Convolver history at a rate-domain change and validate direct geometry.

### CPU and memory

* Normal callback processing can remain bit-for-bit and instruction-for-
  instruction unchanged except for the saturation fix.
* Finish work is proportional to the actually retained tail and distributed
  over caller-provided blocks.
* Offline fixed stages retain one full copy plus one full DSP pass per stage.
* Engine-owned peak offline memory remains close to two full program buffers,
  in addition to the caller's input.

### Advantages

* Lowest implementation and regression risk.
* Smallest patch; easy to review and bisect.
* Fixes the confirmed audible/data-loss defects quickly.

### Disadvantages

* Does not meet the stated best-memory/best-offline-CPU objective.
* Long renders continue to be dominated by full-buffer allocation, copying,
  and memory bandwidth.

## Option B: One owned render vector with in-place fixed stages

### How it works

* Clone input once into the final working vector.
* Process every fixed 1:1 stage in-place in chunks, appending its finish output
  to the same vector before advancing to the next stage.
* Allocate a second full vector only at a true variable-I/O boundary such as
  SoXR resampling.
* Add the same callback finish state machine as Option A.

### CPU and memory

* Removes the mandatory full-buffer copy performed by each fixed adapter's
  out-of-place branch.
* Engine-owned memory is normally one full vector; around resampling, source
  and destination vectors coexist.
* Every stage still walks the complete program separately, so long streams are
  repeatedly written to and read from memory rather than remaining cache-hot.

### Advantages

* Large memory and copy reduction with moderate implementation complexity.
* Reuses existing processor contracts and stage-complete tail ordering.
* Easier to prove equivalent than a new scheduler.

### Disadvantages

* Not the lowest possible DRAM traffic for long renders.
* Appending unknown tails to the same `Vec` still needs careful capacity growth
  to avoid excessive reallocations or safety-cap-sized reservation.

## Option C: Typed block-streaming output pipeline (recommended for the stated objective)

### How it works

* Keep the callback's normal fixed 1:1 hot path simple.
* Rewrite typed `OutputRenderChain::render` as a bounded block pipeline:
  source input block -> all source-rate stages in-place -> optional SoXR output
  block -> output-rate stages -> terminal quantization -> final result vector.
* Use the manifest as the single order source, but keep static dispatch for the
  offline typed chain.
* Propagate EOS with a stage-index state machine. A stage's finish output is
  processed in-place through every downstream fixed stage before the next
  stage begins its own finish.
* Reuse caller output for fixed callback-chain finish; no full callback scratch
  buffer is required. The only persistent finish state is stage index,
  lifecycle flags, timing, and one reusable energy detector.

### CPU and memory

* Temporary render memory becomes `O(block_frames * channels)` plus SoXR's
  existing bounded work buffers. The final returned vector remains `O(output)`.
* A 4096-frame stereo pair of `f64` ping-pong blocks is 128 KiB; a 512-frame
  stereo pair is 16 KiB. Even 8-channel 4096-frame double buffering is 512 KiB.
* Samples stay hot while crossing adjacent fixed stages, reducing DRAM traffic.
* Fixed-stage DSP arithmetic is unchanged; scheduler overhead is once per block
  and bounded by the static stage count.
* Unknown tails stop during generation, so neither CPU nor memory pays the
  configured 30-second safety maximum in the normal decay case.

### Advantages

* Best expected offline peak memory, cache locality, and long-render throughput.
* One streaming model covers ordinary data and EOS propagation.
* Static offline dispatch avoids a generic scheduler's virtual-call and buffer
  ownership costs.

### Disadvantages

* Highest implementation and verification cost of the three options.
* Variable-rate backpressure, latency compensation, tail trimming, and final
  `Vec` growth must be tested under irregular input/output chunk sizes.
* A one-shot rewrite would be risky; it should be delivered in small commits
  with an independent direct oracle and old/new render parity fixtures.

## Rejected upper-bound design: one fully generic graph executor

A generic trait-object scheduler with a preallocated buffer pool could run both
callback and offline graphs. It maximizes abstraction reuse, but adds scheduler
state, capability negotiation, virtual dispatch, and variable-I/O complexity to
the callback surface. The current graph is fixed and known, so this pays runtime
and review cost without a demonstrated product need. It should remain out of
scope unless arbitrary runtime graph topology becomes a requirement.

## Comparison

| Property | A: targeted | B: in-place vector | C: block streaming |
| --- | --- | --- | --- |
| DSP correctness | Full after fixes | Full after fixes | Full after fixes |
| Normal callback overhead | Minimal | Minimal | Minimal |
| Offline full-buffer copies | Per fixed stage | Initial + rate boundary | Initial block feed only |
| Engine temporary memory | About 2x stream | About 1x; 2x at rate boundary | Bounded block buffers |
| Long-render cache locality | Low | Medium | High |
| Implementation risk | Low | Medium | High |
| Future variable-I/O composition | Limited | Limited | Strong |

## Recommendation

Choose Option C if the requirement really is best practical CPU and memory,
but implement it incrementally: first make processor timing/tails correct,
then introduce the block pipeline behind parity tests, then remove the old
stage-materializing driver. Choose Option B if schedule/risk is more important
than the final cache-locality improvement.
