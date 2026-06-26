# Partitioned Convolution Scope Notes

## Current State

- `FFTConvolver` uses overlap-save with one FFT size derived from IR length.
- FIR EQ uses generated IRs that are usually short enough for the current convolver path.
- Benchmarks already cover convolver and FIR EQ performance, making this task measurable.

## Design Direction

- Keep current `FFTConvolver` for short IRs.
- Add a separate partitioned engine for long IRs and route based on IR length or explicit configuration.
- Consider uniform partitioning first; non-uniform partitioning is a possible later optimization if uniform cost is still too high.

## Risks

- Partition scheduling can create hidden latency or uneven CPU bursts if not designed carefully.
- Multichannel partition state can multiply memory use; construction-time allocation must be explicit.
- Equality tests need tolerances because FFT partitioning may change floating-point operation order.
