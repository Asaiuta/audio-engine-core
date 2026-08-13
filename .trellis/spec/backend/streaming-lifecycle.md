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

DspChain::new(sample_rate_hz: u32) -> Result<DspChain, ProcessError>
DspChain::with_capacity(capacity: usize, sample_rate_hz: u32)
    -> Result<DspChain, ProcessError>
DspChain::add<P: FixedInPlaceProcessor + 'static>(&mut self, processor: P)
    -> Result<&mut DspChain, ProcessError>
DspChain::process(&mut self, samples: &mut [f64], channels: usize)
    -> Result<ProcessProgress, ProcessError>
DspChain::reset(&mut self) -> Result<(), ProcessError>
DspChain::set_sample_rate(&mut self, sample_rate_hz: u32)
    -> Result<(), ProcessError>

OutputChainBuilder::build_callback_chain(&self)
    -> Result<DspChain, ProcessError>

OutputChainBuilder::build_render_chain(
    &self,
    source_sample_rate_hz: u32,
) -> Result<OutputRenderChain, ProcessError>

OutputChainBuilder::build_render_chain_with_policy(
    &self,
    source_sample_rate_hz: u32,
    policy: OfflineRenderPolicy,
) -> Result<OutputRenderChain, ProcessError>

OutputRenderChain::render_with_policy(
    &mut self,
    samples: &[f64],
    policy: OfflineRenderPolicy,
) -> Result<RenderedOutput, ProcessError>

OutputRenderChain::render_with_policy_and_block_frames(
    &mut self,
    samples: &[f64],
    policy: OfflineRenderPolicy,
    block_frames: usize,
) -> Result<RenderedOutput, ProcessError>

FFTConvolver::new(ir_data: &[f64], channels: usize)
    -> Result<FFTConvolver, ProcessError>

NoiseShaper::new(channels: usize, sample_rate_hz: u32, bits: u32)
    -> Result<NoiseShaper, ProcessError>
NoiseShaper::process(&mut self, samples: &mut [f64], channels: usize)
    -> Result<(), ProcessError>

DynamicLoudness::new(channels: usize, sample_rate_hz: f64)
    -> Result<DynamicLoudness, ProcessError>
DynamicLoudness::process(&mut self, samples: &mut [f64], channels: usize)
    -> Result<(), ProcessError>

PeakLimiter::new(channels: usize, sample_rate_hz: u32, ...)
    -> Result<PeakLimiter, ProcessError>
PeakLimiter::process(&mut self, samples: &mut [f64], channels: usize)
    -> Result<(), ProcessError>

LoudnessNormalizer::new(channels: usize, sample_rate_hz: u32, config: LoudnessConfig)
    -> Result<LoudnessNormalizer, ProcessError>
LoudnessNormalizer::process(&mut self, samples: &mut [f64], channels: usize)
    -> Result<(), ProcessError>

LoudnessMeter::new(channels: usize, sample_rate_hz: u32)
    -> Result<LoudnessMeter, ProcessError>
LoudnessMeter::with_layout(layout: &ChannelLayout, sample_rate_hz: u32)
    -> Result<LoudnessMeter, ProcessError>
LoudnessMeter::process(&mut self, samples: &[f64])
    -> Result<(), ProcessError>
LoudnessMeter::has_reliable_measurement(&self) -> bool

SpectrumAnalyzer::new(fft_size: usize, num_bins: usize)
    -> Result<SpectrumAnalyzer, ProcessError>
SpectrumAnalyzer::analyze(&mut self, samples: &[f64], sample_rate_hz: u32)
    -> Result<&[f32], ProcessError>
