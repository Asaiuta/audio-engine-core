# Streaming DSP Lifecycle

> Executable contract for the object-safe streaming processor API in
> `src/processor/traits.rs`. Read this with `realtime-safety.md` before changing
> processors, adapters, `DspChain`, resampling, or offline rendering.

## 1. Scope / Trigger

This spec applies when code:

* implements or drives `StreamingProcessor`;
* constructs interleaved DSP block views;
* changes consumed/produced or backpressure behavior;
* adds processor latency, tail, finish/drain, reset, or sample-rate conversion;
* composes realtime or offline processor chains.

The target public contract directly replaced the former fixed in-place API.
Legacy names may appear only in explicit migration/history documentation; do
not reintroduce compatibility implementations, deprecated wrappers, or feature
gates for the removed surface.

## 2. Signatures

Core signatures are:

```rust
AudioBlockRef::new(samples: &[f64], channels: usize)
    -> Result<AudioBlockRef<'_>, AudioBlockError>
AudioBlockMut::new(samples: &mut [f64], channels: usize)
    -> Result<AudioBlockMut<'_>, AudioBlockError>

ProcessBuffers::in_place(block: AudioBlockMut<'_>) -> ProcessBuffers<'_>
ProcessBuffers::out_of_place(input: AudioBlockRef<'_>, output: AudioBlockMut<'_>)
    -> Result<ProcessBuffers<'_>, AudioBlockError>

process_checked(processor: &mut impl StreamingProcessor, buffers: ProcessBuffers<'_>)
    -> Result<ProcessProgress, ProcessError>
finish_checked(processor: &mut impl StreamingProcessor, output: AudioBlockMut<'_>)
    -> Result<ProcessProgress, ProcessError>

DspChain::process(&mut self, samples: &mut [f64], channels: usize)
    -> Result<ProcessProgress, ProcessError>
DspChain::reset(&mut self) -> Result<(), ProcessError>
DspChain::set_sample_rate(&mut self, sample_rate_hz: u32)
    -> Result<(), ProcessError>

OutputChainBuilder::build_callback_chain(&self)
    -> Result<DspChain, ProcessError>

OutputChainBuilder::build_render_chain_with_policy(
    &self,
    policy: OfflineRenderPolicy,
) -> Result<OutputRenderChain, String>

OutputRenderChain::render_with_policy(
    &mut self,
    samples: &[f64],
    policy: OfflineRenderPolicy,
) -> Result<RenderedOutput, ProcessError>
```

`StreamingProcessor` supplies `process`, `finish`, `reset`, `latency`, `tail`,
`output_sample_rate_hz`, enabled state, and off-RT sample-rate update methods.
`FrameDuration` carries both frames and its sample-rate domain. Finite
`TailSpec` values carry an exact `FrameDuration`.

Offline policy and result fields are part of the public contract:

```rust
pub struct OfflineRenderPolicy {
    pub timeline: RenderTimeline,
    pub unknown_tail: UnknownTailPolicy,
}

pub enum RenderTimeline { Compensated, RawCausal }

pub struct UnknownTailPolicy {
    pub energy_threshold_dbfs: f64,
    pub silence_hold_ms: u32,
    pub max_tail_ms: u32,
}

pub struct RenderedOutput {
    pub samples: Vec<f64>,
    pub final_limiter_gain_reduction_db: f64,
    pub rendered_frames: usize,
    pub algorithmic_latency_frames: usize,
    pub semantic_tail_frames: usize,
    pub tail_truncated: bool,
}
```

## 3. Contracts

### Buffer and progress

* Core DSP blocks are interleaved `f64`; file/device format conversion happens
  once at graph boundaries.
* Block views borrow caller memory and never allocate or copy.
* In-place means fixed 1:1: consume and produce the complete block, then return
  `NeedInput`. Partial in-place progress is invalid because overwritten input
  cannot be retried safely.
* Out-of-place calls may partially consume input. `NeedInput` requires all
  supplied input consumed; `NeedOutput` requires all supplied output capacity
  filled.
* Native variable-I/O counts are untrusted boundary data: validate consumed and
  produced counts against the supplied capacities before advancing cursors or
  slicing scratch. Multi-instance channel processors must also reject divergent
  per-channel progress instead of interleaving misaligned audio.
* Drivers use `process_checked` / `finish_checked`. These snapshot capacity,
  reject overruns, reject invalid direction, and reject zero progress when both
  input and output capacity are available.
