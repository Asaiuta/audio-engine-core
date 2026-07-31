# Expand Resampler / Core Performance Benchmark Matrix

## Goal

Close the largest documented gaps in `benches/` so soxr↔rubato and quality/phase/rate choices can be compared with versioned JSON evidence, without breaking existing streaming-resampler baselines.

## In scope

1. New harness `audio_resampler_matrix_perf`:
   - rate pairs beyond the current 44.1→48 / 48→96 pair
   - `ResampleQuality` tiers (at least High + UltraHigh + one cheaper tier)
   - `PhaseResponse` Linear + at least one nonlinear case
   - multi-channel (stereo + one 5.1-style case)
   - setup/construction cost alongside steady-state `process_checked` cost
   - same report schema conventions as other perf benches (environment, trials, baseline gate)
2. Document how to run the matrix in `docs/quality.md`.
3. Register the bench in `Cargo.toml`.

## Out of scope (this task)

- Changing default resampler feature from `soxr` to `rubato`
- Breaking/renumbering existing `audio_resampler_streaming_perf` case keys
- Decoder / device / SQLite / network perf (follow-up)
- Adding the matrix to CI enforce (local/report-first; CI optional later)

## Acceptance

- `cargo bench --bench audio_resampler_matrix_perf -- --quick --enforce` succeeds on default features
- `cargo bench --bench audio_resampler_matrix_perf -- --no-default-features --features rubato -- --quick --enforce` succeeds when built with rubato-only features (via cargo feature flags on the package)
- JSON report writes with stable `case_key`s including backend, rates, quality, phase, channels, frames
- `docs/quality.md` lists the new bench and what it covers / excludes