```

`StreamingProcessor` supplies only the shared lifecycle: `process`, `finish`,
`reset`, `latency`, `tail`, `output_sample_rate_hz`, and the off-RT sample-rate
update. It does not expose enabled/bypass controls. Those operations remain on
the concrete atomic parameter or control handle that can honor them.
`FixedInPlaceProcessor: StreamingProcessor` is the public refinement used for
fixed callback-chain admission. `FrameDuration` carries both frames and its
sample-rate domain. Finite `TailSpec` values carry an exact `FrameDuration`.

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
* Exported raw slice processors are checked shells over `AudioBlockMut`. They
  reject zero channels, incomplete frames, and configured-channel mismatches
  before changing samples or DSP history. Fixed-channel raw APIs always receive
  the process-time channel count explicitly; they never infer success by
  truncating a partial final frame.
* The configured-versus-actual channel check has one crate-level implementation
  shared by raw shells and adapters. Inner `process_validated` kernels are
  crate-private. An adapter may call one only after its typed block driver has
  enforced the same geometry, so callback work does not validate twice.
* Geometry-dependent constructors validate channels and sample rate before
  allocating DSP state or registering realtime snapshot readers. Internal
  constructors named `*_validated` are crate-private setup kernels, not public
  unchecked compatibility APIs.
* `SpectrumAnalyzer` requires `fft_size >= 4` and `num_bins > 0`. `analyze`
  rejects a zero sample rate before touching FFT scratch, magnitude bins, cached
  bin ranges, or the reusable result buffer.
* `FixedInPlaceProcessor` implementors promise that `ProcessBuffers::in_place`
  consumes and produces the complete block and preserves its sample-rate
  domain. `DspChain::add` requires this marker, then defensively checks
  `output_sample_rate_hz` against the chain rate before erasing the stage to
  `Box<dyn StreamingProcessor>`. `StreamingResampler` intentionally does not
  implement the marker, including for equal-rate construction; variable-I/O
  stages need a driver with caller-visible input/output buffers.
* `DspChain::{new,with_capacity}` reject zero Hz and there is no `Default`
  chain. Sample rate is required topology, not a value that may be invented by
  a convenience constructor. The chain drives every admitted stage through
  `process_checked` and returns full-block progress. It is marked bypassed only
  when it is empty or every stage reports per-call transparent bypass.
* `ProcessProgress::is_bypassed` is output metadata for one process call, not a
  generic stage-control capability. Generic code must not infer that every
  `StreamingProcessor` has an enable setter from this progress bit.
* `OutputChainParams` owns only the output/device rate shared by its callback
  and offline products. `build_callback_chain` consumes and validates that
  rate. Each offline `build_render_chain*` operation receives and validates its
  source rate explicitly because only the offline renderer owns optional rate
  conversion.

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
* Offline execution streams bounded typed blocks through the canonical stage
  order. Fixed stages do not materialize program-sized intermediates;
  temporary Rust memory excluding final output is bounded by the configured
  block pool. `block_frames == 0` is rejected before any processor mutates.
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
* `LoudnessMeter` owns a concrete EBU R128 backend. Construction and explicit
  channel-map setup are fallible; a failed backend is never represented by a
  usable placeholder meter. Its steady-state `process` call validates an
  `AudioBlockRef` before backend mutation, maps ingestion failures to the
  allocation-free `ProcessError::Backend` variant, and performs no logging or
  allocation after setup.
* A meter is reliable only after successful construction and at least one
  successfully consumed 400 ms momentary window. Cached readings before that
  point are not a successful measurement.

## 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| `channels == 0` | `AudioBlockError::ZeroChannels` |
| `samples.len() % channels != 0` | `AudioBlockError::IncompleteFrame` |
| out-of-place channel counts differ | `AudioBlockError::ChannelMismatch` |
| block channel count differs from configured processor count | `ProcessError::ChannelCountMismatch` |
| raw constructor receives zero channels | `ProcessError::InvalidBlock(AudioBlockError::ZeroChannels)` before DSP allocation |
| raw constructor or sample-rate update receives zero Hz | typed `ProcessError` before state mutation |
| `DynamicLoudness` receives a non-finite or non-positive `f64` sample rate | `ProcessError::InvalidGeometry` before state mutation |
| `SpectrumAnalyzer::new` receives `fft_size < 4` or `num_bins == 0` | `ProcessError::InvalidGeometry`; do not plan an FFT or allocate analyzer buffers |
| `SpectrumAnalyzer::analyze` receives zero Hz | `ProcessError::InvalidSampleRate`; cached FFT/bin/result state remains unchanged |
| consumed/produced exceeds capacity | `ProcessError::InvalidProgress` |
| non-empty input/output with zero progress | `ProcessError::Stalled` |
| partial or `NeedOutput` in-place | `ProcessError::InvalidProgress` |
| `Finished` from ordinary process | `ProcessError::InvalidProgress` |
| `NeedInput` from finish | `ProcessError::InvalidProgress` |
| input after terminal finish | `ProcessError::AlreadyFinished` |
| sample rate is zero | `ProcessError::InvalidSampleRate` or `TimingError::ZeroSampleRate` |
| `DspChain::new` or `with_capacity` receives zero Hz | `ProcessError::InvalidSampleRate { processor: "DspChain", .. }`; no chain is constructed |
| a type without `FixedInPlaceProcessor` is passed to `DspChain::add` | compile-time trait-bound failure |
| a marker implementor reports an output rate different from the chain rate | `ProcessError::UnsupportedOperation` before insertion |
| callback builder has a zero output rate | `ProcessError::InvalidSampleRate` naming `OutputChainBuilder::output_sample_rate` |
| offline render builder receives a zero source rate or owns a zero output rate | `ProcessError::InvalidSampleRate` naming the corresponding `OutputRenderChain` rate |
| stage input rate differs from fixed resampler input rate | `ProcessError::SampleRateMismatch` |
| unknown-tail threshold is non-finite or above 0 dBFS | `ProcessError::InvalidRenderPolicy` |
| silence hold is zero or exceeds maximum tail | `ProcessError::InvalidRenderPolicy` |
| finite finish does not terminate inside its declared bound | `ProcessError::Backend` |
| `FFTConvolver::new` receives zero channels, empty IR, or an incomplete interleaved frame | typed `ProcessError`; never panic |
| `LoudnessMeter` receives zero channels or a zero sample rate | `ProcessError::InvalidBlock`/`InvalidSampleRate` before backend setup |
| EBU R128 construction or channel-map setup rejects the requested geometry | static `ProcessError::Backend`/`InvalidGeometry`; no meter is returned |
| `LoudnessMeter::process` receives an incomplete interleaved frame | `ProcessError::InvalidBlock(AudioBlockError::IncompleteFrame)` before EBU state or counters change |
| EBU R128 rejects an otherwise valid audio block | allocation-free `ProcessError::Backend`; cached metrics and frame counters are not advanced |
| fewer than 400 ms has been successfully consumed | `has_reliable_measurement() == false` |
| at least 400 ms has been successfully consumed | `has_reliable_measurement() == true` |
| offline block size is zero | `ProcessError::InvalidRenderPolicy` or another named typed validation error before processing |
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
* Good: a custom fixed 1:1 stage explicitly implements
  `FixedInPlaceProcessor`, is admitted at setup, and still passes the chain's
  rate-preservation check.
* Good: callback construction needs only the device/output rate, while offline
  render construction receives the source rate on the operation that consumes
  it.
* Good: a fixed out-of-place stage fills a short output, returns equal
  consumed/produced prefix counts with `NeedOutput`, and resumes from the
  caller-advanced input without replaying overwritten data.
* Good: a standalone `NoiseShaper` configured for stereo rejects a mono block
  with `ChannelCountMismatch`; samples, RNG streams, and error history remain
  exactly unchanged.
* Base: an adapter validates its typed block once and calls the crate-private
  validated kernel; valid output remains bit-identical to the raw checked API.
* Bad: a public slice API divides by caller channels, uses `chunks_exact`
  without reporting the remainder, or bypasses channels beyond configured
  state. Those behaviors turn invalid geometry into a panic or partial success.
* Bad: an in-place processor returns partial consumption after it may already
  have overwritten the corresponding unconsumed samples.
* Bad: adding a boolean geometry query to `StreamingProcessor`, admitting a
  resampler to `DspChain` when its rates happen to match, or adding a callback
  source-rate field that the callback does not consume.
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
* Every exported raw geometry-dependent constructor covers zero channels/rate.
* Loudness meter tests cover invalid geometry, explicit-layout setup failures,
  incomplete-frame rejection before mutation, typed backend error mapping,
  400 ms reliability gating, and steady-state `assert_no_alloc` processing.
  Every raw process shell covers zero channels, incomplete frames, and fixed
  channel mismatch where applicable. Rejection tests assert unchanged samples
  plus representative algorithm state, and execute error paths under
  `assert_no_alloc`.
* Spectrum tests cover FFT sizes 0 through 3, zero output bins, and zero-rate
  analysis after a populated cache; every cache and reusable buffer remains
  unchanged on rejection.
* Adapter constructor tests prove invalid geometry is rejected without
  allocating snapshot-reader or DSP state. Existing adapter valid-path tests
  retain bit-exact output and steady-state no-allocation assertions.
* Processor/chain migrations add random chunking equivalence and native reset
  isolation tests, not only finite-output smoke tests.
* Chain admission tests include a compile-fail example proving
  `StreamingResampler` cannot be passed to `DspChain::add`, positive tests for
  every fixed adapter, and a defensive rejection test for a marker implementor
  that changes the rate.
* Construction tests prove both `DspChain` constructors reject zero Hz and
  valid explicit rates succeed; no test relies on a default chain rate.
* Output-chain tests prove callback construction validates only output rate,
  while both offline builders reject zero source/output rates and preserve
  equal-rate versus resampling behavior.
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

Keep fixed-chain capability and operation-owned rate inputs explicit:

```rust
// Wrong: the broad lifecycle admits variable I/O and callback params carry an
// offline-only source rate.
fn add<P: StreamingProcessor>(&mut self, processor: P) { /* ... */ }
let render = builder.build_render_chain()?;

