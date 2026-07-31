# Phase-1 cross-project resampler results (historical)

> This is phase-1 evidence only. It covers 4 of the 11 required projects and
> must not be cited as the final or complete comparison matrix. The active PRD
> requires terminal coverage for FFmpeg libswresample, SpeexDSP, r8brain,
> zita-resampler, WebRTC, WDL, and libresample as well.

Date: 2026-07-26

## Scope

The phase-1 benchmark compares the active `audio-engine-core` streaming resampler,
raw libsoxr, raw Rubato FFT, and runtime-loaded libsamplerate
`SRC_SINC_BEST_QUALITY`. It measures setup, steady processing, reset, drain,
complete-stream work, API-visible latency, passband gain, THD+N, and folded
alias. It does not cover FFmpeg libswresample, SpeexDSP, a decoder/player
pipeline, or device/driver latency.

## Formal commands

```powershell
cargo bench --bench audio_resampler_comparison_perf --all-features -- `
  --quick --enforce `
  --libsamplerate "D:\AI\audio-engine-core\target\benchmark-deps\libsamplerate-0.2.2-1\mingw64\bin\libsamplerate-0.dll" `
  --require-engine libsamplerate `
  --out target\audio-resampler-comparison-libsamplerate-quick.json

cargo bench --bench audio_resampler_comparison_perf `
  --no-default-features --features rubato -- `
  --quick --enforce `
  --libsamplerate "D:\AI\audio-engine-core\target\benchmark-deps\libsamplerate-0.2.2-1\mingw64\bin\libsamplerate-0.dll" `
  --require-engine libsamplerate `
  --out target\audio-resampler-comparison-rubato-libsamplerate-quick.json
