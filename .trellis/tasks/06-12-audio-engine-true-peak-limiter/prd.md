# Oversampled True Peak Limiter

## Goal

Add a true-peak limiting path that can keep rendered output below the configured ceiling on intersample-peak stress material, while preserving realtime callback safety.

## Requirements

- Audit the current `PeakLimiter`, `LoudnessMeter`, `LoudnessNormalizer`, DSP adapters, and quality benchmarks before changing behavior.
- Design a limiting path whose detection/control is based on oversampled intersample peaks rather than only sample peaks.
- Keep the audio callback path allocation-free and free of locks, file I/O, network I/O, and logging.
- Document the limiter delay/latency and any phase/oversampling assumptions.
- Update atomic parameter plumbing only where needed for mode/quality/threshold/release behavior.
- Preserve or intentionally migrate current sample-peak behavior with clear naming so API users do not confuse sample peak and true peak guarantees.

## Acceptance Criteria

- [ ] Synthetic intersample-peak fixtures that currently expose the limitation are limited below the configured ceiling with a defined tolerance.
- [ ] Tests cover mono, stereo, and at least one multichannel layout.
- [ ] Tests cover cross-buffer continuity, reset behavior, silence, sustained over-threshold material, and non-finite sample handling policy.
- [ ] Realtime processing tests assert no steady-state allocation for the limiter path.
- [ ] `audio_quality_measurements` reports the limiter method accurately and includes true-peak stress results.
- [ ] README limitation text is updated only after measured evidence supports the new behavior.

## Validation Commands

- `cargo test loudness::limiter --lib`
- `cargo test loudness::meter --lib`
- `cargo test processor::adapters --lib`
- `cargo check --benches`
- `cargo bench --bench audio_quality_measurements -- --quick`

## Out of Scope

- Multiband mastering limiter behavior.
- UI controls outside this crate.
- Analog output capture or DAC/ADC loopback proof.
- Replacing SoXR or the existing EBU R128 meter.

## Technical Notes

- Current risk: `PeakLimiter` is named and documented as true peak in places, but implementation is lookahead sample-peak limiting.
- Existing true-peak measurement logic in `LoudnessMeter` is measurement-oriented and can inform fixtures, but the limiter needs a control path suitable for realtime processing.
- Shared audit: `../06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
