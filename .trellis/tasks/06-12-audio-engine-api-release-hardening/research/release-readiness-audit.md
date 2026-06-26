# Release Readiness Audit

Date: 2026-06-26

## Scope

Audited the crate release surface after the DSP roadmap work:

- `Cargo.toml` package metadata and feature flags
- `src/lib.rs` and `src/processor/mod.rs` public re-exports
- `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, and `NOTICE`
- `examples/resample_sine.rs` and `examples/equalizer_curve.rs`
- Release validation commands from this task PRD

## Findings

- Package metadata is present: name, version, description, license, repository,
  homepage, documentation, readme, keywords, and categories.
- Feature gates build independently: default, no-default-features,
  `http` alone, and `loudness-db` alone all build on this workspace.
- `loudness-db` exports stay feature-gated. `http` gates `NetworkError` and
  network source internals, while `HttpCredentials` remains a harmless always
  compiled type so the decoder API shape does not change more than necessary.
- `cargo doc --no-deps` exposed one rustdoc warning: `TruePeakDetector`
  publicly linked to the private `Self::intersample_peak` helper. Fixed by
  removing the private intra-doc link while keeping the contract text.
- README quality evidence was missing the latest listening-DSP metrics from
  `audio_quality_measurements`; added rows for EQ target accuracy, crossfeed
  response/continuity, and dynamic-loudness compensation.
- README and NOTICE overstated SoXR optionality. `soxr` is a required
  dependency today, so `default-features = false` only removes HTTP/SQLite
  dependencies. Updated README, CONTRIBUTING, and NOTICE to make this explicit.
- CHANGELOG lagged behind the completed roadmap. Added user-facing entries for
  channel layout/downmixing, true-peak limiter default mode, oversampled
  saturation modes, partitioned convolution, canonical output-chain builders,
  decoder seek/error fixes, and listening-DSP evidence.
- Full output-chain true peak remains report-only. Current quick evidence shows
  `worst_output_true_peak_dbtp=-0.610`, which is still 0.390 dB above the
  -1 dBTP limiter target after downstream resampling/quantization. README keeps
  this limitation visible instead of promoting the metric to a gate.

## Current Benchmark Evidence

Command:

```bash
cargo bench --bench audio_quality_measurements -- --quick --enforce
```

Key output from this workspace:

- Resampler THD+N, 44.1 kHz to 48 kHz: `-187.01 dB`
- Passband max deviation, 20 Hz to 18 kHz: `0.0013 dB`
- Worst fitted alias attenuation, 96 kHz to 48 kHz: `-297.02 dB`
- Saturation alias-energy reduction, Direct vs `Oversampled4x` Tube stress:
  `16.56 dB`
- True-peak limiter intersample stress: input `+0.10 dBTP`, true-peak-mode
  output `-1.00 dBTP`, sample-peak-mode output `+0.10 dBTP`
- EQ +6 dB target response max error: `0.0000 dB`
- Crossfeed: low band `-46.81 dB`, high band `-9.18 dB`, low-vs-high
  attenuation `-37.63 dB`
- Crossfeed mix-change continuity: preserved `0.000e0`, legacy reset
  simulation `7.992e-3`
- Dynamic loudness low-volume compensation: `+8.23 dB` at 40 Hz and `+2.83 dB`
  at 3 kHz
- EBU loudness and true-peak corpora: skipped because reference files are not
  bundled (`missing_files=53` and `missing_files=9`)
- Full output-chain true peak: report-only, `worst_output_true_peak_dbtp=-0.610`

## Final Validation

Required PRD commands:

- `cargo build` passed.
- `cargo build --no-default-features` passed.
- `cargo build --no-default-features --features http` passed.
- `cargo build --no-default-features --features loudness-db` passed.
- `cargo test --all-features` passed: 218 unit tests, 1 doctest passed, 1
  doctest ignored.
- `cargo doc --no-deps` passed with no warnings after the rustdoc link fix.
- `cargo run --example resample_sine` passed: 48000 frames at 48 kHz rendered
  to 43471 frames at 44.1 kHz (expected approximately 44100; within the
  example's filter-tail margin).
- `cargo run --example equalizer_curve` passed: output peak `0.5000`.
- `cargo package --allow-dirty` passed in the non-sandbox environment:
  packaged 160 files, 1.3 MiB uncompressed, 317.1 KiB compressed, and verified
  the packaged crate.

Additional release-hardening checks:

- `cargo fmt --check` passed.
- `cargo test --no-default-features` passed: 210 unit tests, 1 doctest passed,
  1 doctest ignored.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` passed.
- `python ./.trellis/scripts/task.py validate .trellis/tasks/06-12-audio-engine-api-release-hardening`
  passed with 7 `implement.jsonl` entries and 7 `check.jsonl` entries.

Packaging note: the first sandboxed `cargo package --allow-dirty` attempt hung
on repeated crates.io index SSL credential failures
(`SEC_E_NO_CREDENTIALS`). Rerunning the same command with non-sandbox
permissions completed successfully, so the blocker was the sandbox network /
credential boundary rather than package contents.


Final validation after edits should rerun the task PRD command set.
