# Oversampled Anti Aliasing Saturation

## Goal

Reduce aliasing from nonlinear saturation modes by adding an RT-safe anti-aliasing or oversampling path with objective measurement evidence.

## Requirements

- Audit current tape/tube/transistor waveshaping and highpass exciter mode before changing behavior.
- Add a bounded quality option for anti-aliased saturation, such as fixed 2x/4x oversampling with preallocated filters or another measured RT-safe approach.
- Preserve deterministic behavior and avoid per-callback heap allocation.
- Keep channel state pre-sized during setup, not resized during processing.
- Add alias-energy measurements that compare current fullband saturation with the upgraded path at equivalent drive/mix settings.
- Update benchmark/performance documentation only with measured data.

## Acceptance Criteria

- [ ] Alias-energy measurement shows a meaningful reduction versus the current direct waveshaper on high-frequency stress signals.
- [ ] Unit tests cover all saturation modes, highpass mode, channel counts, reset behavior, and sample-rate changes.
- [ ] Realtime no-allocation tests cover the upgraded processing path after setup.
- [ ] Callback performance remains within the configured budget for 512-frame buffers.
- [ ] Public names distinguish quality/oversampling mode from the existing direct mode.

## Validation Commands

- `cargo test saturation --lib`
- `cargo test processor::adapters --lib`
- `cargo check --benches`
- `cargo bench --bench audio_callback_chain_perf -- --quick`
- `cargo bench --bench audio_quality_measurements -- --quick`

## Out of Scope

- Detailed analog circuit emulation.
- Machine-learning or neural saturation models.
- UI presets outside this crate.
- Replacing unrelated EQ/crossfeed/dynamic loudness behavior.

## Technical Notes

- Current saturation is classic waveshaping and musically useful, but nonlinear processing without oversampling can alias.
- The implementation should prefer simple, measurable antialiasing over a large dependency or unbounded CPU path.
- Shared audit: `../06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
