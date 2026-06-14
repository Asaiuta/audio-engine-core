# Quality Gates Scope Notes

## Current State

- The crate already has benchmark files for callback-chain cost, resampling, quality measurements, convolution, lock-free params, and FIR EQ.
- README includes objective quality and performance tables, plus a visible true-peak limitation note.
- Some measurements are report-only and should not be treated as pass/fail conformance.

## Design Direction

- Separate `report` metrics from `gate` metrics in benchmark output.
- Prefer deterministic synthetic fixtures for default quick gates.
- Keep optional external reference corpora opt-in and clearly marked as skipped when absent.
- Produce evidence that can be copied into README only after a current run.

## Risks

- Overly strict numeric thresholds can be flaky across CPUs, compiler versions, or debug/release settings.
- `cargo bench` behavior can differ from `cargo test`; gate scripts should fail with clear diagnostics.
- Quality metrics must avoid implying analog output quality because these benches render native buffers only.