* Fixed 1:1 adapters share one behavior: in-place calls process the complete
  block without copying; out-of-place calls copy/process
  `min(input_frames, output_frames)` into caller-owned output, then return
  `NeedOutput` only when unconsumed input remains.
* An adapter whose internal state was configured for a fixed channel count
  rejects a different block channel count before entering its DSP kernel.
  Channel-generic stages (currently volume and crossfeed) use the block count.
* `DspChain` holds `Box<dyn StreamingProcessor>`, drives every stage through
  `process_checked`, and returns full-block progress. The chain is marked
  bypassed only when it is empty or every stage reported transparent bypass.

### End of stream

* Ordinary `process` never returns `Finished`; end-of-stream is signalled only
  by driving `finish`.
* `finish` never returns `NeedInput`. It returns `NeedOutput` while more output
  remains or `Finished` when terminal.
* After any terminal `Finished`, repeated finish calls return `Finished` with
  zero produced frames. New input before `reset` returns
  `ProcessError::AlreadyFinished`.
* `reset` clears all Rust and native backend history and re-arms the processor
  for a logically new stream.
* SoXR-backed processors advance both native `input_frames` and `output_frames`
  exactly, use native `drain()` until it returns zero, and call native `clear()`
  during reset. Empty-input `process` calls are not a substitute for drain.
* `DspChain::reset` attempts every stage even after an error, then returns the
  first error. Its sample-rate update follows the same first-error policy and
  rejects zero before mutating any stage.

### Timing

* Algorithmic latency and semantic effect tail are distinct.
* Timing values carry their source sample-rate domain. A chain transforms
  rates through `output_sample_rate_hz`, converts every duration to the final
  output rate, sums fractional frame values, and rounds once.
* Default offline timing uses nearest-frame rounding for accumulated latency
  compensation and ceiling for accumulated finite-tail preservation.
* A finite tail is exact. `Unknown` and `Infinite` use the offline renderer's
  configured pre-dither energy/hold/max-tail policy.
* Offline composition is stage-complete: consume all stage input, drive its
  finish contract, then pass the combined process + finish output into the next
  stage. This is what carries an upstream last-frame impulse or convolution
  tail through downstream limiter and resampling stages.
* `Compensated` removes accumulated algorithmic latency exactly once at the
  final output sample rate and retains semantic tails. `RawCausal` retains the
  leading delay and all finalize output. Both report the same timing metadata.
* A natively drained SoXR sequence is duration- and impulse-aligned, so the
  resampler reports zero offline timeline latency; cropping nominal filter
  group delay would remove valid program audio.
* Unknown/infinite-tail RMS state persists across finish blocks. Detection runs
  before noise shaping, stops finish generation as soon as the configured
  continuous below-threshold hold is reached, and drops the quiet hold from the
  retained signal. The maximum is a safety cap, not the normal amount of work;
  reaching it sets `tail_truncated = true` even if a downstream stage later
  produces silence.

### Errors and realtime safety

* Contract failures use typed `ProcessError` variants; they are never swallowed
  or converted to log lines on the callback thread.
* `ProcessError::Backend` carries allocation-free static backend diagnostics.
  `ProcessError::Owned` exists for legacy setup/offline errors and must not be
  constructed on the realtime path.
* All callback-facing process/finish implementations obey
  `realtime-safety.md`: no alloc/dealloc, locks, logging, I/O, panics, or
  unbounded work.

## 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| `channels == 0` | `AudioBlockError::ZeroChannels` |
| `samples.len() % channels != 0` | `AudioBlockError::IncompleteFrame` |
| out-of-place channel counts differ | `AudioBlockError::ChannelMismatch` |
| block channel count differs from configured processor count | `ProcessError::ChannelCountMismatch` |
| consumed/produced exceeds capacity | `ProcessError::InvalidProgress` |
| non-empty input/output with zero progress | `ProcessError::Stalled` |
| partial or `NeedOutput` in-place | `ProcessError::InvalidProgress` |
| `Finished` from ordinary process | `ProcessError::InvalidProgress` |
| `NeedInput` from finish | `ProcessError::InvalidProgress` |
| input after terminal finish | `ProcessError::AlreadyFinished` |
| sample rate is zero | `ProcessError::InvalidSampleRate` or `TimingError::ZeroSampleRate` |
| stage input rate differs from fixed resampler input rate | `ProcessError::SampleRateMismatch` |
| unknown-tail threshold is non-finite or above 0 dBFS | `ProcessError::InvalidRenderPolicy` |
| silence hold is zero or exceeds maximum tail | `ProcessError::InvalidRenderPolicy` |
| finite finish does not terminate inside its declared bound | `ProcessError::Backend` |
| unknown/infinite finish reaches its maximum | successful output with `tail_truncated = true` |
| hot-path native error | allocation-free `ProcessError::Backend` |

