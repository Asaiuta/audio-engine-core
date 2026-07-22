# Rubato UltraHigh Native-Interleaved Final Evidence

## Decision

Retain the existing UltraHigh sinc parameters:

```text
sinc length: 256
oversampling factor: 512
interpolation: Cubic
window: BlackmanHarris2
```

Optimize the pure-Rubato `StreamingResampler` by constructing one native
interleaved Rubato engine for all channels. This shares the engine's sinc table
or FFT plan and removes adapter-side deinterleave, per-channel dispatch, and
reinterleave work. SoXR routing and the Rubato High FFT / UltraHigh sinc quality
policy are unchanged.

## Quality

The final quick quality run passed all 27 enforced gates. Its UltraHigh values
are identical to the retained Cubic/512 baseline at report precision.

| Metric | Retained baseline | Final native interleaved |
| --- | ---: | ---: |
| 44.1 -> 48 kHz THD+N | -216.24274427102165 dB | -216.24274427102165 dB |
| 20 Hz-18 kHz max passband deviation | 8.219599453172674e-10 dB | 8.219599453172674e-10 dB |
| Worst 96 -> 48 kHz alias attenuation | -208.11158491686967 dB | -208.11158491686967 dB |

Evidence:

- `quality-native-interleaved-final-quick.json`
- `quality-sinc-cubic512-native-stereo-quick.json`

## Noise-Aware ABBA Performance

The same compiled Cubic/512 binary was measured in B-A-A-B order. B selects
the diagnostic legacy one-backend-per-channel path; A selects native stereo.
The environment switch existed only for this measurement and is absent from
the retained implementation.

| Case | Legacy B1 | Native A1 | Native A2 | Legacy B2 | Legacy mean | Native mean | Improvement |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4096f, 1s | 378.416 | 246.819 | 227.532 | 333.451 | 355.934 | 237.175 | 33.4% |
| 4096f, 5s | 293.059 | 184.044 | 182.484 | 247.894 | 270.477 | 183.264 | 32.2% |
| 64f, 1s | 677.046 | 245.658 | 246.491 | 492.702 | 584.874 | 246.074 | 57.9% |

Subtracting the matching active equal-rate chain median isolates the resampler
increment more conservatively:

| Case | Legacy increment | Native increment | Improvement |
| --- | ---: | ---: | ---: |
| 4096f, 1s | 259.522 ns/input sample | 146.014 ns/input sample | 43.7% |
| 4096f, 5s | 199.572 ns/input sample | 117.666 ns/input sample | 41.0% |
| 64f, 1s | 458.646 ns/input sample | 165.662 ns/input sample | 63.9% |

The final post-cleanup spot run measured 234.672 and 168.078 ns/input sample
for the 4096-frame 1-second and 5-second cases, and 247.015 for the 64-frame
1-second case. These values agree with the retained ABBA distribution.

Evidence:

- `output-render-abba-b1-legacy-mono.json`
- `output-render-abba-a1-native-stereo.json`
- `output-render-abba-a2-native-stereo.json`
- `output-render-abba-b2-legacy-mono.json`
- `output-render-native-interleaved-final-quick.json`

## Setup Memory

The native multichannel engine avoids duplicate sinc tables.

| Architecture | Steady-state chain setup bytes | Change |
| --- | ---: | ---: |
| Legacy mono instances | 2,973,658 | reference |
| Native stereo engine, ABBA build | 1,218,898 | -59.0% |
| Native stereo engine, final build | 1,218,570 | -59.0% |

Rubato owns its engine buffers internally, so
`StreamingResampler::working_buffer_bytes()` now reports zero adapter scratch
for the pure-Rust backend. Setup allocation evidence above captures the actual
engine memory.

## High Streaming Guard

The final `process_checked` quick run with 512-frame stereo blocks remained
inside the 20 ns/input-sample target:

| Conversion | 07-21 retained | Candidate run | Final run | Target |
| --- | ---: | ---: | ---: | ---: |
| 44.1 -> 48 kHz | 9.86 | 7.427 | 7.424 | <= 20 |
| 48 -> 96 kHz | 12.57 | 10.801 | 13.202 | <= 20 |

The 48 -> 96 quick median varied between runs but remained comfortably inside
the gate. Evidence:

- `resampler-streaming-native-multichannel-quick.json`
- `resampler-streaming-native-interleaved-final-quick.json`

## Rejected Candidates

| Candidate | Result | Decision |
| --- | --- | --- |
| All common ratios through FFT | 126.64 / 93.65 ns/input sample, but only -200.6337 dB THD+N | Reject: changes UltraHigh numerical meaning |
| Quadratic/512 | -216.2389705751317 dB THD+N | Reject: measurable regression from retained value |
| Quadratic/1024, mono instances | 283.803 / 211.992 ns/input sample; 5,095,370 setup bytes | Reject: setup memory grows materially |
| Quadratic/1024, native stereo | 252.930 / 181.894 ns/input sample; 2,279,762 setup bytes | Reject: changes retained parameters and uses more memory than Cubic/512 native |
| Linear table, 8192-32768 oversampling | THD+N from -213.86 to -216.23 dB; 16,384 setup reached 68,747,210 bytes | Reject: worse quality and excessive table memory |
| Rayon offline channel parallelism | 664.155 / 560.521 ns/input sample | Reject: overhead dominates this workload |

The all-FFT reference is recorded under the archived 07-21 task. The remaining
candidate JSON files are in this task's `research/` directory.

## Verification

The retained implementation passed:

- `cargo fmt --all -- --check`
- `cargo check --no-default-features --features rubato`
- `cargo test --lib --no-default-features --features rubato` (347 tests)
- `cargo test --lib` (350 tests)
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`
- `cargo clippy --all-targets --all-features -- -D warnings`
- Quick quality benchmark with `--enforce` (27/27 gates)
- Quick output-render benchmark with `--enforce`
- Quick streaming-resampler benchmark with `--enforce`

Backend coverage also compares one native two-channel Rubato engine against two
independent mono engines for both High FFT and UltraHigh sinc. Output lengths
must match and every sample must stay within `1e-14` absolute error.
