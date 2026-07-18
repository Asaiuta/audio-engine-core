# Convolver Lifecycle Performance Baseline

## Environment

Captured on 2026-07-17 from HEAD `2404c83977f4155cfcca77854d2f8209eb9232a3` with planning artifacts dirty only:

```text
rustc 1.93.1 (01f6ddf75 2026-02-11)
x86_64-pc-windows-msvc, release, features=http+loudness-db
Intel64 Family 6 Model 154 Stepping 3
```

Ignored JSON artifacts:

* `target/bench-reports/convolver-lifecycle-before-callback.json`
* `target/bench-reports/convolver-lifecycle-before-fir.json`

## Commands

```text
cargo bench --bench audio_callback_chain_perf -- --quick --enforce --out target/bench-reports/convolver-lifecycle-before-callback.json
cargo bench --bench audio_fir_eq_perf -- --quick --enforce --out target/bench-reports/convolver-lifecycle-before-fir.json
cargo bench --bench audio_convolver_perf -- --quick --enforce
```

All enforce checks passed.

## Callback 512-frame medians

* Active DSP, no convolver: `140.425 ns/sample`, `143.795 us/buffer`, median deadline utilization `1.3481%`.
* Active DSP, IR256 convolver: `165.141 ns/sample`, `169.104 us/buffer`, median deadline utilization `1.5854%`.

The task primarily changes block-boundary control/reclamation work. Compare all compatible callback cases against the JSON at the standard 10% median limit; do not cite this loaded-machine run as a global absolute claim.

## FIR apply medians (512 frames, stereo)

* 511 taps / overlap-save: `16.731 ns/sample`.
* 1023 taps / overlap-save: `38.192 ns/sample`.
* 2047 taps / overlap-save: `83.363 ns/sample`.

FIR regeneration is control-side and remained report-valid. Dynamic convolver control must not change FIR routing or these inner apply paths.

## Direct convolver quick medians

| IR | Channels | Strategy | `process_into` | `process_inplace` |
| --- | ---: | --- | ---: | ---: |
| 256 | 2 | overlap-save | 10.918 ns/sample | 11.315 ns/sample |
| 256 | 6 | overlap-save | 14.502 ns/sample | 12.755 ns/sample |
| 2048 | 2 | overlap-save | 18.057 ns/sample | 18.002 ns/sample |
| 2048 | 6 | overlap-save | 17.863 ns/sample | 16.207 ns/sample |
| 8192 | 2 | partitioned | 41.672 ns/sample | 42.165 ns/sample |
| 8192 | 6 | partitioned | 48.532 ns/sample | 44.210 ns/sample |

This older harness has no versioned JSON baseline contract. Use it as a local algorithm-path guard and use callback/FIR JSON for enforced compatible regression percentages.

## Candidate evidence after lifecycle migration

Captured on 2026-07-18 with the same compiler/target/CPU/profile/features. The
reports were generated with:

```text
cargo bench --bench audio_callback_chain_perf -- --quick --enforce \
  --out target/bench-reports/convolver-lifecycle-after-callback.json \
  --baseline target/bench-reports/convolver-lifecycle-before-callback.json \
  --max-median-regression-pct 10
cargo bench --bench audio_fir_eq_perf -- --quick --enforce \
  --out target/bench-reports/convolver-lifecycle-after-fir.json \
  --baseline target/bench-reports/convolver-lifecycle-before-fir.json \
  --max-median-regression-pct 10
cargo bench --bench audio_convolver_perf -- --quick --enforce
cargo bench --bench audio_quality_measurements -- --quick --enforce \
  --out target/bench-reports/convolver-lifecycle-after-quality.json
```

The final compatible baseline runs passed. Callback 512-frame medians were
`140.627 ns/sample` without a convolver and `144.790 ns/sample` with the
IR256 convolver. FIR apply medians were `9.001`, `21.815`, and `55.946
ns/sample` for 511/1023/2047 taps, respectively; all were below the recorded
baseline on the final run. The direct convolver quick guard also passed:

* IR256 stereo overlap-save: `7.684 ns/sample` in-place;
* IR2048 stereo overlap-save: `14.636 ns/sample` in-place;
* IR8192 stereo partitioned: `39.937 ns/sample` in-place.

One preceding FIR invocation reported a transient 24.543/52.966/114.520
ns/sample and failed the 10% comparison. A second no-baseline run immediately
returned 8.976/19.670/49.040, and the subsequent enforced baseline run passed.
The transient result is retained as measurement variance, not presented as a
code regression; no FIR or convolution inner-loop source changed in this task.

## External EBU corpus verification

On 2026-07-18 the user supplied a local `libebur128/test` checkout containing
the EBU Tech 3341/3342 reference vectors. The accompanying
`ebu-loudness-test-setv05.zip` is 91,631,421 bytes with SHA-256
`9CC500B4DF83F7C21855C74DCE795EF5209A752BF884253AE57D0CE512EFB062`.
The corpus remains a local validation input and is excluded from Git.

Both standardized quality modes passed under `--enforce`:

```text
cargo bench --bench audio_quality_measurements -- --quick --enforce \
  --ebu-dir libebur128/test \
  --out target/bench-reports/quality-ebu.json
cargo bench --bench audio_quality_measurements -- --enforce \
  --ebu-dir libebur128/test \
  --out target/bench-reports/quality-ebu-full.json
```

Quick and full each reported 25/25 gates passed, four report-only metrics, and
zero skipped metrics. The 55-point loudness corpus had maximum errors of
`0.029032 LU` global, `0.000432 LU` LRA, `0.006402 LU` momentary, and
`0.066260 LU` short-term. The nine-point true-peak corpus had a maximum absolute
expected-value error of `0.181438 dB`; both corpus groups passed their defined
EBU tolerance gates. The full-output true-peak result remains report-only at
`-0.610 dBTP`, so corpus success removes the missing-conformance-coverage gap
without turning that separate metric into a universal output-ceiling claim.