// Correct: fixed topology is a type bound; source rate belongs to rendering.
impl FixedInPlaceProcessor for MyFixedStage {}
let mut chain = DspChain::new(device_rate_hz)?;
chain.add(MyFixedStage::new())?;
let render = builder.build_render_chain(source_rate_hz)?;
```

For a standalone fixed-channel DSP processor, keep the checked shell public and
the already-validated kernel crate-private:

```rust
// Wrong: floors an incomplete frame and can partially process a mismatch.
pub fn process(&mut self, samples: &mut [f64], channels: usize) {
    self.process_validated(samples, channels);
}

// Correct: all rejection happens before samples or processor history change.
pub fn process(&mut self, samples: &mut [f64], channels: usize)
    -> Result<(), ProcessError>
{
    let block = AudioBlockMut::new(samples, channels)?;
    validate_processor_channels("NoiseShaper", Some(self.rng_state.len()), channels)?;
    self.process_validated(block.into_samples(), channels);
    Ok(())
}
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

## Scenario: Feature-Selected Resampler Channel Architecture

### 1. Scope / Trigger

Apply this scenario when changing `StreamingResampler`, a backend adapter,
SoXR/Rubato or nonlinear engine/channel construction, reusable resampler
memory accounting, phase behavior, or a resampler routing policy.

### 2. Signatures

```rust
StreamingResampler::with_quality(
    channels: usize,
    from_rate: u32,
    to_rate: u32,
    phase: PhaseResponse,
    quality: ResampleQuality,
) -> Result<StreamingResampler, ResamplerError>

StreamingResampler::working_buffer_bytes(
    channels: usize,
    from_rate: u32,
    to_rate: u32,
) -> Result<usize, ResamplerError>

StreamingResampler::process_output_capacity_frames(
    &self,
    input_frames: usize,
) -> Result<usize, ResamplerError>

// One-shot facade: fallible construction plus validated interleaved input.
Resampler::new(channels: usize, from_rate: u32, to_rate: u32)
    -> Result<Resampler, ResamplerError>
Resampler::resample_parallel(
    &self,
    input: &[f64],
    phase: PhaseResponse,
    quality: ResampleQuality,
) -> Result<Vec<f64>, ResamplerError>

ResamplerError::InvalidBlock(AudioBlockError)

// Removed in Gate 7: max_output_len_for_input, max_output_samples_per_chunk,
// input_frames_for_output_frames (mixed sample/frame units, magic margins,
// unchecked arithmetic). There are no compatibility wrappers.

// Private pure-Rubato adapter boundary.
MonoBackend::new_interleaved(
    from_rate: u32,
    to_rate: u32,
    phase: PhaseResponse,
    quality: ResampleQuality,
    channels: usize,
) -> Result<MonoBackend, BackendInitError>

MonoBackend::process(&mut self, input: &[f64], output: &mut [f64])
    -> Result<BackendProgress, BackendProcessError>
```

