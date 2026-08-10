# Validation Evidence: 2026-08-10

Task: `08-01-unify-parameter-validation-policy`
Branch: `chore/gate2-legacy-public-surface`

## Release-gate checks

The implementation and its migrated callers were already verified in both
feature matrices before this closeout:

- `cargo check --all-features`
- `cargo check --all-features --benches`
- focused loudness tests: 58 passed
- focused AutoMix tests: 16 passed
- `cargo fmt --all -- --check`
- strict Clippy for all features and Rubato-only features
- public API snapshot checks for `tests/public-api-all-features.txt` and
  `tests/public-api-rubato.txt`
- all-feature matrix: 494 library tests, 20 benchmark-support tests, 2 API
  tests, 25 resampler-support tests with 1 expected ignored, 3 Windows
  deployment tests, and 7 doctests passed
- Rubato-only matrix: 474 library tests, the same supporting suites, and 7
  doctests passed

## Closeout reruns

- `$env:RUSTDOCFLAGS='-D warnings'; cargo doc --no-deps --all-features`
  passed and generated `target/doc/audio_engine_core/index.html`.
- `cargo package --all-features --allow-dirty --offline` passed: 766 files
  packaged and the package verification build completed. An online package
  attempt was not used as evidence because the local Windows TLS credential
  provider could not establish the crates.io connection.

## Focused component benchmark

Command:

```text
cargo bench --all-features --bench audio_component_perf -- --quick --enforce --out .trellis/tasks/08-01-unify-parameter-validation-policy/research/audio-component-perf-quick.json
```

The run completed with 16 valid cases. Both loudness rows completed their
work-validation checks:

- 512-frame stereo input: `valid=true`, median `578.830 ns/input-sample`.
- 4096-frame stereo input: `valid=true`, median `148.785 ns/input-sample`.

The JSON report is retained beside this note. Its environment records a dirty
working tree because the task changes are intentionally uncommitted.
