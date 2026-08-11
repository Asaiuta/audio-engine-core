# Gate 5 Validation Evidence

## Scope

This evidence closes release gate 5 after narrowing the public streaming
lifecycle, adding fixed in-place chain admission, rejecting invalid chain rates
at construction, and assigning source-rate ownership to offline render
operations. Quick benchmarks establish valid work and allocation/lifecycle
behavior only; they do not establish release performance or device latency.

## Test And Lint Matrices

- `cargo test --all-features`: passed, including 469 unit tests, 20
  benchmark-support tests, public API tests, integration tests, and doctests.
- `cargo test --no-default-features --features rubato`: passed, including 489
  unit tests and the remaining Rubato-only matrix.
- Focused trait, `DspChain`, output-chain, and pipeline tests: passed.
- The `FixedInPlaceProcessor` compile-fail doctest proves that
  `StreamingResampler` cannot be inserted into `DspChain`.
- `cargo check --all-targets --all-features`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`:
  passed.
- `cargo fmt --all -- --check`: passed.

## Public API, Documentation, And Package

- Both public API snapshots were regenerated deliberately after the breaking
  migration. Final `cargo test --test public_api` passed both committed-surface
  checks (2/2).
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`: passed.
- `cargo package --allow-dirty --offline`: passed; Cargo packaged 763 files
  (30.1 MiB, 3.6 MiB compressed) and rebuilt the generated crate.
- The first online package attempt timed out while crates.io index access
  repeatedly failed with Windows Schannel `SEC_E_NO_CREDENTIALS`. The offline
  package and isolated rebuild passed from the resolved local dependency set,
  so no package-content failure was observed.

## Focused Benchmark Gates

- `audio_callback_tail_perf --quick --enforce`: passed all 12 cases with
  `valid=true`, zero missed callback deadlines, and report output at
  `target/bench-reports/gate5-callback-tail.json`.
- `audio_output_render_perf --quick --enforce`: passed all 18 equal-rate,
  active-stage, convolver-tail, and 44.1-to-48 kHz cases with `valid=true`; the
  report is `target/bench-reports/gate5-render.json`.
- `audio_lifecycle_memory_perf --quick --enforce`: passed all 13 timing cases,
  every required allocation-free operation, and the bounded lifecycle soak;
  the report is `target/bench-reports/gate5-lifecycle-memory.json`.

## Static Contract Review

- `StreamingProcessor` no longer contains `is_enabled`, `supports_bypass`, or
  `set_enabled`; effect controls remain on concrete atomic parameters and
  `ConvolverControl`.
- All seven fixed adapters plus `ConvolverProcessor` implement
  `FixedInPlaceProcessor`; `StreamingResampler` does not.
- `DspChain::{new,with_capacity}` return `Result`, reject zero Hz, and no
  `Default` implementation remains. `DspChain::add` requires the marker and
  retains its defensive output-rate check.
- `OutputChainParams` contains only `output_sample_rate`. Callback construction
  consumes that device rate; both offline render builders receive the source
  rate explicitly.
- `streaming-lifecycle.md`, README, changelog, benches, pipeline callers,
  exports, tests, and both public API snapshots describe the same contract.