### 3. Contracts

* Feature precedence remains `soxr` first. Exactly two channels select one
  native `Soxr<Stereo<f64>>` stream and pass validated caller-owned interleaved
  frames directly to process/drain. Other channel counts retain one native mono
  stream per channel plus setup-allocated deinterleave, per-channel output, and
  reinterleave scratch. The fallback must reject divergent channel progress.
* A pure `rubato` build constructs exactly one complete interleaved backend for
  the configured channel count. `PhaseResponse::Linear` uses one Rubato engine;
  `Minimum` and `Maximum` use one setup-designed rational FIR bank and one
  interleaved nonlinear engine. `process` and `drain` pass caller-owned
  interleaved buffers directly to that backend. Do not duplicate a sinc table,
  FFT plan, or nonlinear coefficient bank by constructing one engine per
  channel.
* Rubato adapter staging uses fixed-capacity, setup-allocated sample rings, not
  moving `Vec` prefixes. The input ring holds exactly two fixed backend chunks;
  consuming one complete chunk guarantees that the next front chunk is
  contiguous even after wrap. The output ring applies strict backpressure.
  Neither ring may grow, overwrite unread audio, log, or allocate during
  process/drain. Push/pop may use at most two bounded contiguous copies.
* Exact-2x Linear Rubato High upsampling uses one interleaved 127-tap symmetric
  half-band engine. Other common Linear ratios use the FFT engine at every
  quality tier: UltraHigh selects one FFT sub-chunk (a 2x longer internal FIR)
  while Low through High keep two sub-chunks.
  Pathological Linear ratios use sinc. Minimum/Maximum reduce the ratio and use
  the spectral engine when `up <= 16`, otherwise the contiguous time-domain
  polyphase engine. Both use the identical causal kernel and reject reduced
  geometry beyond 1024 or the coefficient-bank bound; a performance
  optimization must not broaden half-band routing, change this threshold
  without evidence, or fall back to a Linear engine.
* `working_buffer_bytes` accounts for reusable adapter-owned PCM scratch, not
  opaque backend engine allocations. It returns zero for the direct stereo
  SoXR path and pure Rubato, and the exact deinterleave/per-channel/reinterleave
  scratch capacity for the SoXR mono fallback. Setup-allocation measurements
  capture opaque native or Rubato engine memory separately.
* One-shot geometry is validated, not inferred. `Resampler::new` is fallible
  and rejects zero channels or rates before any work. `resample_parallel`
  validates complete interleaved frames (`AudioBlockRef`) before its
  equal-rate bypass, so a trailing partial frame is never silently dropped by
  "enabling" conversion, empty input still returns `Ok(vec![])`, and
  `ResamplerError::InvalidBlock` preserves the typed `AudioBlockError`.
* `process_output_capacity_frames` is the single checked frame-domain
  provisioning contract: exact rational ceiling conversion at the output rate
  plus a fixed 64-frame per-channel burst allowance, `Ok(0)` for zero input,
  and `CapacityOverflow { buffer: "process output" }` on checked overflow. It
  replaces the three removed mixed-unit/unchecked helpers and is shared by
  one-shot chunk scratch, the streaming layout, offline finish bounds, and
  every resampler bench. Backpressure remains authoritative: callers advance
  from `ProcessProgress`, never from the capacity estimate alone.
* SoXR recipe identity resolves every public tier to a distinct pinned recipe:
  `Low -> QualityRecipe::Low`, `Standard -> QualityRecipe::Medium`,
  `High -> QualityRecipe::high()` (20-bit), and
  `UltraHigh -> QualityRecipe::very_high()` (28-bit). No two tiers share a
  recipe; quality labels are never aliases.
* Offline finish bounds use declared timing, not process estimates.
  `finish_frame_limit` converts input frames exactly (`FrameRounding::Ceil` at
  the output rate), adds declared latency plus finite tail plus one block for
  `None`/`Finite` tails, or `max_tail_ms` plus latency for `Unknown`/`Infinite`,
  floors at 1, and uses checked sums (`TimingError::FrameCountOverflow`). For
  nonlinear resamplers this declared-latency/tail bound exceeds any
  process-capacity estimate.
* Backend consumed/produced values are frame counts. Interleaved slice lengths
  must be divisible by the configured channel count before division or
  slicing, and returned progress must be checked against caller frame
  capacities.
* Caller-visible output accounting is independent of the storage route. Every
  frame copied from a FIFO or written directly into caller-owned output must
  advance the backend's cumulative `emitted` count exactly once before the call
  returns. Merely generating a staged frame does not make it emitted. Drain
  computes its remaining duration from this count and must never reproduce
  frames already returned by `process`.
* Non-integer-ratio direct output is prefix-budgeted. The Rubato FFT route and
  both nonlinear engines may write a backend chunk directly into caller memory
  only while cumulative caller-visible output stays within
  `round(processed_real_input * to_rate / from_rate)`, where
  `processed_real_input` counts caller frames actually consumed by completed
  backend chunks — not caller input still queued in the FIFO and not drain pad
  zeros. The bounded per-chunk overflow tail spills into the preallocated
  output ring (never a new allocation) and is emitted only once later real
  input or finish authorizes it. Direct and staged complete streams stay
  bit-exact with the established rounded duration.
* Runtime environment switches must not select production resampler
  architecture. Temporary A/B switches are removed after measurements, and a
  changed architecture receives a new benchmark algorithm identifier.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| `channels == 0` | `ResamplerError::ZeroChannels` before backend construction |
