# True Peak Limiter Scope Notes

## Current State

- `PeakLimiter` applies a delayed sample with gain derived from a lookahead window of sample peaks.
- `LoudnessMeter` has a 4x FIR true-peak detector for measurement, not limiting.
- README already discloses that the sample-peak/lookahead limiter is not a full intersample true-peak guarantee.

## Design Direction

- Prefer an explicit true-peak limiter mode or renamed limiter type over silently changing semantics with no documentation.
- Use stress fixtures that include intersample peaks after resampling/output conversion, not only sample values above threshold.
- Keep latency explicit. A stronger limiter is allowed to add lookahead delay if it is documented and bounded.

## Risks

- Oversampling inside a realtime limiter can be CPU-heavy if implemented naively.
- A detector-only true-peak meter does not automatically solve limiting because gain decisions must be aligned with delayed output.
- Output-chain true peak can change after resampling and final quantization, so the benchmark must mirror the actual chain being claimed.
