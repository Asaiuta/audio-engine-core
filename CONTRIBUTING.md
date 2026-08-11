# Contributing to audio-engine-core

Thanks for your interest in improving `audio-engine-core`. This crate provides
app-agnostic audio decoding, DSP, loudness, resampling, and streaming-pipeline
primitives. It is consumed by the Lyne audio player but is designed to stand on
its own.

## Development setup

The default build links the native
[libsoxr](https://sourceforge.net/projects/soxr/) resampling library. Install
SoXR when building the default/all-features matrix:

- **Debian/Ubuntu:** `sudo apt-get install libsoxr-dev`
- **macOS (Homebrew):** `brew install libsoxr`
- **Windows (MSYS2/MinGW64, CI path):**
  `pacman -S mingw-w64-x86_64-libsoxr mingw-w64-x86_64-pkgconf mingw-w64-x86_64-tools`;
  add `mingw64/bin` to `PATH` and set `PKG_CONFIG_PATH` to
  `mingw64/lib/pkgconfig`. The build script copies `libsoxr.dll` and the
  matching MinGW runtime closure into Cargo's binary, test, example, and
  benchmark output directories, so no extra runtime `PATH` setup is needed.
- **Windows (vcpkg alternative):** `vcpkg install soxr:x64-windows` or the
  static triplet used by your toolchain; set `VCPKG_ROOT` so the build script
  can find it.

For a fully pure-Rust build with no libsoxr dependency, select the alternate
backend explicitly:

```bash
cargo build --no-default-features --features rubato
```

Then the usual workflow:

```bash
cargo build
cargo test
```

## Cargo features

- `http` (default) — HTTP/HTTPS streaming decode via `reqwest`.
- `loudness-db` (default) — SQLite-backed loudness metadata persistence.
- `soxr` (default) — native SoX VHQ resampling; links LGPL-2.1 libsoxr.
- `rubato` — pure-Rust half-band/FFT/sinc/polyphase resampling. Enabling both
  backends is allowed for aggregate checks, but `soxr` wins at compile time.

The default optional services and SoXR backend are enabled for backward
compatibility. At least one resampler backend must be selected; when making
changes, verify both backend matrices plus optional feature combinations:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --no-default-features --features rubato -- -D warnings
cargo build --no-default-features --features rubato,http
cargo build --no-default-features --features rubato,loudness-db
cargo test --all-features
cargo test --no-default-features --features rubato
```

## Quality and performance evidence

Changes to streaming, output-chain, realtime DSP, or FIR design paths should run
the relevant quick evidence entry points and preserve their JSON artifacts:

```bash
cargo bench --bench audio_quality_measurements -- --quick --enforce --out target/bench-reports/quality.json
cargo bench --bench audio_callback_chain_perf -- --quick --enforce --out target/bench-reports/callback.json
cargo bench --bench audio_output_render_perf -- --quick --enforce --out target/bench-reports/render.json
cargo bench --bench audio_resampler_streaming_perf -- --quick --enforce --out target/bench-reports/resampler.json
cargo bench --bench audio_fir_eq_perf -- --quick --enforce --out target/bench-reports/fir-eq.json
```

Omit `--quick` for the full workload; callback, output render, resampler, and
FIR EQ also accept `--heavy`. Their reports include revision/dirty state, compiler/target,
OS/architecture/CPU, Cargo profile/features, stable case keys, raw trials, and
min/median/p95/max statistics. Passing `--baseline <json>` activates the
same-environment median comparison (10% maximum regression by default). Do not
compare incompatible or incompletely identified environments, or treat a
shared-runner absolute timing as a hard performance guarantee. Without a
baseline, performance `--enforce` checks work completion and report validity
only.

The quality report distinguishes `gate`, `report`, and `skipped`; a missing EBU
Tech 3341/3342 corpus is visible evidence of missing conformance coverage, not a
pass. The commands above are the ones most likely to be relevant to a change;
CI runs a wider set — nine quick gates on the default matrix and three more on
the pure-Rust matrix — and uploads every JSON report as a build artifact.

## Before submitting a change

1. `cargo fmt --all` — formatting must match `rustfmt`.
2. `cargo clippy --all-targets --all-features -- -D warnings` — zero warnings.
3. `cargo test --all-features` and
   `cargo test --no-default-features --features rubato` — all green. Bare
   `--no-default-features` is *expected* to fail: at least one resampler
   backend must be selected, and that guard is deliberate.
4. `cargo doc --no-deps --all-features` — docs build without warnings. Run it
   with `RUSTDOCFLAGS="-D warnings"` to match the gate CI applies; `cargo fmt`,
   Clippy, and the test suite do not catch a broken intra-doc link.
5. If you changed the public API, refresh the surface baseline (see below) and
   include the resulting diff in the same commit.
6. Add or update tests for any behavior change. DSP changes that touch the
   realtime path should preserve zero-allocation guarantees (see the
   `assert_no_alloc`-based tests).

Not every CI job runs everywhere. The `test` job is the only one that runs on
Linux, macOS, and Windows; formatting, Clippy, the docs.rs-parity doc build,
the MSRV check, the public API baseline, the benchmark gates, and the
`cargo package` publish dry-run all run on Linux only. See
`.github/workflows/ci.yml`.

## Public API baseline

`tests/public-api-all-features.txt` and `tests/public-api-rubato.txt` record
the public surface of both supported feature matrices. `tests/public_api.rs`
compares the current surface against them, so an intentional API change lands
as a reviewable text diff in the commit that makes it.

After an intentional change:

```bash
UPDATE_SNAPSHOTS=1 cargo test --test public_api
```

This needs the pinned nightly toolchain, because rustdoc JSON is nightly-only
and its format is unstable:

```bash
rustup toolchain install nightly-2026-07-09 --profile minimal
```

Without it the test skips rather than fails, so contributors on stable are not
blocked — but CI always runs it for real. The pinned date lives in
`PINNED_NIGHTLY` in `tests/public_api.rs` and in the `public-api` CI job; bump
both together with a snapshot refresh.

## Minimum supported Rust version

`rust-version` in `Cargo.toml` is the MSRV, and CI proves it by building both
feature matrices with exactly that toolchain. It covers the library only:
dev-dependencies used by the tests and benchmarks carry higher minimums, and
the MSRV is a promise to consumers, who build the library and nothing else.

Raising the MSRV is a deliberate change, not a side effect. If a dependency
bump or a new language feature would raise it, say so in the pull request and
in `CHANGELOG.md`.

## Realtime-safety conventions

The processing path is designed to run on the audio callback thread:

- No heap allocation in steady-state `process` calls — pre-allocate buffers up
  front and reuse them.
- No locks that can block the audio thread; prefer lock-free parameter handoff
  (see `lockfree_params`).
- Tests that assert allocation behavior use the `assert_no_alloc` crate; keep
  them passing.

## Semantic versioning policy

This crate follows [Semantic Versioning](https://semver.org/) with the 1.x
contract:

- **Breaking changes** (removing or changing public items, altering documented
  behavior) bump the **major** version (`1.0` → `2.0`).
- **New features and non-breaking additions** bump the **minor** version
  (`1.0.0` → `1.1.0`).
- **Bug fixes and documentation-only changes** bump the **patch** version
  (`1.0.0` → `1.0.1`).

The public API surface is everything re-exported from `lib.rs` and the public
items of the `config`, `decoder`, `diagnostics`, `pipeline`, `processor`, and
`runtime` modules. Adding a default-on Cargo feature, or making a previously
required dependency optional behind a default-on feature, is **not** considered
a breaking change because existing default builds are unaffected. Turning a
previously optional dependency **on** by default is reviewed as if it were
breaking, because it changes what existing default builds link (as `soxr` did
at 1.0.0, adding the LGPL-2.1 libsoxr build requirement). Both the SemVer
guarantee and the MSRV (see above) are enforced by CI: any breaking change
fails `cargo-semver-checks` against the committed rustdoc-JSON baselines, and
raising the MSRV is a deliberate, changelog-recorded decision.

## Publishing a release

Releases are cut from a green CI state on the release branch:

1. Bump the version in `Cargo.toml`, move the `[Unreleased]` changelog entries
   into a dated release section, and refresh README status text if it changed.
2. Verify the release locally with the commands in the Release Documentation
   Checklist (`.trellis/spec/backend/quality-guidelines.md`): both feature
   matrices build/test/clippy/doc with `-D warnings`, `cargo test --test
   public_api` matches the committed snapshots, `cargo semver-checks` passes
   for both matrices against the committed baselines, and
   `cargo package --allow-dirty` verifies the package.
3. Commit the release changes, then create an annotated tag:
   `git tag -a v1.0.0 -m "audio-engine-core 1.0.0"`.
4. Push the branch and tags (`git push origin <branch> --tags`); changelog
   compare links resolve only after the tag lands on origin.
5. Publish from the tagged commit: `cargo login` (once per machine, token from
   the crates.io account settings) followed by `cargo publish --dry-run` and
   then `cargo publish`.
6. Verify `https://docs.rs/audio-engine-core/<version>` builds the documented
   surface (the docs.rs parity CI job mirrors its environment) and the GitHub
   release notes link the changelog entry.

After 1.0.0, `cargo semver-checks` may switch from the committed rustdoc-JSON
baselines to the registry baseline (`--baseline-version`); that switch is a
documented CI change, not an automatic one.

## Licensing

Contributions are dual-licensed under [MIT](LICENSE-MIT) OR
[Apache-2.0](LICENSE-APACHE), at the user's option. By submitting a change you
agree to license your contribution under these terms. Note that the native
libsoxr dependency is LGPL-2.1 — see [`NOTICE`](NOTICE) for the static-linking
relinking obligation that applies to redistributed binaries.