| Either rate is zero | `ResamplerError::InvalidSampleRate { from_rate, to_rate }` before backend construction |
| Rubato input/output length is not divisible by channels | static backend error; no division, slice overrun, or panic |
| Backend consumed/produced frames exceed caller capacity | allocation-free `ProcessError::Backend` |
| A Rubato FIFO push exceeds fixed capacity | static backend error; no overwrite, resize, or log |
| A direct process route returns `N` frames | cumulative caller-visible `emitted` advances by exactly `N`; later drain excludes those frames |
| Pure-Rubato `working_buffer_bytes` with valid geometry | `Ok(0)` |
| Stereo SoXR `working_buffer_bytes` with valid geometry | `Ok(0)` because the native interleaved path owns no adapter PCM scratch |
| Non-stereo SoXR `working_buffer_bytes` with valid geometry | exact sum of deinterleave, per-channel output, and reinterleave scratch capacities |
| Exact-2x Linear High upsampling | one half-band engine; shared delay skip, emitted accounting, and drain lifecycle |
| UltraHigh at a common audio ratio | FFT engine with one sub-chunk (2x longer FIR than High) |
| Minimum/Maximum with reduced `up <= 16` | one interleaved spectral nonlinear engine |
| Minimum/Maximum with reduced `up > 16` and valid geometry | one interleaved contiguous polyphase engine |
| Pure-Rust Minimum/Maximum reduced ratio exceeds the nonlinear bound | `ResamplerError::RatioExceedsLimit { up, down, limit }`; no linear fallback |
| Checked working-buffer sizing overflows | `ResamplerError::CapacityOverflow { buffer }` |
| `Resampler::new` receives zero channels | `ResamplerError::ZeroChannels` before one-shot work |
| `Resampler::new` receives either rate zero | `ResamplerError::InvalidSampleRate { from_rate, to_rate }` |
| one-shot input length is not whole interleaved frames | `ResamplerError::InvalidBlock(AudioBlockError::IncompleteFrame { .. })` before the equal-rate bypass |
| `process_output_capacity_frames` arithmetic overflows | `ResamplerError::CapacityOverflow { buffer: "process output" }` |
| `process_output_capacity_frames` receives zero input frames | `Ok(0)` |
| offline finish-limit checked sum overflows | `TimingError::FrameCountOverflow` |
| Third-party backend rejects setup | `ResamplerError::BackendInitialization { backend, channel, message }` |
| Backend returns out-of-bounds progress or stalls | `InvalidBackendProgress` or `BackendStalled`, without parsing diagnostic text |

### 5. Good / Base / Bad Cases

* Good: stereo linear Rubato uses one two-channel engine, produces the same
  duration and channel samples as two independent mono reference engines within
  the measured floating-point bound, and allocates nothing after setup.
* Good: stereo SoXR uses one native interleaved stream, remains bit-exact with
  two independent mono reference streams, and owns no adapter PCM scratch.
* Good: 48-to-96 Linear/High uses the half-band engine while 48-to-96
  Linear/Standard remains FFT and Linear/UltraHigh uses the one-sub-chunk FFT.
* Good: 48-to-96 Minimum/Maximum stays spectral (`up = 2`), while 44.1-to-48
  and 48-to-44.1 use contiguous polyphase (`up = 160` and 147). Both preserve
  the shared finite tail and causal latency and allocate nothing after setup.
* Good: an integer-ratio direct-output process call and an output-constrained
  staged call produce bit-exact complete streams; finish emits only the shared
  remaining duration.
* Good: a wrapped two-chunk Rubato input ring still exposes the next complete
  backend chunk contiguously, while output wrap preserves sample order and
  allocation-free strict backpressure.
* Base: mono and non-stereo SoXR layouts use the independent-stream fallback;
  every channel must report identical consumed/produced progress before output
  is interleaved.
* Bad: report zero total setup memory because pure Rubato has zero adapter
  scratch; the engine still allocates internal tables and FIFOs during setup.
* Bad: retain a hidden environment variable that switches between mono and
  interleaved production paths, making benchmark identity and runtime behavior
  non-deterministic.
* Bad: bypass `emit_up_to` for direct output without advancing `emitted`; drain
  then regenerates already-returned frames and can degrade duration and alias
  measurements even though the process block looked correct.
* Bad: reuse an overwrite-on-full/logging pipeline ring for Rubato staging, or
  restore per-chunk `copy_within` prefix shifts after the measured ring layout.
* Bad: restore an infallible `Resampler::new`, sample/mixed-unit capacity
  helpers, or a `Standard`/`High` SoXR recipe alias after the distinct mapping
  is pinned.

### 6. Tests Required

* Build and test both backend selections: the default (pure-Rust rubato) plus
  `--no-default-features --features rubato`, and the opt-in SoXR path via
  `--all-features` or `--features soxr`.
* For SoXR, compare the native stereo stream with independent mono references
  bit-for-bit, assert one backend and zero adapter working bytes for stereo,
  retain non-stereo fallback progress checks, and cover arbitrary chunks,
  terminal drain, reset/fresh equivalence, and process/finish no-allocation.
* Compare one native multichannel Rubato engine against independent mono
  engines for High FFT, exact-2x High half-band, and UltraHigh one-sub-chunk
  FFT. Assert
  equal output lengths and a per-sample bound no weaker than `1e-14` for the
  current `f64` engines.
