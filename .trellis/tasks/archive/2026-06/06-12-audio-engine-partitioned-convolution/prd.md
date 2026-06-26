# Partitioned Convolution For Long Impulse Responses

## Goal

Support long impulse responses with bounded realtime cost by adding a partitioned convolution path while retaining the current efficient short-IR FFT convolver.

## Requirements

- Audit current `FFTConvolver`, `FirEq`, convolver adapters, and benchmarks before implementation.
- Keep the existing short-IR path available for small FIR EQ and short convolution use cases.
- Add a partitioned convolver for long IRs, with a documented routing threshold based on measured performance.
- Precompute FFT plans and partition spectra outside the audio callback.
- Ensure `process_into`/`process_inplace` equivalent paths do not allocate in steady state.
- Support mono, stereo, and common multichannel layouts without channel-order surprises.
- Update benchmarks to compare current and partitioned paths across short, medium, and long IR sizes.

## Acceptance Criteria

- [ ] Partitioned output matches direct/current convolution within a defined numeric tolerance for deterministic fixtures.
- [ ] Cross-buffer continuity and reset behavior are covered by tests.
- [ ] Long IR benchmark shows bounded per-buffer cost and avoids callback-scale spikes.
- [ ] Short IR routing does not regress existing FIR EQ benchmark results beyond an agreed tolerance.
- [ ] Documentation explains when each convolver path is selected.

## Validation Commands

- `cargo test convolver --lib`
- `cargo test fir_eq --lib`
- `cargo test processor::adapters --lib`
- `cargo check --benches`
- `cargo bench --bench audio_convolver_perf -- --quick`
- `cargo bench --bench audio_fir_eq_perf -- --quick`

## Out of Scope

- UI or file loading for impulse responses.
- Network or asset management for IR files.
- Reverb design, room modeling, or HRTF content generation.
- Replacing FIR EQ design unless required for routing compatibility.

## Technical Notes

- Current overlap-save FFT convolver is a good standard implementation, but it scales poorly as a one-size-fits-all long-IR solution.
- Partitioned convolution should be added as an architecture extension, not by making every short IR pay long-IR overhead.
- Shared audit: `../06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
