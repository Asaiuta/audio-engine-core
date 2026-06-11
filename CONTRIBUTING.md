# Contributing to audio-engine-core

Thanks for your interest in improving `audio-engine-core`. This crate provides
app-agnostic audio decoding, DSP, loudness, resampling, and streaming-pipeline
primitives. It is consumed by the Lyne audio player but is designed to stand on
its own.

## Development setup

The crate links the native [libsoxr](https://sourceforge.net/projects/soxr/)
resampling library. Install it before building:

- **Debian/Ubuntu:** `sudo apt-get install libsoxr-dev`
- **macOS (Homebrew):** `brew install libsoxr`
- **Windows (vcpkg):** `vcpkg install soxr:x64-windows` (the crate declares the
  vcpkg dependency in `Cargo.toml`; set `VCPKG_ROOT` so the build script can find it)

Then the usual workflow:

```bash
cargo build
cargo test
```

## Cargo features

- `http` (default) — HTTP/HTTPS streaming decode via `reqwest`.
- `loudness-db` (default) — SQLite-backed loudness metadata persistence.

Both are enabled by default for backward compatibility. When making changes,
verify all three configurations still build and lint cleanly:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test --all-features
cargo test --no-default-features
```

## Before submitting a change

1. `cargo fmt --all` — formatting must match `rustfmt`.
2. `cargo clippy --all-targets --all-features -- -D warnings` — zero warnings.
3. `cargo test --all-features` and `cargo test --no-default-features` — all green.
4. `cargo doc --no-deps --all-features` — docs build without warnings.
5. Add or update tests for any behavior change. DSP changes that touch the
   realtime path should preserve zero-allocation guarantees (see the
   `assert_no_alloc`-based tests).

CI runs all of the above on Linux, macOS, and Windows, plus a `cargo package`
publish dry-run and a docs.rs-parity doc build. See
`.github/workflows/audio-engine-core.yml`.

## Realtime-safety conventions

The processing path is designed to run on the audio callback thread:

- No heap allocation in steady-state `process` calls — pre-allocate buffers up
  front and reuse them.
- No locks that can block the audio thread; prefer lock-free parameter handoff
  (see `lockfree_params`).
- Tests that assert allocation behavior use the `assert_no_alloc` crate; keep
  them passing.

## Semantic versioning policy

This crate follows [Semantic Versioning](https://semver.org/). While the crate
is pre-1.0 (`0.y.z`):

- **Breaking changes** (removing or changing public items, altering documented
  behavior) bump the **minor** version (`0.1` → `0.2`).
- **New features and non-breaking additions** bump the **patch** version
  (`0.1.0` → `0.1.1`).

The public API surface is everything re-exported from `lib.rs` and the public
items of the `config`, `decoder`, `diagnostics`, `pipeline`, `processor`, and
`runtime` modules. Adding a default-on Cargo feature, or making a previously
required dependency optional behind a default-on feature, is **not** considered
a breaking change because existing default builds are unaffected.

Once the API stabilizes the crate will move to `1.0.0` and adopt the standard
1.x compatibility guarantees. Notable changes are recorded in
[`CHANGELOG.md`](CHANGELOG.md).

## Licensing

Contributions are dual-licensed under [MIT](LICENSE-MIT) OR
[Apache-2.0](LICENSE-APACHE), at the user's option. By submitting a change you
agree to license your contribution under these terms. Note that the native
libsoxr dependency is LGPL-2.1 — see [`NOTICE`](NOTICE) for the static-linking
relinking obligation that applies to redistributed binaries.