* For half-band, compare block output against an independent full zero-stuffed
  convolution oracle, assert representative passband/image limits, and prove
  the setup-selected vector accumulator is bit-equal to scalar.
* For nonlinear routing, assert the `up = 16` boundary, both 44.1/48
  directions, the 48/96 retained spectral route, and pathological rejection.
  Compare both engines with the test-only polyphase oracle below `1e-9`, assert
  timing equality, and prove the setup-selected stereo AVX2 dot kernel is
  bit-equal to scalar for vector and remainder lengths.
* Keep random input chunking, short/long duration, impulse alignment, terminal
  drain/reset, and process/finish no-allocation coverage.
* Ring tests cross wrap boundaries, assert exact sample order, prove every
  two-chunk front is contiguous, and wrap push/pop inside `assert_no_alloc`.
* For each direct-to-caller optimization, force the same complete stream
  through a constrained staged-output route and assert equal length plus
  bit-exact samples for representative upsampling and downsampling engines.
* Assert `working_buffer_bytes` is zero for stereo SoXR and pure Rubato, and
  equals compiled adapter capacities for the SoXR mono fallback. Measure total
  setup memory instead of adding opaque engine estimates.
* Assert the Gate-7 facade contracts: `Resampler::new` rejects zero
  channels/rates, one-shot input rejects a trailing partial frame before the
  equal-rate bypass, equal-rate valid and empty input stay identity, capacity
  is the checked exact frame-domain formula (`process_capacity_*` tests),
  every public SoXR tier resolves to a distinct recipe, and the offline render
  finish bound includes nonlinear latency plus finite tail
  (`resampler_finish_bound_includes_nonlinear_latency_and_tail`).
* Run quality, output-render, and streaming benchmarks after a channel
  architecture change; update the streaming algorithm identifier so stale
  baselines cannot compare as the same implementation. The hybrid route uses
  `matrix_process_checked_v4_nonlinear_polyphase_up16` in the matrix probe.

### 7. Wrong vs Correct

#### Wrong

```rust
// Duplicates Rubato's sinc table/FFT plan and adapter channel-copy work.
let backends = (0..channels)
    .map(|_| MonoBackend::new(from_rate, to_rate, phase, quality))
    .collect::<Result<Vec<_>, _>>()?;
```

#### Correct

```rust
// One native interleaved engine shares immutable resampling structures.
let backends = vec![MonoBackend::new_interleaved(
    from_rate, to_rate, phase, quality, channels,
)?];
```

#### Wrong

```rust
let direct = process_chunk_into(output)?;
produced += direct; // drain still believes these caller-visible frames remain
```

#### Correct

```rust
let direct = process_chunk_into(output)?;
self.emitted += direct as u64;
produced += direct;
```

#### Wrong

```rust
out_fifo.extend_from_slice(new_samples);
out_fifo.copy_within(consumed_samples.., 0); // shifts every queued prefix
```

#### Correct

```rust
out_fifo.push(new_samples)?;          // fixed capacity, never overwrites
let copied = out_fifo.pop_into(output); // at most two bounded copies
```

## Scenario: Dynamic Convolver Publication And Reclamation

### 1. Scope / Trigger

Apply this scenario when an impulse response can change while a
`StreamingProcessor`, `DspChain`, callback chain, or offline render chain is
alive. It replaces the former unreachable `disposal_slot()` composition path.

### 2. Signatures

```rust
FFTConvolver::new(ir_data: &[f64], channels: usize)
    -> Result<FFTConvolver, ProcessError>
ConvolverControl::new(enabled: bool) -> ConvolverControl
ConvolverControl::publish_at_rate(&self, kernel: FFTConvolver, sample_rate_hz: u32)
    -> Result<u64, ProcessError>
ConvolverControl::reclaim_retired(&self) -> bool
ConvolverControl::status(&self) -> ConvolverStatus
ConvolverControl::is_quiescent(&self) -> bool
ConvolverProcessor::new(control: ConvolverControl)
    -> Result<ConvolverProcessor, ProcessError>
OutputChainBuilder::convolver_control(&self) -> ConvolverControl
OutputChainBuilder::build_callback_chain(&self) -> Result<DspChain, ProcessError>
OutputChainBuilder::build_render_chain(&self, source_sample_rate_hz: u32)
    -> Result<OutputRenderChain, ProcessError>
```

### 3. Contracts

* A control handle is cloneable for control-side publishers, but a private CAS
  lease permits exactly one live audio consumer. Direct processor, callback,
  and offline-render construction all acquire the same lease. The lease is
  released after any later construction failure and when the consumer drops.
* `publish` accepts the kernel by value. It serializes concurrent control-side
  publish/reclaim calls, assigns install-order generations, opportunistically
  drains the retired slot, and is latest-wins until audio withdraws a value.
  The control-only serialization gate is never acquired by audio.
* Every publication carries a non-zero sample-rate domain. `publish_at_rate`
  is the explicit API; legacy `publish` stamps the documented default rate.
  Audio adopts only a kernel whose stamp matches its active stream rate.
* Publication and retirement use two fixed `AtomicPtr` ownership slots. The
  control side converts `Box` values to/from raw pointers; audio withdraws into
  unique `AudioOwned` values and only performs a fixed exchange/CAS. Audio
  never allocates, deallocates, scans a thread registry, or adjusts an `Arc`
  reference count for a heavy kernel.
* Audio ownership is fixed and bounded: `owned`, `incoming`,
  `pending_retire`, and one `retired` hand-off slot. A full retirement slot
  keeps the old kernel active and defers adoption without dropping either
  value on audio.
