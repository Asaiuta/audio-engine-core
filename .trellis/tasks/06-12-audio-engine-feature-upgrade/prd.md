# Audio Engine Feature Upgrade Roadmap

## Goal

Turn the current audio engine from a strong collection of mature DSP building blocks into a set of measurable, high-end playback features. The work should prioritize source-backed gaps found in the algorithm audit instead of making broad claims that every current algorithm is already industry-leading.

## What I Already Know

- The crate already includes Symphonia decoding, SoXR resampling, EBU R128 loudness measurement, FIR true-peak measurement, IIR/FIR EQ, FFT convolution, dynamic loudness, crossfeed, saturation, dither/noise shaping, and callback-focused benchmarks.
- SoXR resampling, EBU loudness measurement, and SoX-style dither/noise shaping are already strong foundations.
- The current `PeakLimiter` is a lookahead sample-peak limiter, not a strict oversampled true-peak limiter.
- Current saturation uses direct nonlinear waveshaping without an explicit oversampling or anti-aliasing stage.
- Current FFT convolution is efficient for shorter IRs but is not a partitioned long-IR engine.
- README quality numbers are useful, but future stronger claims need fresh, reproducible benchmark evidence.

## Requirements

- Split the feature upgrade into small, independently executable Trellis child tasks.
- Each child task must have a PRD, research notes, implementation/check context, acceptance criteria, and validation commands.
- Prioritize measurable audio-quality gaps over marketing language.
- Preserve realtime callback constraints: no heap allocation, locks, logging, file I/O, network I/O, or unbounded work in hot audio processing paths.
- Treat benchmark evidence as part of the feature, not an afterthought.
- Keep public API churn bounded and document any intentional compatibility break.

## Child Tasks

Each task carries a priority tier (see Priority Tiers below): **P1** = commit now,
**P2** = evidence-driven enhancement, **Backlog** = needs a downstream (Lyne app)
requirement before starting, **Release gate** = runs last.

1. `06-12-audio-engine-trellis-spec-bootstrap` **[P1]** - replace placeholder backend spec with source-backed conventions and create `realtime-safety.md` that other tasks' jsonl inject. Hard unblocker for every other task.
2. `06-12-audio-engine-quality-gates` **[P1]** - establish and enforce repeatable audio-quality/performance gates. Mostly formalizes existing benches; low cost, builds the before/after baseline.
3. `06-12-audio-engine-decoder-format-capability` **[P1]** - make Symphonia decode/seek/error behavior explicit and tested. Contains the only confirmed correctness bug (post-seek `encoder_delay` double-trim) that corrupts audio in the consuming app.
4. `06-12-audio-engine-true-peak-limiter` **[P1]** - add an oversampled true-peak limiting path and prove output ceilings. Closes the limitation already documented in README.
5. `06-12-audio-engine-oversampled-saturation` **[P2]** - reduce nonlinear saturation aliasing with an RT-safe quality mode. Enhancement; gated on quality-gates baseline existing.
6. `06-12-audio-engine-partitioned-convolution` **[P2]** - support long impulse responses with bounded callback cost. Enhancement; only valuable once a consumer needs long room IRs.
7. `06-12-audio-engine-eq-perceptual-dsp` **[Backlog]** - upgrade IIR/FIR EQ, dynamic loudness, and crossfeed with measured evidence. Speculative until a source-backed quality gap or app request justifies it.
8. `06-12-audio-engine-channel-layout-mixing` **[Backlog]** - add channel-layout metadata and downmix/upmix policy. New capability (5.1/7.1); start only when the Lyne app actually needs multichannel.
9. `06-12-audio-engine-api-release-hardening` **[Release gate]** - stabilize the public API, feature flags, docs, and release readiness. Runs last so it reflects actual delivered capabilities; premature while the API still churns from DSP work.

## Priority Tiers

This roadmap is a backlog, not a single next step. The tiers reflect what the
crate actually needs next versus what is speculative for a pre-1.0 primitives
library whose only consumer is the Lyne app:

- **P1 (commit now)** — unblockers, baseline, and the one confirmed correctness
  bug. These are justified regardless of new feature demand: spec-bootstrap
  (hard dependency), quality-gates (baseline), decoder-format-capability (real
  post-seek corruption bug), true-peak-limiter (closes an already-documented
  README limitation).
- **P2 (evidence-driven enhancement)** — oversampled-saturation,
  partitioned-convolution. Real gaps, but enhancements; start after the P1
  baseline exists so before/after metrics are meaningful.
- **Backlog (needs downstream demand)** — eq-perceptual-dsp,
  channel-layout-mixing. No PRD currently cites a Lyne app requirement for
  these; building 5.1/7.1 downmix or perceptual-DSP rewrites before a consumer
  asks is YAGNI risk. Promote to P2 when a source-backed gap or app request
  appears.
- **Release gate (last)** — api-release-hardening, after the API stops churning.

## Recommended Sequence

1. Implement `trellis-spec-bootstrap` first: it creates `realtime-safety.md` and fills the placeholder backend spec that other tasks' `implement.jsonl`/`check.jsonl` reference. `task.py validate` rejects jsonl entries whose spec files do not yet exist, so this is a hard prerequisite, not just a recommendation.
2. Run or extend quality gates so there is a baseline for the current engine before any DSP change.
3. Fix `decoder-format-capability` next: the post-seek `encoder_delay` double-trim is the only confirmed correctness bug in the audit and corrupts audio after any non-zero seek, so it outranks the DSP enhancements.
4. Implement true-peak limiting because it closes the clearest, already-documented limitation in current README claims.
5. (P2) Implement oversampled saturation after the limiter so nonlinear processing has objective alias metrics.
6. (P2) Implement partitioned convolution after the quality harness can compare short and long IR cost.
7. (Backlog) Defer `eq-perceptual-dsp` and `channel-layout-mixing` until a downstream requirement justifies them; channel-layout also depends conceptually on the layout count surfaced by decoder-format-capability.
8. (Release gate) Run `api-release-hardening` last so the public surface, docs, and README claims reflect the actual, measured capabilities delivered above.

## Acceptance Criteria

- [ ] All child tasks exist and are linked from this parent.
- [ ] Parent and child PRDs define scope, out-of-scope, acceptance criteria, and validation commands.
- [ ] Each task has `implement.jsonl` and `check.jsonl` entries that reference only spec/research files.
- [ ] `task.py validate` passes for the parent and each child task.
- [ ] No child task is marked `in_progress` until the user chooses the first implementation target.

## Definition of Done

- Task files are valid Trellis artifacts.
- The roadmap can be executed one child task at a time.
- The known true-peak limitation remains visible until the limiter task proves otherwise.
- The README is not upgraded to stronger claims without current measurement evidence.

## Out of Scope

- Implementing DSP code in this planning task.
- Replacing the decoder or SoXR resampler.
- UI, device output, CPAL/WASAPI ownership, or application integration work outside this crate.
- Marketing claims such as "all algorithms are industry-leading" without measured evidence.

## Technical Notes

- Source audit anchors: `src/processor/loudness/limiter.rs`, `src/processor/loudness/meter.rs`, `src/processor/saturation.rs`, `src/processor/convolver.rs`, `src/processor/fir_eq.rs`, `src/processor/dsp.rs`, and `benches/audio_quality_measurements.rs`.
- Shared research summary: `research/current-algorithm-audit.md`.
- The project Trellis backend spec is still bootstrap-level; implementation tasks should inspect live Rust source and avoid relying on generic spec placeholders alone.
