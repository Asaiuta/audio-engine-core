# Audio Quality Gates And Benchmark Evidence

## Goal

Make audio-quality and realtime-performance claims repeatable by adding or tightening benchmark gates, JSON/report outputs, and README evidence rules before feature upgrades change the DSP stack.

## Requirements

- Audit existing benchmarks and identify which metrics are report-only versus enforced gates.
- Add threshold-based checks for metrics that are stable enough to enforce locally.
- Keep external corpus support optional and explicit: missing EBU files should be reported as skipped/limited, not silently passed.
- Extend quality measurements to cover true-peak limiter stress, saturation aliasing, convolver scaling, resampler quality, and dither/noise shaping where practical.
- Emit machine-readable evidence for benchmark runs so README/docs updates can cite current values.
- Document known limitations honestly when a metric cannot be enforced yet.

## Acceptance Criteria

- [ ] Current benchmark inventory is documented with method, metric, threshold, and report/gate status.
- [ ] Quick benchmark mode remains practical for local development.
- [ ] Full/slow benchmark mode can produce richer evidence without changing code behavior.
- [ ] README claims are traceable to benchmark names and current output values.
- [ ] Gate failures include actionable metric names and measured values.
- [ ] Benchmarks do not require network access.

## Validation Commands

- `cargo check --benches`
- `cargo bench --bench audio_quality_measurements -- --quick`
- `cargo bench --bench audio_callback_chain_perf -- --quick`
- `cargo bench --bench audio_resampler_streaming_perf -- --quick`
- `cargo bench --bench audio_convolver_perf -- --quick`
- `cargo bench --bench audio_fir_eq_perf -- --quick`

## Out of Scope

- External listening tests.
- Analog output capture, DAC/ADC loopback, or microphone measurement.
- CI service configuration outside this repository.
- Claiming EBU corpus conformance when reference files are absent.

## Technical Notes

- This task is recommended first so later DSP feature work can compare before/after behavior.
- Existing README numbers are useful but should be regenerated or clearly labeled when used as proof.
- Shared audit: `../06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
