# Playback facade robustness and lifecycle command channel

## Goal

Close the gaps that block real product integration of the public playback
facade (`CallbackSpec` / `PlaybackConfig` / `PlaybackBuilder` /
`PlaybackController` / `PlaybackParameters` / `PlaybackPipeline`, added in
`83753ce`): lifecycle operations are unreachable in the recommended ownership
model, non-finite control input permanently poisons DSP state, several value
contracts silently disagree with what the API reports back, and two public
setters exist only to return an error.

## What I already know (measured on 2026-07-28)

- Baseline is green: `cargo fmt --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, and `cargo test --all-features`
  (368 + 20 + 25 + 3 + 6 tests) all pass.
- Marker traits are already correct: `PlaybackPipeline` and
  `PlaybackController` are `Send`, `PlaybackParameters` is `Send + Sync`,
  `PlaybackPipeline` is not `Clone`.
- The callback path really is allocation-free under load. A temporary probe
  (max-capacity block, every stage enabled, 4x oversampled saturation, 8192-tap
  IR adoption plus a superseding hot swap) passed inside `assert_no_alloc`, and
  a deliberate canary allocation proved the guard fires. That probe is **not**
  in the repo; the only committed no-allocation test uses a 1-frame transparent
  block (`src/pipeline.rs:1251`).
- `PlaybackPipeline::reset()` is also allocation-free on the audio thread with
  every stage enabled and a 16384-tap partitioned convolver loaded (measured
  the same way). An in-callback reset therefore does not violate
  `realtime-safety.md`; its cost is bounded by preallocated state size.
- **Lifecycle unreachable**: `process`, `finish_into_with_policy`, and `reset`
  all take `&mut self` (`src/pipeline.rs:761`, `:770`, `:782`). The pipeline
  must be moved into the audio callback, so a control thread can never obtain
  `&mut` to drain or reset. Track change / seek therefore requires rebuilding
  the whole pipeline (re-allocating, re-loading the IR) or an app-supplied
  atomic flag consumed inside the callback.
- **NaN poisoning** (measured): `controller.set_volume(f64::NAN)` produces NaN
  output and `parameters.volume()` reads NaN back;
  `PlaybackConfig::with_eq([f64::NAN; 10])` produces NaN output that persists
  after writing sane 0 dB gains, because the IIR history is already NaN. Root
  cause: `f64::clamp` returns NaN for NaN input
  (`src/processor/lockfree_params.rs:884`, `:417`, and siblings).
- **Silent clamping / readback disagreement** (measured):
  `with_volume(2.0)` silently becomes `1.0` with no documented range;
  `set_eq(true, [40.0; 10])` reads back 40 dB but only 15 dB is applied
  (`AtomicEqParams::write` at `lockfree_params.rs:397` does not clamp, while
  `set_band_gain` at `:417` and `Equalizer::set_band_gain` at `eq.rs:115` do);
  `set_dynamic_loudness(true, +6.0, _)` reads back `0.0` dB because the value is
  clamped in the linear domain, and `-inf` round-trips as `-inf`.
- **Always-failing setters**: `PlaybackParameters::set_saturation_enabled` and
  `set_saturation_drive` unconditionally return
  `ProcessError::UnsupportedOperation` (`src/pipeline.rs:402-418`) — even
  though the layer below already separates `armed` (fixed-latency arming,
  changeable only before the stream starts) from `enabled` (runtime soft bypass
  that preserves delay and history): `adapters.rs:376-378`, `:395`, `:431`.
- Device callbacks keep firing after a stream ends, but the current contract
  makes `process` return `ProcessError::AlreadyFinished` after a terminal
  finish (`streaming-lifecycle.md` §3). The facade must define an idle
  behaviour instead of forcing products to swallow errors in the callback.
- `ChainFinishPolicy` is a small `Copy` value (`dsp_chain.rs:44`), so a drain
  policy can be fixed at build time and needs no cross-thread payload channel.
- Test-matrix gaps versus `.trellis/spec/backend/streaming-lifecycle.md` §6 and
  the prior task's research matrix: no irregular chunk-size equivalence, no
  reset isolation assertion (the existing test only proves `process` works
  again after `reset`), no full-capacity/all-stage no-allocation coverage, and
  `finish_was_capped()` (`src/pipeline.rs:737`) is public but untested.

## Decisions (ADR-lite)

### 1. Lifecycle command channel: reset + drain requests

**Context**: `&mut self` lifecycle methods are unreachable once the pipeline is
moved into the audio callback; products cannot change tracks without tearing
down and rebuilding the pipeline.
**Decision**: `PlaybackController` publishes lock-free requests
(`request_reset`, `request_drain`, `request_stop_with_fade`);
`PlaybackPipeline::process` consumes them at a block boundary. `reset` clears
state immediately. `drain` renders the remaining tail into the current and
following output blocks until terminal. The drain policy is fixed at build time
through `PlaybackConfig` so the request itself carries no payload. The control
thread observes progress through a controller-side lifecycle status carrying
applied-request generations. `finish_into_with_policy` remains the
offline/non-callback drain path.
**Consequences**: an in-callback reset costs one bounded state-clearing spike
(measured allocation-free); the facade grows a small state machine that must be
covered by request-ordering and edge-case tests.

### 2. Idle semantics after a terminal drain

**Context**: the device callback keeps calling `process` after the stream ends.
**Decision**: after a terminal in-callback drain the pipeline enters an idle
state where `process` writes silence and returns `Ok` with an explicit
`Finished`/idle indication, instead of returning `AlreadyFinished`. Reset (or a
new request) re-arms it. The low-level `StreamingProcessor` contract is
unchanged; this is a facade-level state.
**Consequences**: the facade's states must be documented and tested; the typed
`AlreadyFinished` error stays reachable through the low-level API.

### 3. Non-finite defence is two-layered

**Context**: `f64::clamp` passes NaN through, permanently poisoning IIR state.
**Decision**: `lockfree_params` sanitizes centrally — a non-finite write is not
applied and the previous snapshot survives — so advanced `Atomic*Params` /
`OutputChainBuilder` users are protected too. On top of that the facade
(`PlaybackConfig` builders, `PlaybackParameters` setters,
`PlaybackBuilder::build`) rejects non-finite input with a typed `ProcessError`.
**Consequences**: affected facade setter signatures change from `()` to
`Result<(), ProcessError>` — a breaking change permitted by the crate's
documented 0.x policy; the CHANGELOG must call it out.

### 4. Out-of-range (finite) values: strict at build, clamped at runtime

**Context**: a bad config/preset should surface, but a UI slider must not fail
on float noise.
**Decision**: `PlaybackBuilder::build` rejects out-of-range `PlaybackConfig`
values with a typed error. Runtime `PlaybackParameters` setters keep clamping,
every control-thread reader returns the value actually in effect, and the valid
ranges are exported as documented constants for UI bounds. `AtomicEqParams::write`
is fixed so the stored gains match the applied ±15 dB clamp.
**Consequences**: readback becomes trustworthy; one low-level write path
changes behaviour (previously it stored unclamped gains that were never
applied).

### 5. Callback volume stays attenuation-only (0.0–1.0)

**Context**: `AtomicVolumeParams` clamps to 0.0–1.0 with no documentation.
**Decision**: keep the range and document it. Products needing positive gain
(ReplayGain compensation, pre-amp) apply it upstream or through the existing
loudness-normalization path.
**Consequences**: no DSP behaviour change; the constraint becomes explicit.

### 6. Saturation gains real runtime control

**Context**: two public setters exist only to return `UnsupportedOperation`,
while the adapter already supports runtime soft bypass and parameter changes.
**Decision**: arming (which establishes fixed latency) stays a build-time
decision. Once armed, drive/threshold/mix/type/gains and soft enable/disable
become real runtime operations that preserve latency and history. Calling them
on a non-armed pipeline returns a typed error explaining that an armed build is
required.
**Consequences**: products can build armed with `mix = 0` and automate live; the
latency contract is unchanged.

### 7. Fade-out stop and a gapless extension point

**Context**: switching tracks by resetting mid-signal clicks; gapless playback
is the obvious next step.
**Decision**: `request_stop_with_fade(fade_ms)` runs a callback-side gain ramp
before entering drain/reset, so the fade-then-switch sequencing is not left to
each product. Additionally the lifecycle status and request types reserve a
gapless / next-stream-preroll shape (types, status bits, `#[non_exhaustive]`)
without implementing it in this task.
**Consequences**: more state and timing tests now, but no breaking change when
gapless lands.

