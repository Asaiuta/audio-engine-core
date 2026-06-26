# EQ and Perceptual DSP Upgrades

## Goal

Upgrade the listening-feature DSP (IIR/FIR EQ, dynamic loudness, crossfeed) beyond the current classic baseline, with measurable quality evidence rather than marketing claims, while preserving realtime callback safety.

## Requirements

- Audit the current `processor/eq.rs`, `processor/fir_eq.rs`, `processor/dynamic_loudness.rs`, and `processor/crossfeed.rs` implementations and document their present behavior and limitations.
- Identify the specific, source-backed quality gaps to close (e.g. EQ filter accuracy/phase behavior, dynamic-loudness contour fidelity, crossfeed model realism) instead of assuming every module needs change.
- Implement targeted improvements with documented design choices (filter type, contour model, crossfeed parameters) and objective measurements.
- Keep the audio callback path allocation-free and lock-free; precompute coefficients on parameter change, not per sample.
- Reuse the existing lock-free parameter plumbing for any new tunable parameters.
- Provide before/after measurements (frequency response error, phase, or alias/noise metrics as appropriate) through the quality benchmark harness.
- Update the `equalizer_curve` example and any docs only after measured evidence supports the change.

## Acceptance Criteria

- [ ] Each upgraded module has a documented rationale tied to an audited gap, not a generic "make it better".
- [ ] EQ changes are validated against target frequency responses within a defined tolerance.
- [ ] Dynamic loudness and crossfeed changes have objective metrics demonstrating the intended effect.
- [ ] Tests cover mono and stereo, plus parameter-change continuity (no clicks/discontinuities on coefficient updates).
- [ ] Realtime processing tests assert no steady-state allocation for the upgraded paths.
- [ ] `audio_quality_measurements` reports the relevant before/after metrics.
- [ ] The `equalizer_curve` example still runs and reflects current behavior.

## Validation Commands

- `cargo test processor::eq --lib`
- `cargo test processor::fir_eq --lib`
- `cargo test processor::dynamic_loudness --lib`
- `cargo test processor::crossfeed --lib`
- `cargo check --benches`
- `cargo bench --bench audio_quality_measurements -- --quick`
- `cargo run --example equalizer_curve`

## Out of Scope

- True-peak limiting (owned by `06-12-audio-engine-true-peak-limiter`).
- Oversampled saturation anti-aliasing (owned by `06-12-audio-engine-oversampled-saturation`).
- Partitioned long-IR convolution (owned by `06-12-audio-engine-partitioned-convolution`).
- Channel layout/downmix policy (owned by `06-12-audio-engine-channel-layout-mixing`).
- Replacing SoXR or the EBU R128 meter.

## Technical Notes

- The audit classifies IIR/FIR EQ, crossfeed, and related modules as classic, maintainable DSP that is useful but not automatically industry-leading; improvements must be evidence-backed.
- This task covers the listening-feature upgrades intentionally left out of the first DSP-hardening pass.
- Source anchors: `src/processor/eq.rs`, `src/processor/fir_eq.rs`, `src/processor/dynamic_loudness.rs`, `src/processor/crossfeed.rs`, `src/processor/lockfree_params.rs`, `examples/equalizer_curve.rs`.
- Shared audit: `../06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
- Task audit: `research/listening-dsp-audit.md`.
