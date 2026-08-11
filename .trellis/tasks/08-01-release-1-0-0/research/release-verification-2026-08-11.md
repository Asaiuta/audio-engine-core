# Gate 9 Release Verification - 2026-08-11

## Outcome

The 1.0.0 release cut is prepared and locally verified end to end. `Cargo.toml`
is at 1.0.0, the CHANGELOG `[Unreleased]` content became the dated `[1.0.0]`
entry, README/CONTRIBUTING carry the stable-1.0 wording and the 1.x SemVer
policy plus a publish runbook, and every Release Documentation Checklist
command passed on this machine. The actual crates.io publish is handed off to
the owner per the decided prepare-and-runbook path (no token on this machine);
`cargo package` dry-run proved the package builds from the packed tarball.

## Decisions (2026-08-11, user)

- **Soak period**: immediate release. The gate sequence (Aug 1 → Aug 11) plus
  Lyne production usage is the soak; gates 1-8 landed with full validation.
- **Publish path**: prepare + runbook handoff. CONTRIBUTING.md gains a
  "Publishing a release" section; owner runs `cargo login` + `cargo publish`
  from the tagged commit.

## Facts Discovered

- `audio-engine-core` **0.1.0 was already published** on crates.io
  (2026-06-12, owner Asaiuta, 18 downloads; default features were
  `[http, loudness-db]`, no `soxr`). The Aug 1 gate plan's "never published"
  assumption was outdated. No git tag existed for it; `bf9addb` is the publish
  commit.
- Current default features are `[http, loudness-db, soxr]` — 1.0.0 default
  builds gain the LGPL-2.1 libsoxr build requirement. The Unreleased entry
  already documents the `soxr`-default feature addition; the CONTRIBUTING
  1.x policy now states that turning an optional dependency on by default is
  reviewed as if breaking.
- Pinned nightly-2026-07-09 and cargo-semver-checks 0.50.0 were already
  installed locally, so the full semver/public-api verification ran on this
  machine.

## Changes Made

- `Cargo.toml`: `version = "1.0.0"` (rust-version 1.87, edition 2021
  unchanged); `Cargo.lock` root package at 1.0.0.
- `README.md`: status banner → stable 1.0.0 statement; Quick Start and rubato
  snippets → `"1"`; Project Status rewritten (1.0 stability, MSRV 1.87,
  SemVer policy pointer).
- `CHANGELOG.md`: header policy paragraph → 1.x stability statement; empty
  `[Unreleased]` placeholder kept; `[1.0.0] - 2026-08-11` entry cut from the
  484-line Unreleased content; link refs updated
  (`[Unreleased]` → compare/v1.0.0...HEAD, `[1.0.0]` → compare/v0.1.0...v1.0.0).
- `CONTRIBUTING.md`: Semantic versioning policy rewritten to the 1.x contract
  (major = breaking, minor = feature, patch = fix; MSRV raising and
  default-on dependency changes are deliberate, changelog-recorded decisions);
  "Publishing a release" runbook added (verify local checklist → tag →
  push tags → `cargo login`/`cargo publish --dry-run`/`cargo publish` →
  docs.rs check; registry-baseline switch for semver-checks noted as a
  documented future CI change).

## Verification (2026-08-11, all exited zero)

- `cargo check --all-features` / `--no-default-features --features rubato`
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings` (and rubato
  matrix)
- `RUSTDOCFLAGS="-D warnings" DOCS_RS=1 cargo doc --no-deps --all-features`
- `cargo test --all-features`: 480 + 20 + 2 + 25 + 3 + 7 passed, 1 ignored
  (by-design native-shim evidence test)
- `cargo test --no-default-features --features rubato`: 500 + 20 + 2 + 25 +
  3 + 7 passed, 1 ignored (same)
- `cargo test --test public_api`: 2/2 — both feature-matrix snapshots match
- `cargo semver-checks --baseline-rustdoc ... --release-type patch` both
  matrices: 223 checks pass, 31 skip each ("no semver update required")
- `cargo package --allow-dirty`: 776 files packed, 39.0 MiB uncompressed /
  4.4 MiB compressed; in-package verification compiled successfully
- `grep` across README/CHANGELOG/CONTRIBUTING/NOTICE/docs: zero residual
  0.1.x / experimental / "may change before 1.0" wording (only historically
  accurate "pre-1.0" phrases inside the release entry describing the
  0.1 → 1.0 transition)
- `git diff --check` clean

## Tags

- `v0.1.0` backfilled at `bf9addb` ("Initial public release of
  audio-engine-core v0.1.0", 2026-06-12) so the CHANGELOG compare link
  `v0.1.0...v1.0.0` resolves.
- `v1.0.0` (annotated) at the release commit.
- Push of branch + tags requires explicit user confirmation (PRD DoD).

## Publish Handoff (owner)

```bash
git push origin chore/gate2-legacy-public-surface --tags   # branch first, then tags
cargo login                                                # crates.io token (once)
cargo publish --dry-run                                    # from the tagged commit
cargo publish
# verify https://docs.rs/audio-engine-core/1.0.0 and open a GitHub release
# linking the CHANGELOG entry
```