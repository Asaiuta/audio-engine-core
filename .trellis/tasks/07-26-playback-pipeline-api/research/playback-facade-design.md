# Playback Facade Design Research

## Decision

Do not stabilize the currently added `PlaybackFormat` / `PlaybackControls` /
`PlaybackPipelineBuilder` surface as-is. It is a useful prototype, but exposes
internal atomic implementation types and obscures important lifecycle and
single-consumer constraints.

## Evidence from this repository

### Existing architectural boundaries

- `OutputChainBuilder` is the one canonical callback/offline chain materializer;
  its macro manifest owns stage order. A facade must delegate to it rather than
  recreate stages. See `src/processor/output_chain.rs`.
- Callback processing is device/output-rate-domain only. Decode-side resampling
  belongs outside the callback facade; source/output rate fields are an
  implementation detail needed by the offline renderer.
- `DspChain` has explicit `process`, stateful finalization, `latency`, `tail`,
  bounded finish policy, reset, and capped-tail reporting. A facade that offers
  draining must surface or deliberately encapsulate those contracts.
- Atomic parameter snapshots are correct for control-to-callback transfer, but
  their ArcSwap/realtime-reader setup is control-thread-only. Raw atomic types
  are not appropriate as the primary high-level API.
- `ConvolverControl` admits exactly one simultaneous audio consumer. Cloning
  an aggregate control object must not imply it can construct another live
  callback/render pipeline.
- `ChannelLayout` is not currently a callback-DSP routing contract. A format
  containing only count/rate must not claim layout-aware semantics.

## External API patterns

- CPAL separates requested/resolved device configuration from callback buffer
  processing and stream ownership.
- NIH-plug separates initialization/preparation, reset, and a narrow process
  method, with persistent parameter identities distinct from processing state.
- FunDSP separates graph construction/allocation from runtime processing.

The transferable lifecycle is:

```text
configure -> resolve callback spec -> build/allocate -> activate
-> process blocks -> explicit drain/stop -> reset or drop
```

Only the process-block operation is realtime-safe.

## Public design target

```rust
pub struct CallbackSpec {
    pub sample_rate_hz: u32,
    pub channels: NonZeroUsize,
    pub max_frames: NonZeroUsize,
}

pub struct PlaybackConfig { /* stable, intent-level DSP settings */ }
pub struct PlaybackBuilder { /* control thread */ }
pub struct PlaybackController { /* control thread; clone only if safe */ }
pub struct PlaybackPipeline { /* callback-owned, !Clone */ }

impl PlaybackBuilder {
    pub fn new(spec: CallbackSpec) -> Self;
    pub fn configure(self, config: PlaybackConfig) -> Self;
    pub fn build(self) -> Result<(PlaybackPipeline, PlaybackController), PlaybackBuildError>;
}

impl PlaybackPipeline {
    pub fn spec(&self) -> CallbackSpec;
    pub fn timing(&self) -> PlaybackTiming;
    pub fn process(&mut self, samples: &mut [f64]) -> Result<ProcessProgress, ProcessError>;
    pub fn finish_into_with_policy(
        &mut self,
        output: &mut [f64],
        policy: ChainFinishPolicy,
    ) -> Result<ProcessProgress, ProcessError>;
    pub fn reset(&mut self) -> Result<(), ProcessError>;
}
```

`PlaybackConfig` should use stable value types such as `EqSettings`,
`LimiterSettings`, `CrossfeedSettings`, and `NoiseShapingSettings`; it should
not expose `Arc<Atomic*Params>`. `PlaybackController` should expose intention
methods (for example `set_volume`, `set_eq_band_gain`, `set_limiter_ceiling`)
and telemetry readers. It owns private snapshot handles.

## Required redesign decisions

1. Rename `PlaybackFormat` to `CallbackSpec`/`CallbackFormat`, or document
   unambiguously that it represents only already-converted callback audio.
2. Add `max_frames` before claiming complete callback preparation guarantees;
   this makes fixed allocation/capacity part of the public contract.
3. Remove `PlaybackPipeline::convolver_control()` from the high-level facade.
4. Do not derive `Clone` for a controls aggregate containing `ConvolverControl`.
   Pair one controller with one pipeline, or expose only explicitly safe clones.
5. Keep an advanced module/path for users who deliberately need raw
   `OutputChainBuilder` and atomic handles.
6. Forward timing/tail and make unknown-tail finishing policy explicit; never
   say an arbitrary output buffer is "correctly sized" without a sizing or
   bounded-policy contract.
7. Keep channel layout out of this API until there is an actual layout-aware
   routing/downmix policy.

## Test matrix before stabilization

- `process` and `finish` allocation-free on a fresh callback thread.
- Typed invalid geometry for process and finish, including zero-length behavior.
- Idempotent terminal finish, process-after-finish failure, reset isolation.
- Known latency/tail forwarding and bounded unknown-tail policy behavior.
- Default configuration is transparent/bypass-correct where intended.
- Controller updates become visible atomically at block boundaries and remain
  allocation-free during concurrent processing.
- Convolver single-consumer lease: duplicate construction fails typed; drop
  releases the lease.
- Documentation examples compile as doctests or integration tests.

## Sources

- Repository: `src/pipeline.rs`, `src/processor/output_chain.rs`,
  `src/processor/dsp_chain.rs`, `src/processor/lockfree_params.rs`,
  `src/channel_layout.rs`, and `.trellis/spec/backend/{realtime-safety,streaming-lifecycle}.md`.
- CPAL: https://docs.rs/cpal/latest/cpal/traits/trait.DeviceTrait.html
- NIH-plug Plugin lifecycle: https://nih-plug.robbertvanderhelm.nl/nih_plug/prelude/trait.Plugin.html
- NIH-plug parameter model: https://nih-plug.robbertvanderhelm.nl/nih_plug/params/trait.Param.html
- FunDSP AudioUnit: https://docs.rs/fundsp/latest/fundsp/audionode/trait.AudioUnit.html

External source URLs should be live-verified before treating exact upstream
signatures as normative; the research worker lacked browser tools.
