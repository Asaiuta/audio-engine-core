# Oversampled4x Performance Evidence

Date: 2026-07-22

Environment: Windows 11, Intel Alder Lake, `x86_64-pc-windows-msvc`, rustc
1.93.1, release profile, default features (`http`, `loudness-db`,
`resampler-soxr`). The compatible baseline was captured immediately before the
source change at revision `b32c217`; both reports are dirty but otherwise share
the same environment and case matrix.

## Change

The 2x/4x paths now dispatch quality once per block into const-generic kernels.
The fixed kernels compile a constant phase count and tap count, while the
existing transfer functions, filter state layout, coefficients, and arithmetic
order remain unchanged. High-pass processing uses the same kernel with
`<1,0>` for Direct and `<2,17>`/`<4,33>` for oversampled modes. A focused unit
test compares dynamic and fixed state sample-by-sample and passes bit-for-bit.

## Callback benchmark

Command:

```text
cargo bench --bench audio_callback_chain_perf -- --quick --enforce \
  --out callback-final.json --baseline callback-baseline.json \
  --max-median-regression-pct 100
```

The generic comparison threshold was set to 100% only to avoid treating the
sub-nanosecond transparent-bypass case as a timing gate. The task-specific
acceptance checks remained active and all passed. The four isolated saturation
cases were strictly faster:

| Block | Baseline | Candidate | Change |
| ---: | ---: | ---: | ---: |
| 64 frames | 43.026 ns/sample | 31.870 | -25.927% |
| 128 frames | 36.311 ns/sample | 30.461 | -16.113% |
| 256 frames | 36.252 ns/sample | 29.536 | -18.525% |
| 512 frames | 36.942 ns/sample | 30.142 | -18.406% |

The 512-frame complete-chain checks also passed:

| Scenario | Baseline | Candidate | Change |
| --- | ---: | ---: | ---: |
| Active DSP, no convolver | 64.800 ns/sample | 54.854 | -15.350% |
| Active DSP, IR256 convolver | 74.810 ns/sample | 63.731 | -14.809% |
| No-convolver p95 deadline utilization | 0.626% | 0.538% | -14.039% |
| IR256 p95 deadline utilization | 0.815% | 0.633% | -22.385% |

## Quality benchmark

`audio_quality_measurements --quick --enforce` passed all `27/27` gates with
zero skips. Saturation remained at `+16.32 dB` folded alias reduction and
`+0.35 dB` fundamental delta. The existing continuity, passband, stopband,
limiter, loudness, crossfeed, and noise-shaper metrics also passed unchanged.

## Output render

`audio_output_render_perf --quick --enforce` passed all 18 scenario/duration/
block cases. The active equal-rate `Oversampled4x` render measured `29.657
ns/input sample` for five seconds with 4096-frame blocks and retained the exact
four-frame semantic tail. Fixed-stage temporary memory remained bounded and
the report recorded active work, finite output length, and allocation counts.
