# Establish public API baseline and MSRV gate

## Goal

Give the 1.0 effort a measuring instrument before the public surface is
reshaped. Gates 2-7 each intentionally change the public API; today nothing in
the repository or CI makes that change visible, reviewable, or bounded. This
task commits a public-surface baseline and adds an MSRV job, so every later
gate produces an explicit surface delta in its own diff and cannot silently
raise the minimum supported Rust version.

This is gate 1 of 9 on the road to 1.0. It deliberately does **not** enforce
semver compatibility or documentation completeness — those are gate 8, after
the surface stops moving.

## What I already know (measured 2026-08-01)

- The crate was never published: no git tag exists, and CI has a `package`
  dry-run job but no `publish` job. There is therefore no external
  compatibility obligation and no published baseline to diff against.
- `Cargo.toml` declares `rust-version = "1.87"`. **No CI job verifies it.**
  The local toolchain is 1.93.1, so the declared MSRV is an unverified claim.
- The dependency tree does not contradict 1.87. Of 164 dependencies declaring
  a `rust-version`, the highest native-relevant floor is 1.86 (`icu_*` 2.2.0,
  reached through `reqwest` → `idna`). The only 1.87 entry is `wasip2`, a
  wasm-target dependency irrelevant to the supported matrices. The remaining
  risk is therefore this crate's own code, which only a real 1.87 build
  settles.
- There is no `rust-toolchain.toml`. Every CI job uses
  `dtolnay/rust-toolchain@stable`.
- Existing CI jobs: `lint` (fmt + clippy on both feature matrices + docs.rs
  parity doc build), `test` (ubuntu/macos/windows), `pure-rust`,
  `quality-performance-gates`, `package`.
- Neither `cargo-public-api` nor `cargo-semver-checks` is installed.
- **Rustdoc JSON generation works on this crate.**
  `cargo +nightly rustdoc --lib --all-features -- -Z unstable-options
  --output-format json` completes in ~32s. This is the mechanism every
  surface-diffing tool needs, and it is nightly-only: `-Z unstable-options`
  has no stable equivalent, so any real solution requires a nightly toolchain.
- The public surface is feature-dependent and must be baselined per matrix:
  `loudness-db` gates `LoudnessDatabase`/`TrackLoudness`/`DatabaseStats`
  (`src/lib.rs:146`), `http` gates `HttpCredentials`, and the resampler
  backends gate further items. The repository's established convention is the
  dual matrix `--all-features` and `--no-default-features --features rubato`.
- Surface size: ~196 top-level public items plus ~471 public methods across
  `pub mod`s `channel_layout`, `config`, `decoder`, `diagnostics`, `pipeline`,
  `processor`, `runtime` (`src/lib.rs:108-113`).
- The repository already has a precedent for a test that skips when its
  prerequisite is absent: the resampler-comparison native-shim test is
  explicitly ignored when the separately built shim is missing.
- Evidence that a mechanical gate has value: while gathering this context, the
  freshly committed P1 remediation was found to fail the existing docs.rs
  parity job with nine broken intra-doc-link errors. fmt, clippy, and the full
  test suite were all green, so no existing gate caught it. Fixed in `38b37a6`.

## Requirements

- A committed public-surface baseline exists for both supported feature
  matrices: `--all-features` and `--no-default-features --features rubato`.
- An integration test renders the current public surface and fails when it
  differs from the committed baseline, printing the diff.
- The test skips with an explanatory message when no nightly toolchain is
  available, so contributors on stable are not blocked. CI runs it for real on
  a **pinned** nightly date, so a rustdoc JSON format change cannot break CI
  on an unrelated day.
- Refreshing the baseline is a single documented command.
- A CI job builds the crate with the exact toolchain named by `rust-version`.
- `rust-version` states a value that is actually verified. If 1.87 does not
  build, this task states the true floor rather than keeping a false claim.
- `CONTRIBUTING.md` documents the refresh command and the MSRV policy, and the
  drift the audit recorded in that file is corrected while it is open: it
  currently announces five quick commands while listing four, and claims all
  checks run on three operating systems when only the `test` job does.

## Acceptance Criteria

- [x] `tests/public-api-all-features.txt` and `tests/public-api-rubato.txt`
      are committed and correspond to the current surface.
- [x] Deliberately adding a public item without refreshing the baseline fails
      the test; refreshing makes it pass and produces a readable text diff.
- [x] The test skips, not fails, on a stable-only toolchain, and says why.
- [x] A CI job builds both feature matrices on the declared MSRV toolchain and
      fails if that toolchain cannot build them.
- [x] The MSRV named in `Cargo.toml` is the one CI proves.
- [x] `CONTRIBUTING.md` lists the refresh command among its quick commands,
      states the MSRV policy, and no longer miscounts its own quick commands or
      overstates which checks run on three operating systems.

## Definition of Done

- `cargo fmt --all -- --check` passes.
- `cargo clippy --all-targets -- -D warnings` passes on both matrices.
- `cargo test` passes on both matrices.
- `cargo doc --no-deps --all-features` passes under `RUSTDOCFLAGS=-D warnings`.
- New CI jobs are green on a real run, not only reasoned about.

## Technical Approach

Use the `public-api` crate as a dev-dependency with `rustdoc-json` to build the
JSON, rendered to a stable sorted text form and diffed against the committed
baselines. This is the approach whose delta lands in the pull-request diff,
which is the whole point of gate 1: when gate 3 replaces
`Result<_, String>` on `LoudnessDatabase`, its own commit shows the exact
signature change as reviewable text, and gate 8 has an in-repo reference for
what the surface looked like when it froze.

## Decision (ADR-lite)

**Context**: Gates 2-7 deliberately break the public API of a crate with ~670
public items and no published baseline. Without an in-repo surface record,
those changes are invisible to review and gate 8 has nothing to freeze against.

**Decision**: Commit per-matrix public-surface snapshots and enforce them with
an integration test, rather than a CI-only diff against the merge base or
deferring measurement to gate 8.

**Consequences**: Two text files churn on every intentional API change — that
churn *is* the deliverable, not a cost. A nightly toolchain becomes a CI
dependency, mitigated by pinning the date. Contributors on stable see a
skipped test instead of a failure. Semver *semantics* remain unchecked until
gate 8; this task only records the surface.

## Out of Scope

- `#![deny(missing_docs)]` and `cargo-semver-checks` enforcement — gate 8, once
  gates 2-7 have stopped moving the surface. Turning them on now would spend
  documentation effort on items gate 2 is going to delete.
- Any change to the public API itself. This task only measures.
- Raising the MSRV as a feature decision. If 1.87 turns out to be false, the
  correction is to state the true floor, not to modernize the codebase.
- A `rust-toolchain.toml` pinning the whole project. The pinned nightly is
  scoped to the baseline job only.

## Technical Notes

- Nightly rustdoc JSON is the only viable mechanism; verified working on this
  crate at ~32s for `--all-features`.
- The native `libsoxr` dependency means an `--all-features` baseline job needs
  the same platform setup as the existing `lint` job. The
  `--no-default-features --features rubato` matrix needs no native library and
  is the cheaper of the two.
- The MSRV job should cover both matrices: the default feature set ships
  `http`, `loudness-db`, and `soxr`, so a rubato-only MSRV proof would not
  cover what users actually get by default.
