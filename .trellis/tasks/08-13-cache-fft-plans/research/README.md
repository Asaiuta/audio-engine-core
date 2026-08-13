# Bench evidence

Interleaved before/after, three reps each, median of 9 trials per case, run with
`--no-default-features --features rubato` (the default `soxr` feature routes
around this code entirely).

`CTRL 192k Linear` is the in-run control: linear-phase setup performs no
minimum-phase work, so it must not move. It stayed within -2..-5% across all
three reps, which is this host's noise floor for these cases.

| files | content |
| --- | --- |
| `b1..b3.txt` | before (source changes stashed) |
| `a1..a3.txt` | after |

**EQ cases are the reportable result**: every rep improved, -16% to -68%, with
no sign flips.

**SETUP cases are noise-dominated and are not claimed.** Per-rep deltas for
`48->192k High` were -24%, -20%, +9%; run-to-run spread on setup reached 50%+ in
an earlier attempt. The per-object saving is real in principle (two cold planners
collapse to one) but this host cannot resolve it at the sample size used.