## Requirements

- A control thread can request `reset`, `drain`, and `stop with fade` while the
  pipeline lives in the audio callback, with no locks, allocation, logging, or
  unbounded work on the callback path.
- Requests take effect at a block boundary; the control thread can observe
  whether a request has been applied without blocking or busy-waiting.
- Request handling has defined behaviour for: two requests arriving in the same
  block, a request arriving before the first `process`, a request arriving
  after the stream is already terminal, and a `PlaybackController` dropped
  while the pipeline keeps running.
- After a terminal in-callback drain, `process` outputs silence and returns
  `Ok` with an idle/finished indication; `reset` re-arms it.
- Non-finite values cannot reach DSP state through any facade path, and cannot
  be stored by the low-level atomic parameter types either.
- `PlaybackBuilder::build` rejects out-of-range configuration with typed
  errors; runtime setters clamp; every reader returns the effective value.
- Valid ranges are exported as documented public constants.
- Saturation drive/threshold/mix/type/gains and soft enable/disable work at
  runtime on an armed pipeline and return a typed error otherwise.
- `request_stop_with_fade(fade_ms)` applies a callback-side gain ramp before
  the terminal transition.
- Lifecycle status/request types are `#[non_exhaustive]` and reserve the
  gapless/next-stream shape.
- Tests cover: full-capacity all-stage no-allocation (process, finish, reset,
  and request handling), irregular chunk-size equivalence, reset isolation
  (no previous-stream history leaks), `finish_was_capped`, request ordering and
  edge cases, NaN rejection at both layers, readback-equals-applied for every
  parameter, and runtime saturation control.