* A full retirement path keeps the current kernel processing, leaves the new
  adoption pending, and reports `backpressured` plus
  `pending_reclamations` through the allocation-free status snapshot.
* The first `finish` call locks the current kernel generation and exact
  `ir_length - 1` remaining frames. A later disable records control intent but
  cannot retire that kernel until the promised tail reaches terminal
  `Finished`; repeated terminal finish calls then progress disabled retirement.
* A sample-rate change immediately stops using and resets old-rate signal
  history. If no matching publication exists, processing is a dry bypass and
  telemetry reports `waiting_for_sample_rate_hz`. A later matching kernel is
  adopted only at a block boundary.
* Same-rate dry/kernel activation and enable/disable transitions use a
  complementary smoothstep over `ceil(0.005 * sample_rate_hz)` frames while
  executing at most one convolution kernel. A rate boundary never runs an
  old-rate kernel merely to finish a fade.
* `latest_published_generation` and `audio_drained_generation` form the
  authoritative acknowledgement. `ConvolverStatus` is eventually-consistent
  telemetry only. `ConvolverControl::is_quiescent()` runs under the control
  gate, requires disabled plus equal generations, and checks both ownership
  slots again after the generation read to close the retirement/acknowledgement
  TOCTOU window. Publishers must stop before this check and remain stopped.
* Dynamic convolver timing remains zero algorithmic latency and a finite
  `ir_length - 1` tail in the current sample-rate domain; reset preserves the
  adopted control/kernel configuration while clearing signal history.
* `ConvolverProcessor` teardown is off-RT. Drop releases all local ownership
  before acknowledging the drained generation, so a disabled control can
  reach authoritative quiescence without a stale generation.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| More than one live direct/callback/render consumer uses one control | `ProcessError::ConsumerAlreadyActive { processor: "Convolver" }` |
| IR geometry is empty, zero-channel, or incomplete | `FFTConvolver::new` returns a typed error |
| `publish_at_rate` receives zero Hz | `ProcessError::InvalidSampleRate` |
| Published rate differs from active rate | do not process it; retire/defer and report the awaited rate |
| Control publishes before audio withdraws | older publication is superseded on control and counted |
| Retirement slot is full | normal processing continues; adoption is deferred and status is backpressured |
| Control reclaims a retired slot | next block/finish retries the pending hand-off without waiting |
| Disable occurs after a partial finish | the locked generation emits every remaining frame, with no backend error or truncation |
| Disabled terminal processor still owns a kernel | repeated `finish` calls advance retirement; no `NeedInput` is returned |
| Build fails after acquiring the consumer lease | the partial chain drops and a later consumer can acquire the same control |
| Telemetry reports idle during a concurrent update | no teardown decision is made from `status()`; call `control.is_quiescent()` |
| Audio stores retired ownership while acknowledging drained | the final slot recheck makes `is_quiescent()` return `false` |
| `reclaim_retired` called with no retired value | returns `false`, no state change |

### 5. Good / Base / Bad Cases

* Good: a builder caller retains `builder.convolver_control()`, publishes by
  value after callback chain type erasure, and periodically reclaims retired
  kernels off the callback thread.
* Base: a burst of publications coalesces to the latest withdrawn generation
  while status counters remain bounded and auditable.
* Good: a partial finish continues through a concurrent disable, then a
  repeated terminal finish retires the locked kernel to the control side.
* Good: a 48 kHz kernel is retired at a 96 kHz boundary, dry audio continues,
  and a later 96 kHz publication fades in over exactly 480 frames.
* Bad: retaining a strong kernel `Arc`, using ArcSwap for the ownership slot,
  or treating `status().audio_idle` as teardown authority can move destruction
  to audio or admit a stale lifecycle decision.

### 6. Tests Required

* `convolver_control_stress_remains_bounded_and_adopts_latest_generation`:
  10,000 updates, burst coalescing, saturation/recovery, disable/reenable,
  final generation and counter invariants.
* `convolver_control_serializes_concurrent_publishers`: ordered generations
  and latest install under concurrent control clones.
* `convolver_kernels_are_destroyed_by_control_not_audio_thread`: replaced and
  retired destructor probes never fire on the audio thread.
* `first_process_on_a_new_audio_thread_is_allocation_free` and
  `terminal_finish_and_retirement_are_allocation_free_on_new_audio_thread`:
  first-use adoption/finish/retirement do not rely on same-thread prewarming.
* `partitioned_adoption_process_and_finish_are_allocation_free_on_new_audio_thread`:
  a long-IR kernel is adopted, processed, drained for exactly `ir_length - 1`
  frames, and reaches stable terminal state without callback allocation.
* `partitioned_fade_reversal_and_finish_match_direct_convolution_oracle`:
  long-IR enable reversal, irregular finish chunks, exact length, and every
  output sample match a direct convolution plus analytic smoothstep oracle.
* `consumer_lease_rejects_second_direct_consumer_and_releases_on_drop` plus
  output-chain entry tests: direct, callback, and render conflicts return the
  same typed error, and failed construction/drop releases the lease.
* `disable_during_partial_finish_preserves_locked_tail`: every declared tail
  frame survives a mid-finish disable and terminal finish is idempotent.
* `publication_during_idle_ack_cannot_commit_a_stale_generation` and
  `quiescence_rechecks_retirement_after_generation_acknowledgement`: stale
  acknowledgement and slot-check TOCTOU interleavings cannot authorize
  teardown.
