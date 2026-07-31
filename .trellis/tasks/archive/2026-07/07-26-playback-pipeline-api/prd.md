# Expose a high-level playback pipeline API

## Goal

Add an ergonomic, app-agnostic public facade that assembles the crate's existing decode, resampling, and DSP primitives into a recommended playback-processing path, while retaining low-level `StreamingProcessor` APIs for advanced integrations.

## What I already know

- The crate intentionally excludes device ownership, UI, queue/state-machine, media library, and application runtime concerns.
- `StreamingProcessor` plus `ProcessBuffers`/`ProcessProgress` provides the existing low-level realtime processing contract.
- `OutputChainBuilder` already materializes canonical callback and offline DSP chains from `OutputChainParams`.
- `StreamingDecoder` is the existing public streaming decoder.
- The former `AudioPipeline` worker was removed because it had incorrect backpressure semantics; a replacement must not recreate an unbounded/drop-oldest background pipeline.

## Assumptions (temporary)

- The first facade should be synchronous and caller-driven, with no owned worker thread, device integration, or ring-buffer policy.
- It should compose existing public types rather than duplicate DSP configuration.
- The new API should make format, lifecycle/drain, and timing behavior explicit.

## Open Questions

- None for the MVP: first release is a synchronous, caller-driven DSP facade over the existing output-chain builder. Decoder ownership remains separate.

## Requirements (evolving)

- Preserve access to the current low-level APIs under their existing advanced paths.
- Replace the prototype high-level controls surface with a stable split: `CallbackSpec` (validated callback-domain geometry/capacity), intent-level `PlaybackConfig`, callback-owned `PlaybackPipeline`, and control-thread `PlaybackController`.
- Add complete intent-level configuration for the canonical callback DSP stages: volume, EQ, limiter, saturation, crossfeed, dynamic loudness, and noise shaping. Config values use domain units and do not expose `Arc<Atomic*>` or `ConvolverControl`.
- Split control responsibilities: an exclusive `PlaybackController` owns lifecycle/single-consumer-sensitive authority, while a clonable, safe parameter-update handle can update ordinary atomic controls from UI/remote threads without exposing raw atomic types.
- Do not expose `Arc<Atomic*>` implementation handles or `ConvolverControl` through the high-level API.
- Make the callback-domain-only rate contract explicit; decoder ownership, decode-side resampling, device negotiation, queueing, and workers remain out of scope.
- `CallbackSpec` includes a nonzero `max_frames` capacity/prepare contract.
- Surface pipeline timing/tail data and make bounded drain policy explicit through the facade.
- Preserve canonical stage construction by delegating to `OutputChainBuilder`; do not recreate the manifest/stage order.
- Avoid allocation, locking, I/O, logging, or hidden work on the callback processing path.
- `PlaybackPipeline` must be callback-owned and non-cloneable; controller ownership/clone semantics must not suggest that a convolver control can back multiple simultaneous audio consumers.

## Acceptance Criteria (evolving)

- [ ] High-level public API is `CallbackSpec`, `PlaybackConfig`, `PlaybackBuilder`, exclusive `PlaybackController`, clonable safe parameter-update handle, and `PlaybackPipeline`; the prototype raw `PlaybackControls`/`PlaybackFormat` public surface is removed or intentionally deprecated before stabilization.
- [ ] `PlaybackConfig` offers documented intent-level configuration for all canonical callback DSP stages without raw atomics.
- [ ] `CallbackSpec` validates channels, sample rate, and maximum callback frames, and `PlaybackPipeline::process` rejects blocks exceeding that prepared capacity without allocating.
- [ ] `PlaybackConfig` uses documented, stable intent-level values and has an explicit transparent/default profile.
- [ ] The clonable parameter-update handle permits ordinary UI/remote parameter updates while neither it nor the exclusive controller expose `Arc<Atomic*>` or `ConvolverControl`; unsafe multi-consumer construction is not implied by clone semantics.
- [ ] `PlaybackPipeline` forwards timing/tail information and exposes an explicit bounded drain-policy operation while preserving typed `ProcessProgress` / `ProcessError`.
- [ ] Canonical callback stage construction still delegates to `OutputChainBuilder`.
- [ ] Unit tests cover spec validation and maximum frames, transparent defaults, every high-level effect configuration, cloned parameter updates including concurrent control/audio activity, timing/tail, terminal drain/reset, typed invalid geometry, convolver ownership, and fresh-thread no-allocation for process and finish.
- [ ] README/API documentation contains a compile-checked minimal integration example and states thread/lifecycle boundaries.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` pass.

## Definition of Done (team quality bar)

- Tests added/updated (unit/integration where appropriate)
- Lint / typecheck / CI green
- Docs/notes updated if behavior changes
- Rollout/rollback considered if risky

## Out of Scope (explicit)

- Audio-device management or CPAL/WASAPI integration
- Playback queue/state machine, UI, media-library, and server APIs
- Background decode threads and an owned ring-buffer policy
- Stabilizing every existing 0.1 API for a 1.0 release

## Technical Notes

- `src/pipeline.rs`: only `RingBuffer` remains; old worker pipeline was removed for broken backpressure.
- `src/processor/output_chain.rs`: `OutputChainBuilder` owns the canonical callback/offline DSP composition.
- `src/lib.rs`: current public re-export surface.
- `README.md`: declares the intended Decode → Resample → Loudness → DSP → Analyze → Stream model and callback constraints.
