# Uniform Parameter-Transition Continuity Gate

Generalize the crate's existing *ad-hoc, per-processor* continuity probes into
one **systematic discontinuity gate** covering every processor that accepts
live parameter updates from the control thread.

## Motivation

The crate already treats "a parameter change must not produce an audible
click" as a real correctness property — but only in scattered places, chosen
per processor by whoever happened to write that probe:

| Existing evidence in `docs/quality.md` | Kind |
| --- | --- |
| Crossfeed mix-change continuity delta `0.000e0` (vs `5.762e-3` reset sim) | parameter-step probe |
| Saturation threshold max jump `1.416e-6` / derivative mismatch `3.610e-4` | *transfer-curve* probe, not a parameter step |
| `Equalizer` per-band crossfade (`smooth_counter`, `EQ_SMOOTH_SAMPLES`) | unit-tested, **no quality gate** |

So today: the EQ has real smoothing machinery (`src/processor/eq.rs:120`,
per-sample interpolation at `:312`, `assert_transition_adopts_target_state`)
yet contributes **zero rows** to the quality evidence. The saturation row is
about the shape of the waveshaper curve, not about stepping `drive` mid-block.
There is no uniform statement of the form "no processor in this crate steps its
output by more than X when a parameter changes."

The gap is not missing smoothing — much of it exists. The gap is **missing
uniform evidence** that it exists and keeps existing.

## Goal

Add one parameter-transition discontinuity measurement that iterates every
processor with an `Atomic*Params` control surface, steps each parameter
mid-block, and gates the resulting worst-case sample-to-sample delta — reported
in `docs/quality.md` alongside frequency response and THD+N, and enforced in
CI by the existing `audio_quality_measurements` bench.

## Scope

In scope — the atomic-param processors in `src/processor/lockfree_params.rs`:

- `AtomicEqParams` (`:517`)
- `AtomicSaturationParams` (`:720`)
- `AtomicCrossfeedParams` (`:970`) — already probed; fold into the uniform gate
- `AtomicPeakLimiterParams` (`:1066`)
- `AtomicVolumeParams` (`:1150`)
- `AtomicNoiseShaperParams` (`:1231`)

**Explicitly out of scope — do not touch:**

- `AtomicDynamicLoudnessParams` (`:1325`) and the limiter *attack ramp*, the
  dynamic-loudness ±3 dB bypass step, the normalizer block-gain zipper, and
  normalizer re-enable stale history. All four are owned by the **already-open
  `08-11-gain-trajectory-continuity`** task (status `planning`, base branch
  `fix/review-2026-08-11-followups`). This task must not re-fix them.
- Any change to smoothing behavior itself. This task **measures**; it does not
  redesign ramps. If a probe exposes a genuine discontinuity, record it as a
  finding and (if it belongs to the 08-11 theme) hand it to that task rather
  than fixing it here.
- Public API changes. Additive measurement only.

Boundary rule with 08-11: that task changes *gain trajectories*; this task adds
*measurement infrastructure and evidence rows*. If both are in flight, this one
consumes whatever trajectory 08-11 lands on — so its thresholds must be derived,
not hand-pinned to today's numbers for the processors 08-11 will touch.

## Requirements

1. **Probe harness.** For each in-scope processor: render a steady-state signal,
   step one parameter at a known mid-block sample index, and measure the maximum
   `|sample[n+1] - sample[n]|` in a window around the step.
2. **Baseline separation.** The metric must isolate the *parameter step* from
   the signal's own natural slew. Compare against the same processor run with no
   parameter change (as the crossfeed row already does with its reset
   simulation), so a steep-but-legitimate waveform doesn't read as a click.
3. **Derived bounds, not magic numbers.** Each threshold must be justified from
   the processor's documented smoothing window (e.g. `EQ_SMOOTH_SAMPLES`, the
   ~10 ms crossfeed ramp) and the test signal's own maximum slope — following the
   existing convention of deriving bounds rather than pinning observed output.
4. **Every parameter, not one per processor.** Step each field of each
   `Atomic*Params`, so a newly added unsmoothed parameter cannot slip through.
5. **Evidence integration.** New section in `docs/quality.md` under
   "Audio Quality", in the established table style, stating what the probe does
   and does *not* prove (synthetic probe, not a listening test — match the
   hedging already used for the listening-DSP rows).
6. **CI enforcement.** Wire into `benches/audio_quality_measurements.rs` using
   the existing `gate` / `gate_within` / `MetricResult` helpers and the `--quick`
   path, so `.github/workflows/ci.yml:269` picks it up with no workflow edit.
7. **No regressions.** Existing quality gates and the `Scope & Limitations`
   honesty boundary stay intact.

## Non-Goals

- Spectral artifact analysis of transitions (FFT of the click). Amplitude
  discontinuity first; spectral characterization is a possible follow-up.
- Fixing any discontinuity this surfaces (see Scope).
- Immutable/RCU published processing graph — separately considered, not part of
  this task.

## Completion Criteria

| Condition | Required |
| --- | :---: |
| Probe covers all 6 in-scope `Atomic*Params`, every field stepped | ✅ |
| Thresholds derived and documented, not pinned to observed values | ✅ |
| `docs/quality.md` section added with explicit limitations | ✅ |
| Runs in the bench's `--quick` path; CI green with no workflow edit | ✅ |
| No public API change; existing gates still pass | ✅ |
| Zero overlap with `08-11-gain-trajectory-continuity` deliverables | ✅ |

## Provenance

Emerged from auditing a set of "projects worth learning from" recommendations.
Most of that advice did not survive verification — the top-ranked reference
(`forge-audio`) is a 3.5k-line crate whose GitHub repo 404s and which ships
`src/stubs.rs` gating the real implementation behind a commercial license; the
second (`audio-graph-bsd`) was credited with buffer-lifetime analysis it does
not implement (`graph.rs:127` allocates one scratch buffer per port, the naïve
strategy); and the "learn parameter smoothing from FreeEQ8" suggestion was
premised on a click bug this crate had already fixed. Every recommended project
was smaller than this one — several are single-commit repos.

What *did* survive is this: continuity-under-parameter-change deserves to be a
first-class, uniform gate rather than three hand-picked rows. That conclusion
stands on the repo's own evidence, not on the discarded references.
