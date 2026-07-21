# Final rubato 4 quality-aware routing evidence

Date: 2026-07-22

Environment: Windows 11, Intel Alder Lake, `x86_64-pc-windows-msvc`, rustc
1.93.1, release profile, `--no-default-features --features rubato`.

## Final High performance

Command:

```text
cargo bench --bench audio_resampler_streaming_perf --no-default-features --features rubato -- --quick --enforce --out .trellis/tasks/07-21-rubato-resampler-performance/research/resampler-rubato-quality-aware-quick.json
```

512-frame `process_checked` seven-trial medians:

| Conversion | Old rubato sinc | Final rubato High FFT | SoXR reference |
| --- | ---: | ---: | ---: |
| 44.1 kHz to 48 kHz | 133.59 ns/input sample | 9.86 ns/input sample | 8.45 ns/input sample |
| 48 kHz to 96 kHz | 179.59 ns/input sample | 12.57 ns/input sample | 6.73 ns/input sample |

The final report labels the algorithm as
`rubato_streaming_quality_aware_fft_sinc`, so the old all-common-ratio FFT and
sinc reports cannot be accepted as compatible performance baselines.

## Final UltraHigh quality

Command:

```text
cargo bench --bench audio_quality_measurements --no-default-features --features rubato -- --quick --enforce --out .trellis/tasks/07-21-rubato-resampler-performance/research/quality-rubato-quality-aware-quick.json
```

All 27 gates passed with zero skips. Resampler evidence was:

| Metric | Final rubato UltraHigh sinc |
| --- | ---: |
| 44.1 kHz to 48 kHz THD+N | -216.24 dB |
| Passband maximum deviation, 20 Hz to 18 kHz | 0.0000 dB |
| 20 kHz gain | -0.0017 dB |
| Worst fitted alias attenuation, 96 kHz to 48 kHz | -208.11 dB |

The quality bench explicitly requests UltraHigh. Routing that tier through
rubato 4 sinc restores the old sinc evidence (-216.2 dB THD+N) while leaving
the public default High tier on FFT.

## FFT tuning decision

The same-machine 512-frame `process_checked` sweep was:

| FFT configuration | 44.1 kHz to 48 kHz | 48 kHz to 96 kHz | Decision |
| --- | ---: | ---: | --- |
| 1024-frame chunk, 1 sub-chunk | 35.10 ns/input sample | 19.20 ns/input sample | Reject: misses the 44.1 to 48 kHz target |
| 1024-frame chunk, 2 sub-chunks | 9.86 ns/input sample | 12.57 ns/input sample | Keep |
| 1024-frame chunk, 4 sub-chunks | 9.14 ns/input sample | 14.59 ns/input sample | Reject: regresses 48 to 96 kHz |

Changing the window does not reduce runtime FFT work; it changes only the
precomputed filter coefficients and quality tradeoff. Smaller chunks paired
with proportionally fewer sub-chunks reproduce the same FFT unit sizes while
increasing adapter-call frequency. The evidence-backed final configuration
therefore remains 1024 frames, two sub-chunks, and `BlackmanHarris2`.

## UltraHigh output-render cost

`OutputRenderChain` requests UltraHigh, so its resampled rubato scenario uses
sinc. Quick output-render reports were generated for the final route and a
temporary all-common-ratio FFT diagnostic reference:

| Active 44.1 kHz to 48 kHz render | Quality-aware UltraHigh sinc | All-FFT diagnostic reference |
| --- | ---: | ---: |
| 1 second, 4096-frame blocks | 353.97 ns/input sample | 126.64 ns/input sample |
| 5 seconds, 4096-frame blocks | 266.38 ns/input sample | 93.65 ns/input sample |
| Setup bytes | 2,972,058 | 1,054,394 |

This is a roughly 2.8x offline-render CPU and setup-memory cost for restoring
the strict UltraHigh quality tier. The final route still measured only 3.12%
and 2.35% realtime factors for the one- and five-second cases. Evidence files:
`output-render-rubato-quality-aware-quick.json` and
`output-render-rubato-all-fft-reference-quick.json`.
