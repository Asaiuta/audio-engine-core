# Verification Evidence

Date: 2026-07-19
Environment: Windows MSVC, `x86_64-pc-windows-msvc`, Rust `1.93.1`, release
profile, features `http,loudness-db`, Intel Family 6 Model 154.

## Correctness and Safety

- `cargo check --all-targets`
- `cargo test --lib`: 344 passed, 0 failed
- `cargo test --lib --no-default-features`: 336 passed, 0 failed
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo clippy --all-targets --no-default-features -- -D warnings`
- `cargo fmt --all -- --check`
- `cargo rustdoc --all-features --lib -- -D warnings`
- `cargo test --lib processor::adapters::convolver::tests:: -- --nocapture`:
  15 passed, including long partitioned adoption, reversal, exact tail, and
  new-audio-thread no-allocation coverage.
- Saturation focused suite: 25 passed.
- Public malformed-geometry regression: typed `FFTConvolver::new` error,
  no panic.
- Realtime snapshot concurrent-publication and new-thread no-allocation tests
  passed; all callback adapters use the registered hazard reader.

## Release Packaging

- `cargo build`
- `cargo build --no-default-features`
- `cargo build --no-default-features --features http`
- `cargo build --no-default-features --features loudness-db`
- `cargo package --allow-dirty --offline`: packaged 253 files and verified the
  package by compiling it successfully.

The ordinary `cargo package --allow-dirty` attempt reached package creation but
could not update the crates.io index because Windows Schannel returned
`SEC_E_NO_CREDENTIALS`; the offline package verification above isolates this as
an environment/network credential failure rather than a manifest or source
failure.

## Audio Quality

Commands:

```text
cargo bench --bench audio_quality_measurements -- --quick --enforce --out target/bench-reports/dsp-lifecycle-quality-check.json
cargo bench --bench audio_quality_measurements -- --enforce --out target/bench-reports/dsp-lifecycle-quality-full.json
```

Both runs report `27/27` gates passed, zero skipped corpus gates, 55 EBU
loudness files passed, and 9 EBU true-peak files passed. The full run reports
final output at or below `-1.0 dBTP`. Saturation 4x reports `16.32 dB` alias
reduction and `+0.35 dB` fundamental delta; the fundamental-preservation gate
is `>= -0.5 dB`.

## Callback Performance

Baseline and candidate reports use the same 16-case workload and environment:

- Baseline: `target/bench-reports/dsp-lifecycle-baseline-callback-compatible.json`
- Candidate: `target/bench-reports/dsp-lifecycle-candidate-callback-compared.json`

The enforced callback comparison passes every case. At 512 frames, active
chain median CPU changes are `-65.1%` without Convolver and `-45.8%` with the
256-tap Convolver. Relative p95 deadline utilization changes are `-71.8%` and
`-14.9%`. All four isolated Saturation 4x block sizes strictly improve
(`28.6%` to `54.1%`).

## Offline Performance and Memory

Baseline and candidate reports use the same 18-case scenario/duration/block
matrix:

- Baseline: `target/bench-reports/dsp-lifecycle-baseline-render-compatible.json`
- Candidate: `target/bench-reports/dsp-lifecycle-candidate-render-compared.json`

The enforced comparison passes CPU, work, and memory gates. Candidate fixed
stage temporary memory is about `131,712--131,776` bytes at 4096-frame blocks
and `2,688--2,752` bytes at 64-frame blocks, independent of render duration.
Five-second temporary-memory reductions are `96.6%` to `98.7%` at 4096-frame
blocks and over `99.6%` at 64-frame blocks. The one visible 1-second
44.1-to-48 kHz CPU comparison is `+9.4%` per input frame because the candidate
retains the newly required IIR/effect tail (53,652 output frames versus the
old 48,000); normalized per output frame it is lower. The 5-second case is
`-60.4%` per input frame.

## Public Contract Changes

- `FFTConvolver::new` is fallible and rejects malformed interleaved geometry.
- Convolver publications may carry an explicit sample-rate domain through
  `publish_at_rate`; mismatched kernels are never processed.
- Offline callers can select bounded `block_frames` through
  `render_with_policy_and_block_frames`.
- Saturation armed/hard-bypass state, 32-frame sparse automation, fixed
  four-frame timing, and nonlinear-residual oversampling are explicit.