## 5. Good / Base / Bad Cases

* Good: a resampler fills output, returns `NeedOutput` with exact input/output
  counts, then resumes at the unconsumed input frame.
* Good: an unknown decay crosses the RMS threshold, remains below it for the
  hold duration across multiple blocks, and stops without computing the rest of
  the safety cap.
* Base: a fixed EQ processes the complete block in place and returns equal
  consumed/produced counts with `NeedInput`.
* Good: a fixed out-of-place stage fills a short output, returns equal
  consumed/produced prefix counts with `NeedOutput`, and resumes from the
  caller-advanced input without replaying overwritten data.
* Bad: an in-place processor returns partial consumption after it may already
  have overwritten the corresponding unconsumed samples.
* Bad: an offline renderer calls `process(&[])` as an undocumented flush instead
  of repeatedly driving the processor's finish contract.
* Bad: an unknown-tail renderer always computes `max_tail_ms` and only then
  scans/truncates the buffered result; this preserves output but wastes up to
  the full safety-cap CPU and memory on every render.
* Bad: callback code ignores the `Result` from `DspChain::process`, logs the
  error on the audio thread, or unwraps it across the callback boundary.

## 6. Tests Required

* Block-view tests cover immutable and mutable zero-channel, incomplete-frame,
  frame count, channel count, and pointer identity.
* Progress tests cover overrun, wrong direction, zero-progress stall, complete
  in-place 1:1, ordinary-process `Finished`, and finish `NeedInput` rejection.
* Stateful lifecycle tests cover multi-call finite drain, `Finished(n)` followed
  by stable `Finished(0)`, process-after-finish rejection, and reset re-arming.
* Timing tests cover cross-rate conversion and floor/nearest/ceil behavior.
* No-allocation tests cover in-place, out-of-place, and callback-facing finish
  after setup.
* Processor/chain migrations add random chunking equivalence and native reset
  isolation tests, not only finite-output smoke tests.
* Variable-rate tests cover short and long exact-ratio streams, random input and
  output chunking, a mid-stream impulse peak at the rate-converted frame, native
  drain idempotence, and process/finish no-allocation after setup.
* Offline finalize tests cover a last-frame impulse, raw-vs-compensated content
  equivalence, finite convolution + limiter + resampler propagation, and final
  timing metadata.
* Unknown-tail tests assert retained output is block-size independent, decays
  stop before the configured maximum is generated, persistent energy reaches
  the exact cap, and capped status survives later silence trimming.

## 7. Wrong vs Correct

### Wrong

```rust
let progress = processor.process(buffers)?;
// Trust arbitrary counts and retry the same aliased in-place input.
```

### Correct

```rust
let progress = process_checked(processor, buffers)?;
match progress.state() {
    ProcessState::NeedInput => { /* advance input */ }
    ProcessState::NeedOutput => { /* advance output and retry remaining input */ }
    ProcessState::Finished => { /* only reachable through finish_checked */ }
}
```

For a fixed callback chain, propagate the typed result to a callback boundary
that can apply a preselected non-panicking fault policy:

```rust
let _progress = chain.process(samples, channels)?;
```

For offline unknown tails, energy detection belongs inside the finish loop, not
in a post-pass after generating the maximum:

```rust
// Wrong: always pay max_tail_ms, then trim.
while generated < max_tail_frames { append(finish_checked(...)?); }
trim_below_threshold(&mut output);

// Correct: keep detector state across blocks and stop as soon as hold is met.
while generated < max_tail_frames {
    let progress = finish_checked(processor, output_block)?;
    if let Some(silence_start) = detector.observe(produced_samples, channels, first_frame) {
        output.truncate(silence_start * channels);
        break;
    }
}
```

## Scenario: Dynamic Convolver Publication And Reclamation

### 1. Scope / Trigger

Apply this scenario when an impulse response can change while a
`StreamingProcessor`, `DspChain`, callback chain, or offline render chain is
alive. It replaces the former unreachable `disposal_slot()` composition path.

