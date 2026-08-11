# Gate 4 Validation Evidence

## Scope

This evidence closes release gate 4 after the typed `MediaLocation` boundary,
AutoMix migration, and typed loudness-cache identity/schema work. Quick
benchmarks below establish work-valid cases only; they do not support a release
performance or end-to-end playback claim.

## Test And Lint Matrices

- `cargo test --all-features`: passed, 470 tests.
- `cargo test --no-default-features --features rubato`: passed, 490 tests.
- Focused loudness database tests: passed, 14 tests.
- Focused decoder tests: passed, 24 all-feature tests and 20 Rubato-only tests.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`:
  initially found that the component benchmark gated its `MediaLocation` import
  behind `loudness-db`; the import was split and the strict matrix then passed.
- `cargo fmt --all -- --check`: passed after the fix and spec update.
- `git diff --check`: passed after the fix and spec update.

## Feature, Documentation, And Package Matrices

- `cargo check --all-features`: passed.
- `cargo check --no-default-features --features rubato,http`: passed.
- `cargo check --no-default-features --features rubato,loudness-db`: passed.
- `cargo doc --no-deps`: passed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`: passed.
- `cargo package --allow-dirty`: passed; Cargo packaged 760 files (30.1 MiB,
  3.6 MiB compressed) and rebuilt the generated crate. The first sandboxed
  attempt timed out on crates.io Schannel credentials; an approved network
  retry completed, so this was an environment access failure rather than a
  package defect.

## Public API

- Snapshots were regenerated deliberately with
  `UPDATE_SNAPSHOTS=1 cargo test --test public_api` after the API migration.
- A final non-updating `cargo test --test public_api` passed both the
  all-features and Rubato-only committed-surface checks (2/2).

## Focused Benchmark Gates

- `cargo bench --bench audio_component_perf -- --quick --enforce`: passed all
  16 cases with `valid=true`, including AutoMix and the typed loudness database
  operations.
- `cargo bench --bench audio_decoder_perf -- --quick --enforce`: passed all 7
  cases with `valid=true`, including local source open, probe, decoder build,
  first/steady decode, and seek cases. Allocation telemetry was emitted.

## Static Boundary Review

- No source-routing prefix or lossy-UTF-8 classification remained under
  `src/`, `benches/`, or `tests/`. The only `starts_with("http...")` match was
  an assertion for the `http:sha256:` cache namespace.
- Every HTTP lifecycle log in `src/decoder/source/http.rs` formats the typed
  origin-only identity or numeric transfer data; none formats the request URL.
- Backend specs now record typed routing, native local path preservation,
  namespaced cache IDs, validator-less HTTP staleness, and explicit v2 SQLite
  schema invalidation.
