# Enforce a documented and semver-checked public API

1.0 release gate 8 of 9.

## Goal

Freeze the post-gate-7 public surface as a *documented* and *semver-guarded*
contract. Turn on `deny(missing_docs)` for the library, wire
`cargo-semver-checks` into CI against a committed baseline, refresh the
`public-api` snapshots, and close the gap between the existing git-level
surface baseline and a real semantic-versioning gate.

## What I Already Know

- Gate 1 committed `tests/public_api.rs` + `tests/public-api-{all-features,rubato}.txt`
  using `public-api` 0.52.1 / `rustdoc-json` 0.9.10 and a pinned
  `nightly-2026-07-09` (rustdoc JSON is nightly-only). CI job `public-api`
  installs that nightly, verifies it, and runs `cargo test --test public_api`.
  The test skips when the pinned nightly is absent (local dev), so it cannot
  silently fail a machine without it.
- `src/lib.rs` currently has **no** crate-level lint attributes
  (`grep '^#!'` is empty); `missing_docs` is not enabled anywhere.
- Measuring with `RUSTDOCFLAGS="-D missing-docs" cargo doc --no-deps
  --all-features` (DOCS_RS=1): **428 errors**; rubato-only matrix **424** —
  nearly symmetric. Breakdown (all-features): 234 struct fields, 101 methods,
  48 enum variants, 32 associated functions, 4 free functions, 3 constants,
  2 structs, 2 modules, 2 enums.
- `cargo-semver-checks` is **not installed** locally and there is no semver
  CI job. Cargo.toml has no dev-dependency or script for it.
- Cargo.toml: version 0.1.0, `rust-version = "1.87"`, edition 2021; CI has
  fmt+clippy(-D warnings, both matrices), docs.rs parity (`DOCS_RS=1`
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`), MSRV
  checks, public-api, 3-OS test matrix, and a pure-Rust job.
- Gates 2-7 reshaped the surface deliberately; each reshaping commit already
  updated the `.txt` snapshots, so the surface is now stable by design.

## Assumptions (temporary)

- Every remaining public item is intentional; `#[doc(hidden)]` is not an
  acceptable substitute for documentation because the 1.0 promise is a
  documented surface, not a filtered one.
- crate-level `#![deny(missing_docs)]` in `src/lib.rs` is the enforcement
  point (runs under plain `cargo doc`, the docs.rs parity job, and rustdoc
  JSON emission for public-api/semver baselines), possibly with a small
  `#![allow(...)]` set only where the lint is structurally wrong.
- `cargo-semver-checks` can be installed in CI via a prebuilt action
  (`taiki-e/install-action`), not a multi-minute source build.

## Open Questions

- ~~Semver baseline strategy~~ **Decided (2026-08-10, corrected 2026-08-11)**:
  commit rustdoc JSON baselines for both feature matrices under
  `tests/semver-baseline/`; CI runs `cargo-semver-checks 0.50.0
  --baseline-rustdoc` against them and fails on breaking changes.
  `--baseline-root` accepts an old crate source tree, not a JSON directory, so
  it is not the selected contract. After 1.0 is published, the gate may switch
  to the registry baseline (documented switch, not automatic).
- ~~Missing-docs depth~~ **Decided**: meaningful one-line comments stating
  purpose/boundary for every missing item (struct fields, variants,
  constants), behavior/error notes for methods/functions; no full doctest
  tutorial-ization.

## Requirements (evolving)

- Enable `deny(missing_docs)` for the library crate; both feature matrices
  build docs with zero missing-doc errors.
- Wire a semver gate into CI that compares HEAD's rustdoc JSON against a
  committed baseline and fails on breaking changes; the gate must not depend
  on a floating nightly.
- Keep the `public-api` snapshot job and refresh both `.txt` snapshots where
  documentation-only changes alter rendering (they should not).
- No `#[doc(hidden)]` on public API items; private internals stay private.
- MSRV promise (1.87) unaffected; dev/CI tooling may use newer toolchains.
- Provide a local runbook for refreshing the semver baseline and the public-api
  snapshots (mirroring `UPDATE_SNAPSHOTS=1`).

## Acceptance Criteria (evolving)

- [x] `cargo doc --no-deps --all-features` and the rubato-only matrix pass
      with missing docs denied (both locally and in CI's docs.rs parity job).
- [x] Zero missing-doc errors under `RUSTDOCFLAGS="-D missing-docs"` for both
      feature matrices.
- [x] rustdoc JSON baselines for both feature matrices committed under
      `tests/semver-baseline/` and regenerable from a documented command
      (pinned nightly, `-Z unstable-options --output-format json`).
- [x] A CI job runs `cargo-semver-checks --baseline-rustdoc` against the
      committed baselines and fails on a breaking API change; a deliberate
      breaking fixture fails the gate locally before it can pass.
- [x] `tests/public-api-*.txt` refresh (if needed) and stay green.
- [x] Every public item documented with a non-trivial comment (no empty
      `///` placeholders, no `#[doc(hidden)]`).

## Definition of Done

- 428/424 missing-doc items resolved; deny enforced in-tree.
- Semver CI gate green on HEAD baseline; breaking-change fixture proven to fail
  (a deliberate revert must fail the gate before it can pass again).
- Runbook recorded in the task research and backend spec
  (`quality-guidelines.md` Release Documentation Checklist section).
- Changes committed coherently; nothing pushed or archived without explicit
  user direction.

## Out of Scope (explicit)

- Renaming, moving, or deleting public items to dodge documentation (gate 2 is
  the surface-shrinking gate; the surface is frozen here).
- Adding `#[doc(hidden)]` or `#[allow(missing_docs)]` as a bulk escape.
- Changing MSRV or edition.
- Publishing 1.0.0 (gate 9).
- Polishing the wording of already-documented items beyond what the lint needs.

## Technical Notes

- Enforcement measurement commands (recorded 2026-08-10):
  `DOCS_RS=1 RUSTDOCFLAGS="-D missing-docs" cargo doc --no-deps --all-features`
  → 428 errors; `--no-default-features --features rubato` → 424.
- `tests/public_api.rs` PINNED_NIGHTLY = `nightly-2026-07-09`; CI toolchain
  entries to keep in sync: public-api and (new) semver jobs.
- The docs.rs parity CI job already denies warnings; adding the crate-level
  lint makes `cargo doc` itself enforce it locally.
- Semver baseline candidates to evaluate: committed rustdoc-JSON directory
  (`.semver/`), `--baseline-root` support in cargo-semver-checks, and the
  post-publish registry baseline (`--baseline <version>` / default crates.io
  current) that gate 9's release makes real.