* `convolver_processor_kernel_swap_is_allocation_free_on_audio_side` and
  `convolver_terminal_finish_can_retire_to_control_quiescence`: adoption,
  retirement, backpressure, finish, and recovery under `assert_no_alloc`.
* Direct nested-loop oracle tests cover mono/stereo, overlap-save/partitioned,
  irregular chunks, exact tail, stable `Finished(0)`, and reset isolation.

### 7. Wrong vs Correct

#### Wrong

```rust
let control = ConvolverControl::new(true);
let first = ConvolverProcessor::new(control.clone())?;
let second = ConvolverProcessor::new(control.clone())?; // invalid second consumer
let idle = control.status().audio_idle; // telemetry is not teardown authority
```

#### Correct

```rust
let builder = OutputChainBuilder::new(params);
let control = builder.convolver_control();
let mut chain = builder.build_callback_chain()?;
let kernel = FFTConvolver::new(&ir, channels)?;
control.publish_at_rate(kernel, sample_rate_hz)?; // control thread owns allocation
chain.process(samples, channels)?; // audio thread, fixed ownership hand-off
control.set_enabled(false);
// Drive process/repeated finish until ownership is returned.
control.reclaim_retired();          // control/offline thread destroys kernel
if control.is_quiescent() {
    drop(chain);                     // non-realtime teardown
}
```

---

## Scenario: Callback Playback Facade Ownership

### 1. Scope / Trigger

Apply this scenario when changing anything in `src/pipeline.rs` reachable from
`PlaybackPipeline`, `PlaybackBuilder`, `PlaybackController`, `PlaybackConfig`,
or `PlaybackParameters`. This is the crate's highest-level recommended API, and
before this section its contract lived only in rustdoc and the changelog, so a
well-intended spec-driven edit could have regressed it.

### 2. Signatures

```rust
CallbackSpec::stereo(sample_rate_hz: u32, block_frames: usize)
    -> Result<CallbackSpec, ProcessError>
PlaybackPipeline::builder(spec: CallbackSpec) -> PlaybackBuilder
PlaybackBuilder::configure(self, config: PlaybackConfig) -> PlaybackBuilder
PlaybackBuilder::build(self)
    -> Result<(PlaybackPipeline, PlaybackController), ProcessError>
PlaybackPipeline::process(&mut self, samples: &mut [f64])
    -> Result<ProcessProgress, ProcessError>
PlaybackPipeline::lifecycle_state(&self) -> PlaybackLifecycleState
PlaybackController::parameters(&self) -> PlaybackParameters
PlaybackController::request_reset(&self) -> u64
PlaybackController::request_drain(&self) -> u64
PlaybackController::request_stop_with_fade(&self, fade_ms: u32)
    -> Result<u64, ProcessError>
PlaybackController::lifecycle_status(&self) -> PlaybackLifecycleStatus
PlaybackController::load_impulse_response(&self, interleaved_ir: &[f64])
    -> Result<u64, ProcessError>
```

### 3. Contracts

* `PlaybackPipeline::process` is a realtime callback entry point. Every hot-path
  prohibition in `realtime-safety.md` applies to it and to everything it calls:
  no allocation, lock, `log::*`, IO, panic, or unbounded work. `pipeline.rs` is
  on the forbidden list in `logging-guidelines.md` for this reason, even though
  the builder/controller half of the same file is control-thread code.
* Control authority is split and each control has exactly one owner.
  `PlaybackController` is non-cloneable and owns only the two things that cannot
  be shared: the private single-consumer convolver lease and the lifecycle
  request channel. Every ordinary DSP control belongs to the cloneable
  `PlaybackParameters` from `PlaybackController::parameters`. Do not re-add
  convenience proxies for ordinary controls onto the controller; a control
  reachable through two handles has no owner.
* Lifecycle requests cross the boundary through one packed atomic word carrying
  request kind, fade payload, and generation together, so a request is never
  observed half-applied. Requests coalesce at callback block boundaries; the
  returned generation plus `lifecycle_status` is the acknowledgement, not a
  side-channel flag.
* The drain tail is bounded by the `ChainFinishPolicy` fixed at build time.
  `PlaybackConfig::validate` rejects a policy that cannot bound a tail, so an
  invalid preset fails at build rather than inside the first callback drain.
  `DspChain::finish_with_policy` still validates, because a chain can also be
  driven directly.
* Build-time and runtime validation differ deliberately. `PlaybackBuilder::build`
  validates strictly and fails with `ProcessError::InvalidParameter`, so a bad
  preset or config file surfaces once at setup. Runtime `PlaybackParameters`
  writes clamp a finite out-of-range value and reject a non-finite one, because
  a callback-adjacent publisher must not fail a UI interaction. Both layers use
  the published ranges and the shared `sanitized` policy in `lockfree_params.rs`
  rather than re-encoding bounds.
* An idle pipeline writes silence instead of returning an error; that is the one
  documented exception to "a stage that produces no output is a failure".
  Reaching `PlaybackLifecycleState::Idle` is a normal terminal state that
  `request_reset` re-arms.
* Impulse-response adoption follows the convolver scenario above:
  `load_impulse_response` prepares the kernel on the control thread and publishes
  it in the controller's callback rate domain; audio adopts it at a block
  boundary without allocating.

### 4. Tests Required

* Build rejects each invalid drain-policy shape and accepts a narrow valid one.
* A bypassed/disabled facade config matches the corresponding core defaults
  rather than an independently written literal.
* Each paired control publishes as one coherent snapshot, with a single
  publication per call.
* Fade → drain → `Idle` → `request_reset` → processing resumes, driven only
  through `process` and the lifecycle channel.