### 2. Signatures

```rust
ConvolverControl::new(enabled: bool) -> ConvolverControl
ConvolverControl::publish(&self, kernel: FFTConvolver) -> u64
ConvolverControl::reclaim_retired(&self) -> bool
ConvolverControl::status(&self) -> ConvolverStatus
ConvolverProcessor::new(control: ConvolverControl) -> ConvolverProcessor
OutputChainBuilder::convolver_control(&self) -> ConvolverControl
```

### 3. Contracts

* A control handle is cloneable for control-side callers but has exactly one
  live audio consumer. Separate callback/render consumers require separate
  handles.
* `publish` accepts the kernel by value. It serializes concurrent control-side
  publish/reclaim calls, assigns install-order generations, opportunistically
  drains the retired slot, and is latest-wins until audio withdraws a value.
  The control-only serialization gate is never acquired by audio.
* Audio ownership is fixed and bounded: `owned`, `incoming`,
  `pending_retire`, and one `retired` hand-off slot. Audio may withdraw,
  adopt, or hand off ownership, but never drops or deep-clones a heavy kernel.
* A full retirement path keeps the current kernel processing, leaves the new
  adoption pending, and reports `backpressured` plus
  `pending_reclamations` through the allocation-free status snapshot.
* `set_enabled(false)` followed by process or repeated finish calls drains
  disabled ownership staging. After control-side `reclaim_retired()` consumes
  every hand-off, `status().is_quiescent()` is the only condition that permits
  destroying an audio-owned processor from a non-realtime thread. Publishers
  must be stopped before taking that shutdown snapshot and remain stopped.
* Dynamic convolver timing remains zero algorithmic latency and a finite
  `ir_length - 1` tail in the current sample-rate domain; reset preserves the
  adopted control/kernel configuration while clearing signal history.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| More than one live audio consumer uses one control | documented misuse; use distinct controls |
| Control publishes before audio withdraws | older publication is superseded on control and counted |
| Retirement slot is full | normal processing continues; adoption is deferred and status is backpressured |
| Control reclaims a retired slot | next block/finish retries the pending hand-off without waiting |
| Disabled terminal processor still owns a kernel | repeated `finish` calls advance retirement; no `NeedInput` is returned |
| Owned kernel loses unique ownership | allocation-free `ProcessError::Backend`, never an audio-side clone |
| `reclaim_retired` called with no retired value | returns `false`, no state change |

### 5. Good / Base / Bad Cases

* Good: a builder caller retains `builder.convolver_control()`, publishes by
  value after callback chain type erasure, and periodically reclaims retired
  kernels off the callback thread.
* Base: a burst of publications coalesces to the latest withdrawn generation
  while status counters remain bounded and auditable.
* Bad: retaining a strong kernel `Arc` in the producer, or exposing an
  internal retirement slot only before type erasure, prevents unique adoption
  or makes reclamation unreachable.

### 6. Tests Required

* `convolver_control_stress_remains_bounded_and_adopts_latest_generation`:
  10,000 updates, burst coalescing, saturation/recovery, disable/reenable,
  final generation and counter invariants.
* `convolver_control_serializes_concurrent_publishers`: ordered generations
  and latest install under concurrent control clones.
* `convolver_kernels_are_destroyed_by_control_not_audio_thread`: replaced and
  retired destructor probes never fire on the audio thread.
* `convolver_processor_kernel_swap_is_allocation_free_on_audio_side` and
  `convolver_terminal_finish_can_retire_to_control_quiescence`: adoption,
  retirement, backpressure, finish, and recovery under `assert_no_alloc`.
* Direct nested-loop oracle tests cover mono/stereo, overlap-save/partitioned,
  irregular chunks, exact tail, stable `Finished(0)`, and reset isolation.

### 7. Wrong vs Correct

#### Wrong

```rust
let swap = ArcSwapOption::empty();
let chain = OutputChainBuilder::new(params).build_callback_chain()?;
// The erased processor's retirement slot has no reachable control owner.
```

#### Correct

```rust
let builder = OutputChainBuilder::new(params);
let control = builder.convolver_control();
let mut chain = builder.build_callback_chain()?;
control.publish(kernel);       // control thread
chain.process(samples, channels)?; // audio thread, fixed ownership hand-off
control.reclaim_retired();     // control/offline thread
```