```

Both commands exited zero. The all-features report contains eight valid cases
and no unavailable engines. The Rubato-only report contains six valid cases
and explicitly records raw libsoxr as `skipped` because the `soxr` feature was
not compiled. It does not count that engine as passed.

## Environment

- Revision: `342fd447c4c92025c86497b3cfb0d729559046ab`, dirty worktree
- Compiler: `rustc 1.93.1 (01f6ddf75 2026-02-11)`
- Target/profile: `x86_64-pc-windows-msvc`, release
- CPU: Intel Family 6 Model 154 Stepping 3
- Workload: stereo, 512 input frames, 32 warm-up buffers, 200 iterations,
  seven alternating-order trials
- Quality input: 16,384 frames per signal

## All-features quick report

Primary steady values are median nanoseconds per input sample. Setup, reset,
and drain are median microseconds.

| Rate | Engine | Steady | p95 | Setup | Reset | Drain | Latency | THD+N | Alias |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 44.1 to 48 kHz | project SoXR | 13.892 | 20.742 | 642.0 | 508.9 | 126.8 | 0 | -134.31 dB | n/a |
| 44.1 to 48 kHz | raw libsoxr | 9.351 | 18.275 | 289.3 | 342.9 | 108.5 | 0 | -134.31 dB | n/a |
| 44.1 to 48 kHz | raw Rubato | 12.661 | 23.032 | 98.3 | 1.5 | 8.3 | 160 | -196.54 dB | n/a |
| 44.1 to 48 kHz | libsamplerate | 502.375 | 602.813 | 223.8 | 60.6 | 1.0 | 0 | -149.43 dB | n/a |
| 48 to 44.1 kHz | project SoXR | 9.880 | 13.784 | 624.7 | 567.5 | 87.4 | 0 | -134.38 dB | -137.67 dB |
| 48 to 44.1 kHz | raw libsoxr | 8.130 | 8.400 | 313.3 | 301.0 | 83.9 | 0 | -134.38 dB | -137.67 dB |
| 48 to 44.1 kHz | raw Rubato | 8.722 | 10.270 | 82.4 | 1.0 | 6.6 | 147 | -206.60 dB | -202.86 dB |
| 48 to 44.1 kHz | libsamplerate | 394.557 | 427.547 | 209.4 | 54.7 | 0.9 | 0 | -145.61 dB | -163.66 dB |

All engines consumed the complete input and produced the exact finite stream
defined by their lifecycle. The project, raw libsoxr, and libsamplerate rows
produced duration-aligned 17,833/15,053-frame streams. Raw Rubato deliberately
preserved its 160/147-frame leading delay and produced 17,993/15,200 frames.
The project/raw-libsoxr comparison is the closest wrapper control because both
use the same f64 lane and measured quality recipe. The project wrapper was
48.6% and 21.5% slower than raw libsoxr in this run.

## Rubato-only quick report

| Rate | Engine | Steady | p95 | Setup | Reset | Drain |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 44.1 to 48 kHz | project Rubato | 11.672 | 13.569 | 155.7 | 2.9 | 18.7 |
| 44.1 to 48 kHz | raw Rubato | 10.156 | 13.216 | 92.2 | 1.2 | 5.9 |
| 44.1 to 48 kHz | libsamplerate | 458.263 | 483.939 | 55.2 | 54.2 | 0.5 |
| 48 to 44.1 kHz | project Rubato | 10.628 | 17.423 | 186.4 | 3.3 | 33.6 |
| 48 to 44.1 kHz | raw Rubato | 11.334 | 17.003 | 127.5 | 1.5 | 9.4 |
| 48 to 44.1 kHz | libsamplerate | 521.583 | 610.446 | 58.1 | 58.3 | 0.7 |

Absolute timing must not be compared between this build and the all-features
build as though they were an AB experiment. The reports have incompatible
feature/backend identities.

## Pinned native dependency

- Package: `mingw-w64-x86_64-libsamplerate-0.2.2-1-any.pkg.tar.zst`
- Package SHA-256:
  `454b2d8eb1a22f8df2a84d10fa0244420fde55de877823062e93c192a551f8b6`
- Signature: verified with MSYS2 development key
  `5F944B027F7FE2091985AA2EFA11531AA0AA7F57`
- DLL SHA-256:
  `1e08aeb1fecade2cf2d7a83463a1b375e13d5f2f008cdeea7409a3eff7ed9a0e`
- DLL size: 1,502,619 bytes

The DLL and package remain under ignored `target/benchmark-deps/` storage and
are not repository artifacts.

## Failures found and fixed

1. Exact `serde_json::Value` equality rejected valid JSON float round trips.
   Report verification now keeps structure, strings, integers, and booleans
   exact while allowing finite floats to differ by at most four machine
   epsilons, with a field-path diagnostic and regression coverage.
2. libsamplerate initially ended 144 to 156 frames short. Its streaming API
   requires `end_of_input=1` on the final call that contains real input, not a
   later empty call. The adapter now submits the final real chunk correctly,
   produces exact complete output, and identifies the changed contract as
   `libsamplerate_sinc_best_quality_streaming_f32_v2`.
3. Strict Clippy found a large `EngineAdapter` enum variant and manual channel
   divisibility checks. The raw Rubato state is boxed at setup and geometry
   checks use `is_multiple_of`; the final reports were regenerated after that
   setup-affecting change.

## Verification

The final verification matrix is:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --no-default-features --features rubato -- -D warnings
cargo test --all-features
cargo test --no-default-features --features rubato
```

Every command above exited zero. The all-features run completed 351 library
tests, 18 shared benchmark-support tests, 13 comparison-support tests, three
Windows runtime tests, and two doctests. The Rubato-only run completed
387 + 18 + 12 + 3 + 2 tests in the same groups. There were no test failures.

The default and Rubato-only forms of
`cargo rustc --lib -- -D unused-crate-dependencies` also exited zero. An extra
diagnostic attempt with `--all-features` exited non-zero because the crate's
intentional feature precedence selects SoXR while the simultaneously enabled
`rubato` dependency is unused by the library target. This pre-existing feature
combination is not one of the maintained unused-dependency lanes.

A missing-DLL Rubato-only quick run exited zero, wrote JSON, and marked
libsamplerate `skipped` with `required=false`. Repeating it with
`--require-engine libsamplerate` wrote JSON with `required=true` and then
exited one with a named required-engine error.

Cross-engine speed remains report-only: libsamplerate is f32 while the other
rows are f64, quality recipes differ, and the quick reports are unpinned and
visibly sensitive to host load.
