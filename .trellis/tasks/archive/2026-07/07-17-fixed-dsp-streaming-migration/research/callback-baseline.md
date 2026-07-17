# Callback Baseline Before Streaming Migration

Measured on 2026-07-17 in the current Windows workspace with:

```text
cargo bench --bench audio_callback_chain_perf -- --quick
```

Five independent runs were used because quick mode reports one trial per run.
The comparison metric is the median `ns_per_sample` for a 512-frame stereo
buffer at 48 kHz.

| Scenario | Runs (ns/sample) | Median |
| --- | --- | ---: |
| bypass_default | 0.134, 0.115, 0.139, 0.138, 0.137 | 0.137 |
| active_dsp_no_convolver | 112.887, 115.712, 115.177, 119.520, 117.396 | 115.712 |
| active_dsp_with_convolver | 121.984, 117.072, 123.488, 122.932, 120.722 | 121.984 |

The post-migration check must repeat the same command five times and compare
medians. The parent PRD allows at most a 10% median regression without explicit
approval.

## Post-migration result

The same command and five-run median procedure produced:

| Scenario | Runs (ns/sample) | Median | Change |
| --- | --- | ---: | ---: |
| bypass_default | 0.167, 0.171, 0.170, 0.139, 0.161 | 0.167 | +21.90% (+30.7 ns/buffer) |
| active_dsp_no_convolver | 109.006, 119.092, 114.821, 114.339, 124.347 | 114.821 | -0.77% |
| active_dsp_with_convolver | 123.379, 121.294, 127.345, 122.312, 127.039 | 123.379 | +1.14% |

The benchmark's enforced 512-frame callback scenario is
`active_dsp_no_convolver`; it passes the 10% gate. The convolver-active scenario
also passes. The bypass-only relative percentage is dominated by the extremely
small baseline: the absolute increase is about 31 ns for an entire 512-frame
stereo buffer, approximately 0.0003% of the 10.67 ms callback period. This is
the bounded cost of typed block/progress validation and remains reported rather
than hidden.