- README / lib.rs docs / CHANGELOG updated, including the breaking setter
  signature changes and the documented ranges.

## Acceptance Criteria

- [x] `request_reset` / `request_drain` / `request_stop_with_fade` take effect
      at a block boundary and are proven allocation-free on a fresh thread with
      every stage enabled and a large IR loaded.
- [x] After a terminal in-callback drain, `process` returns `Ok` with silence
      and an idle indication; `reset` re-arms; the control thread can read the
      applied lifecycle state.
- [x] Request edge cases are tested: same-block collision, pre-first-process
      request, post-terminal request, controller dropped before pipeline.
- [x] `NaN` / `±inf` through any facade config or parameter path cannot alter
      audio output or poison filter state, and `Atomic*Params` refuse to store
      non-finite values.
- [x] `PlaybackBuilder::build` returns typed errors for out-of-range config;
      runtime setters clamp; every reader equals the applied value, including
      EQ presets written through `set_eq`.
- [x] Public range constants exist and are referenced from the docs.
- [x] Runtime saturation control works on an armed pipeline (latency unchanged,
      history preserved) and returns a typed error on a non-armed one.
- [x] Fade-out stop produces a monotonic ramp to silence before the terminal
      transition, verified numerically.
- [x] New tests: full-capacity all-stage no-allocation, chunk-size equivalence,
      reset isolation, `finish_was_capped`.
- [x] `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D
      warnings`, and `cargo test --all-features` pass.
- [x] README, crate-level docs, and CHANGELOG record the new lifecycle model,
      documented ranges, and breaking signature changes.

## Definition of Done (team quality bar)

- Tests added/updated (unit/integration where appropriate)
- Lint / typecheck / CI green
- Docs/notes updated if behavior changes
- Rollout/rollback considered if risky

## Out of Scope (explicit)

- Product-ergonomics work tracked in a separate follow-up task:
  `PlaybackConfig` serde/getters, preset application
  (`PlaybackParameters::apply(&PlaybackConfig)`), `f32` device-boundary
  helpers, a public playback example plus public-surface integration test,
  `RingBuffer` disposition, and the README convolution contradiction.
- Implementing gapless / next-stream preroll (only the extension shape is
  reserved here).
- Audio-device management, decode ownership, queue/state machine.
- Changing the canonical stage order or `OutputChainBuilder` composition.
- Raising the callback volume ceiling above 1.0.

## Findings discovered during implementation

- **Dynamic loudness is block-size dependent (out of scope, needs follow-up).**
  `DynamicLoudness::process` (`src/processor/dynamic_loudness.rs:619-642`)
  restarts its `BLOCK_SIZE` gain-smoothing chunking at every call, so the
  smoother steps once per caller-supplied block. Measured on the facade: the
  same 512-frame input split as `1 + 7 + 64 + 100 + 128 + 212` frames differs
  from a single 512-frame call by ~1.3e-3 relative at the start, and the
  divergence decays only very slowly (3.7e-4 after 24 blocks, 1.3e-4 after 400
  blocks of the same signal). Every other callback stage is bit-exact across
  irregular chunking, which the new
  `callback_output_is_identical_across_irregular_chunk_sizes` test asserts with
  dynamic loudness deliberately excluded. This violates the irregular-chunk
  equivalence requirement in `realtime-safety.md`, but fixing the smoother is a
  DSP-state change in an existing stage rather than facade work.

## Technical Notes

- `src/pipeline.rs` — the whole facade plus the legacy `RingBuffer`.
- `src/processor/lockfree_params.rs` — clamping and snapshot publication;
  `SharedParams` already provides a lock-free control→callback publication
  pattern that the lifecycle channel can reuse.
- `src/processor/dsp_chain.rs` — `process` / `finish_with_policy` / `reset`,
  `ChainFinishPolicy`.
- `src/processor/adapters.rs:376-431` — saturation `armed` vs `enabled`.
- `src/processor/dsp.rs:35-90` — `VolumeController` smoothing, reusable for the
  fade ramp.
- `.trellis/spec/backend/realtime-safety.md` — hot-path prohibitions.
- `.trellis/spec/backend/streaming-lifecycle.md` — lifecycle, validation
  matrix, required tests.
- Prior task `.trellis/tasks/07-26-playback-pipeline-api/` — the PRD and
  `research/playback-facade-design.md` this task builds on.

## Implementation Plan (small PRs)

- **PR1** — lifecycle command channel: request publication, block-boundary
  consumption, facade state machine (armed / draining / idle), lifecycle
  status, plus the request-ordering and edge-case tests and the strengthened
  no-allocation coverage.
- **PR2** — input contracts: two-layer non-finite defence, build-time range
  validation, runtime clamping with truthful readback, exported range
  constants, `AtomicEqParams::write` fix.
- **PR3** — saturation runtime control, fade-out stop, gapless extension shape.
- **PR4** — remaining test-matrix gaps (chunk equivalence, reset isolation,
  `finish_was_capped`) and docs/CHANGELOG.
