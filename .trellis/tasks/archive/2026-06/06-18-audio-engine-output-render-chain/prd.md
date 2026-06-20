# Unified Output Render Chain

## Goal

Converge the offline quality benchmark, the realtime callback DSP chain, and the
export/render path onto a single canonical node-order definition so that the
"full output-chain true-peak" number measures the chain that actually runs, and
can eventually be promoted from report-only to an enforced gate.

## What I already know (from code inspection)

There are currently **three** divergent chains, not two:

1. `render_full_output_chain` (`benches/audio_quality_measurements.rs:1753`):
   `source f64 -> PeakLimiter(-1 dBTP) -> StreamingResampler(when needed) ->
   NoiseShaper(24-bit) -> f32`. **Missing** gain/volume, EQ, saturation,
   crossfeed, dynamic loudness — i.e. everything that runs *before* the limiter
   at runtime.
2. `build_chain_bundle` (`benches/audio_callback_chain_perf.rs:144`):
   `EQ -> Saturation -> Crossfeed -> Convolver -> Volume -> DynamicLoudness ->
   PeakLimiter`. Has no resampler; configures a NoiseShaper param set but never
   adds the noise shaper to the chain (line ~240); limiter is at the tail.
3. The real runtime `build_dsp_chain` lives in the **downstream Lyne app**, not
   in this crate (per the comment at `audio_callback_chain_perf.rs:139-143`).

Consequences:
- The report-only metric `full_output_chain_worst_true_peak`
  (`audio_quality_measurements.rs:822`) measures a chain that resembles neither
  the perf bench nor the runtime. Promoting it to a gate today would gate a
  fiction.
- Resampling is direction-dependent: playback resamples *before* the callback
  (background worker in `pipeline.rs`, source->device rate), while offline export
  resamples *after* DSP at an arbitrary target rate. A single fixed order cannot
  model both.
- The realtime callback path must stay allocation-free / lock-free / no-I/O
  (`.trellis/spec/backend/realtime-safety.md`); resampler + f32 quantization are
  not callback-stage operations.

## Scope decision

**Chosen: full proposal** — a single `OutputRenderChain` is the canonical output
path, and the offline bench, perf bench, and (downstream) realtime callback all
derive from one shared node-order definition. Resolved scope boundaries:

- **Callback convergence** = this crate exports the authoritative builder (one
  shared order table producing either an offline `OutputRenderChain` or the
  realtime `DspChain`) plus a node-order snapshot API. The downstream Lyne
  callback is changed to call it in a separate cross-repo task; this task only
  guarantees the crate-side single source of truth.
- **Limiter** = single limiter (reuse the existing `PeakLimiter` / the true-peak
  work), placed after DSP and before quantization. No source+final split — that
  avoids a behavior change and zero-overlaps the in-flight
  `06-12-audio-engine-true-peak-limiter` task.

Because the scope still spans realtime safety and gate promotion, it is
decomposed into sequenced subtasks (see Implementation Plan) so each change is
independently reviewable.

## Assumptions (temporary — validate)

- This crate owns the canonical chain definition + builder + offline renderer.
  The runtime callback assembly currently lives in the downstream Lyne app; this
  task must decide how that convergence is represented here (cross-repo question
  below).
- The "source limiter + final safety limiter" split is a real behavior change
  that overlaps the in-flight `06-12-audio-engine-true-peak-limiter` task; the
  two must be reconciled, not duplicated.

## Requirements

1. **Canonical node-order definition** — one place declares the output chain
   order: `Volume/Gain -> EQ -> Saturation -> Crossfeed -> Convolver ->
   DynamicLoudness -> PeakLimiter -> (Resampler) -> NoiseShaper/Quantize ->
   (Meter)`. This is the single source of truth; nothing else hard-codes order.
   (Confirm the pre-limiter order against the downstream Lyne `build_dsp_chain`
   during implementation — the perf bench order is the current best proxy.)
2. **Shared builder** — a builder consumes the order definition and produces
   either (a) an offline `OutputRenderChain` (includes resampler + quantization +
   meter stages) or (b) the realtime `DspChain` (callback-safe stages only;
   resampler/quantization excluded because those run off-callback). Resampler
   position is configurable per direction (export = after DSP; playback = handled
   off-callback upstream).
3. **Offline renderer parity** — `render_full_output_chain` is rebuilt on the
   shared builder and gains the missing pre-limiter stages (Volume/EQ/Saturation/
   Crossfeed/DynamicLoudness).
4. **Both benches consume the builder** — `audio_quality_measurements` and
   `audio_callback_chain_perf` build their chains from the shared definition.
   Fix the perf bench's configured-but-never-added NoiseShaper.
5. **Single limiter** — reuse the existing `PeakLimiter`; no source+final split.
6. **Node-order parity test** — a test fails if the offline-measured node list
   diverges from the realtime `DspChain` node list (intersection of shared
   stages).
7. **Per-sample equivalence test** — assert `OutputRenderChain` and the realtime
   `DspChain` produce sample-identical output for the shared stages on a fixed
   input (off-callback stages — resampler, quantization — conditionally excluded).
8. **Latency/state metadata** — the order definition records, per stage, whether
   it carries cross-buffer state / introduces latency (e.g. limiter lookahead,
   resampler), reserved for future latency reporting.
