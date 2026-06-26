# Saturation Aliasing Scope Notes

## Current State

- `Saturation` applies direct tape/tube/transistor waveshaping in fullband mode.
- Highpass mode separates high-frequency content with a first-order HPF and saturates that component.
- Current tests focus on behavior, parameter updates, channel state sizing, and denormal handling rather than aliasing metrics.

## Design Direction

- Prefer a quality mode that is explicit and measurable: direct mode remains simple, higher quality mode spends more CPU to reduce aliasing.
- Preallocate all resampling/filter buffers during setup.
- Add benchmark cases for high-frequency sine, swept sine, and driven mixed content.

## Risks

- Using a general resampler in every DSP callback can be too heavy unless state and buffers are carefully reused.
- Oversampling can change saturation tone and output level, so tests should compare aliasing and level compensation separately.
- Silence bypasses and threshold logic must not reintroduce clicks when switching quality modes.
