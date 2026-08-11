# SemVer Baseline Runbook

## Decision

Gate 8 commits rustdoc JSON for both supported feature matrices and compares
new JSON with `cargo-semver-checks 0.50.0 --baseline-rustdoc`.
`--baseline-root` is deliberately not used: in cargo-semver-checks 0.50.0 it
means a directory containing an old crate source tree, not a JSON baseline.

The pinned rustdoc producer remains `nightly-2026-07-09`, shared with
`tests/public_api.rs` and `.github/workflows/ci.yml`. A floating nightly or
floating cargo-semver-checks version would make tool/schema drift look like an
API change.

## Baseline Paths

- `tests/semver-baseline/all-features/audio_engine_core.json`
- `tests/semver-baseline/rubato/audio_engine_core.json`

The corresponding current JSON paths produced by `cargo test --test
public_api` are:

- `target/public-api/all-features/doc/audio_engine_core.json`
- `target/public-api/rubato/doc/audio_engine_core.json`

## Refresh

Install the exact tools once:

```powershell
rustup toolchain install nightly-2026-07-09 --profile minimal
cargo install cargo-semver-checks --locked --version 0.50.0
```

Regenerate the public surface and both rustdoc JSON files, then copy them only
after the API change and its required version policy have been reviewed:

```powershell
$env:UPDATE_SNAPSHOTS = '1'
cargo test --test public_api
Copy-Item target/public-api/all-features/doc/audio_engine_core.json tests/semver-baseline/all-features/audio_engine_core.json
Copy-Item target/public-api/rubato/doc/audio_engine_core.json tests/semver-baseline/rubato/audio_engine_core.json
Remove-Item Env:UPDATE_SNAPSHOTS
```

Documentation-only changes should leave `tests/public-api-*.txt` unchanged,
but rustdoc JSON is still refreshed so the committed documentation payload is
current.

## Verify

```powershell
cargo semver-checks --baseline-rustdoc tests/semver-baseline/all-features/audio_engine_core.json --current-rustdoc target/public-api/all-features/doc/audio_engine_core.json --release-type patch
cargo semver-checks --baseline-rustdoc tests/semver-baseline/rubato/audio_engine_core.json --current-rustdoc target/public-api/rubato/doc/audio_engine_core.json --release-type patch
```

Do not pass Cargo feature flags with `--baseline-rustdoc` and
`--current-rustdoc`; the selected feature sets are already encoded in those
two generated files.

## Negative Control

Before accepting a new gate implementation, temporarily remove or privatize a
known public item, regenerate only the current JSON, and confirm the matching
command exits non-zero with the removed item named. Restore the source, rebuild
the current JSON, and require the same command to pass. Never refresh the
committed baseline while the negative control is active.

After 1.0 is published, switching to a crates.io version baseline is a separate
reviewed policy change. It must not happen implicitly during an ordinary
baseline refresh.

## Gate 8 Validation (2026-08-11)

- Positive control: both committed JSON baselines compared against their
  matching `target/public-api` current JSON with cargo-semver-checks 0.50.0.
  Each matrix ran 223 checks: 223 passed and 31 were inapplicable/skipped.
- Negative control: `audio_engine_core::diagnostics` was temporarily made
  private without changing the committed baseline. The all-features gate
  exited 1 and reported four major-violation lint classes:
  `function_missing`, `module_missing`, `pub_module_level_const_missing`, and
  `struct_missing`. Diagnostics named `decode_memory_budget`, the diagnostics
  module, all four budget constants, and `DecodeMemoryBudget`.
- Restoration control: the module was restored to public, `cargo test --test
  public_api` passed 2/2 and regenerated current JSON, then both SemVer checks
  returned to 223/223 passing.

The negative JSON lives only under ignored `target/semver-negative/`; it is not
a baseline and must not be committed.