9. **Realtime safety preserved** — callback-direction builder output stays
   allocation-free / lock-free / no-I/O per `realtime-safety.md`.

## Acceptance Criteria

- [x] A single canonical node-order definition exists; grep shows no other
      hard-coded ordering of these stages in benches.
- [x] The shared builder produces both the offline `OutputRenderChain` and the
      realtime `DspChain` from that one definition.
- [x] `render_full_output_chain` includes the pre-limiter DSP stages in runtime
      order and is built via the shared builder.
- [x] Both benches build their chains from the shared builder; perf bench's
      NoiseShaper is actually in the chain.
- [x] Node-order parity test fails on a deliberately reordered chain.
- [x] Per-sample equivalence test passes for shared stages on a fixed fixture.
- [x] Per-stage latency/state metadata is queryable from the order definition.
- [x] Realtime-safety tests (no steady-state alloc in callback stages) still pass.

## Definition of Done

- Tests added/updated (parity, per-sample equivalence, RT-safety, render unit).
- `cargo test`, `cargo check --benches`,
  `cargo bench --bench audio_quality_measurements -- --quick`,
  `cargo bench --bench audio_callback_chain_perf -- --quick` green.
- Realtime-safety invariants preserved.
- README full-output-chain wording updated only if measured behavior changes.

## Technical Approach

Add a chain-definition module in this crate (e.g.
`src/processor/output_chain.rs`) exporting: an ordered stage descriptor list
(stage id + latency/state flags), a builder that materializes either an
`OutputRenderChain` (offline: DSP + resampler + quantize + meter) or a realtime
`DspChain` (callback stages only), and a node-order snapshot accessor for tests
and the downstream Lyne callback. Benches and the downstream callback all derive
from this one definition. Limiter stays single (`PeakLimiter`). Gate promotion
of `full_output_chain_worst_true_peak` is deferred.

## Decision (ADR-lite)

**Context**: Three divergent chains (offline quality bench, perf bench, downstream
runtime callback) mean the "full output-chain true-peak" metric measures a path
that matches neither the perf bench nor the real callback, so it cannot honestly
become a gate.

**Decision**: Build one canonical node-order definition + shared builder in this
crate; converge both benches and the offline renderer onto it; expose a builder
+ node-order snapshot for the downstream Lyne callback to adopt separately. Keep
a single limiter (no source+final split). Defer gate promotion.

**Consequences**: Eliminates crate-side order drift and makes the true-peak
metric measure a faithful chain, unblocking a later gate promotion. Full
callback convergence and gate promotion are tracked as follow-ups. Avoids a
behavior change (second limiter) and zero-overlaps the in-flight true-peak task.

## Out of Scope

- Adding a second/"source" limiter (single limiter chosen; overlaps
  `06-12-audio-engine-true-peak-limiter`).
- Promoting `full_output_chain_worst_true_peak` from report to gate (deferred
  follow-up; depends on parity + downstream convergence landing first).
- Changing the downstream Lyne callback assembly (separate cross-repo task that
  adopts this crate's builder + snapshot).
- Multiband/mastering limiter behavior; replacing SoXR or the EBU R128 meter.

## Implementation Plan (subtasks, sequenced)

- **PR1 — Canonical definition + builder (no behavior change)**: add
  `output_chain` module with the ordered stage descriptors (+ latency/state
  metadata) and the builder producing `OutputRenderChain` / `DspChain`. Unit
  tests for builder output and metadata. No bench/runtime wiring yet.
- **PR2 — Offline renderer + benches consume builder**: rebuild
  `render_full_output_chain` on the builder with the missing pre-limiter stages;
  repoint `audio_quality_measurements` and `audio_callback_chain_perf` at the
  builder; fix perf bench NoiseShaper. Node-order parity test + per-sample
  equivalence test. RT-safety test for the callback-direction output.
- **(Follow-up, separate task)** Downstream Lyne callback adopts the builder +
  snapshot; then gate promotion of full-output true-peak.

## Validation Commands

- `cargo test output_chain --lib`
- `cargo test processor::adapters --lib`
- `cargo check --benches`
- `cargo bench --bench audio_quality_measurements -- --quick`
- `cargo bench --bench audio_callback_chain_perf -- --quick`

## Validation Results (2026-06-20)

- `cargo test output_chain --lib` — passed (7 output-chain tests).
- `cargo test processor::adapters --lib` — passed (15 adapter tests).
- `cargo check --benches` — passed.
- `cargo bench --bench audio_quality_measurements -- --quick` — passed; full
  output true peak remains report-only (`worst_output_true_peak_dbtp=-0.610`,
  `over_limit_points=1`).
- `cargo bench --bench audio_callback_chain_perf -- --quick` — passed; callback
  node snapshot prints
  `Volume,Equalizer,Saturation,Crossfeed,Convolver,DynamicLoudness,PeakLimiter,NoiseShaper`.

- Key files: `benches/audio_quality_measurements.rs`,
  `benches/audio_callback_chain_perf.rs`, `src/processor/dsp_chain.rs`,
  `src/processor/adapters.rs`, `src/pipeline.rs`.
- Related task: `.trellis/tasks/06-12-audio-engine-true-peak-limiter/`.
- RT-safety spec: `.trellis/spec/backend/realtime-safety.md`.
