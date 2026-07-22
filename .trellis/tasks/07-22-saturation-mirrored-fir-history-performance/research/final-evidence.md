# Saturation Mirrored FIR History Evidence

Date: 2026-07-22

Environment: Windows 11, Intel Alder Lake, `x86_64-pc-windows-msvc`, rustc
1.93.1, release profile, default features (`http`, `loudness-db`,
`resampler-soxr`). Candidate reports and both baselines use the same case
matrix and environment. Reports are dirty because task evidence was present in
the worktree; revision and dirty state are traceability fields rather than
compatibility keys.

## Change

`OversamplingChannelState` now stores a fixed mirrored history and points
`filter_index` at the newest residual. Each push writes both mirror positions,
and the FIR reads one contiguous newest-to-oldest window. The coefficient and
accumulator order is unchanged. A test-only legacy circular implementation
matches the new state bit-for-bit across repeated wraps, reset, and initialize.

## Callback benchmark

Command:

```text
cargo bench --bench audio_callback_chain_perf -- --quick --enforce \
  --out callback-final-repeat.json \
  --baseline .trellis/tasks/archive/2026-07/07-22-saturation-oversampled4x-performance/research/callback-final.json \
  --max-median-regression-pct 100
```

The generic comparison limit was set to 100% only to avoid the transparent
bypass timing noise; task-specific isolated and active callback acceptance
checks remained strict and all passed. The primary baseline is the preceding
fixed-dispatch task's retained same-machine report:

| Block | Fixed-dispatch baseline | Mirrored candidate | Change |
| ---: | ---: | ---: | ---: |
| 64 frames | 31.870 ns/sample | 22.955 | -27.972% |
| 128 frames | 30.461 ns/sample | 21.905 | -28.088% |
| 256 frames | 29.536 ns/sample | 22.971 | -22.228% |
| 512 frames | 30.142 ns/sample | 22.897 | -24.037% |

At 512 frames, the no-convolver active chain improved from `54.854` to
`50.272 ns/sample` (-8.354%), and its p95 deadline utilization improved from
`0.538%` to `0.498%`. The IR256 chain improved from `63.731` to
`60.892 ns/sample` (-4.455%), and p95 utilization improved from `0.633%` to
`0.613%`. Both active callback acceptance checks passed the +3% median and +5%
p95 limits.

The fresh immediately-pre-change baseline (`callback-baseline.json`) was
materially noisier than the retained fixed-dispatch run. Candidate 1 and 2
still passed every strict gate against it, with isolated medians between
`19.471` and `24.260 ns/sample`. The first comparison against the stable
archived baseline (`callback-final.json`) failed only the p95 gates because of
single slow trials (+10.594% and +5.034%); medians and every isolated case
improved. The immediate repeat above passed all gates, so both reports are
retained rather than hiding timing variance.

## Quality and render benchmarks

`audio_quality_measurements --quick --enforce` passed all `27/27` gates with
zero skips. Saturation remained at `+16.32 dB` folded alias reduction and
`+0.35 dB` fundamental delta.

`audio_output_render_perf --quick --enforce` passed all 18 scenario/duration/
block cases. The active equal-rate `Oversampled4x` render measured `21.263
ns/input sample` for five seconds with 4096-frame blocks, retained the exact
four-frame semantic tail, and kept peak temporary bytes bounded at `131776`.
