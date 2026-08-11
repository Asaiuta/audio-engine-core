# Release 1.0.0

1.0 release gate 9 of 9.

## Goal

Ship the frozen, documented, and semver-guarded public surface as the first
stable release: version bump to 1.0.0, CHANGELOG release entry, README/status
text replacing the experimental 0.1.x wording, a 1.x SemVer + MSRV policy
statement, git tags, and a working publish path. Since gates 1-8 are landed and
the crate is already published at 0.1.0 by the same owner, this gate converts
an existing experimental crate into a 1.0-compatible one rather than a
first-ever publish.

## What I Already Know

- Cargo.toml: `version = "0.1.0"`, `rust-version = "1.87"`, edition 2021.
  Default features are `["http", "loudness-db", "soxr"]`.
- **0.1.0 was already published** on crates.io (2026-06-12, owner Asaiuta,
  18 downloads, `max_version = "0.1.0"`). The published 0.1.0's default
  features were `["http", "loudness-db"]` — no `soxr`. **No git tag exists**
  for it; `bf9addb` ("Initial public release of audio-engine-core v0.1.0",
  2026-06-12) is the publish commit, and `CHANGELOG.md` compare links already
  reference `v0.1.0` + `v1.0.0`-style tags.
- So 1.0.0 silently changes default builds of existing users: default gains
  the native `soxr` backend dependency. Changelog must call this out.
- README has three pre-1.0 wordings to replace: the leading status banner
  (line ~13 "0.1.x — actively evolving; the API may change before 1.0"),
  the Quick Start dep line (`audio-engine-core = "0.1"`), and the Project
  Status section ("Experimental 0.1.x ... not yet for 1.0-level API
  compatibility guarantees").
- CHANGELOG: header paragraph says "While the crate is in the 0.x series the
  public API is considered experimental"; `## [Unreleased]` is a 484-line,
  fully structured (Added/Removed/Changed/Fixed) record of gates 3-8 that
  becomes the `[1.0.0] - 2026-08-11` entry; link refs at the bottom need
  `v1.0.0` compare entries (`...compare/v0.1.0...v1.0.0`).
- CONTRIBUTING.md Semantic versioning policy section is pre-1.0 wording
  ("Breaking changes bump the minor version `0.1` → `0.2`") and promises that
  "Once the API stabilizes the crate will move to 1.0.0" — rewrite to the 1.x
  contract. MSRV section already states the MSRV promise correctly.
- CI (`.github/workflows/ci.yml`) has 7 jobs: lint (fmt+clippy+doc both
  matrices), msrv (1.87 both matrices), public-api (surface + semver-checks
  against committed baselines), test 3-OS, pure-rust, quality-performance
  gates, and package (publish dry-run). **No release/publish workflow.**
- `docs/*.md` and `src/lib.rs` have no 0.1/experimental wording issues
  (checked: only unrelated numeric matches).
- No `~/.cargo/credentials` on this machine → real `cargo publish` needs a
  token via `cargo login` or a CI secret.
- Gates 3-6 (`replace-string-errors-with-typed-errors`,
  `introduce-typed-media-location`, `narrow-processor-and-chain-capability-model`,
  `unify-parameter-validation-policy`) have landed: code committed, PRDs +
  research recorded; their task.json status is "in_progress" only because
  finish-work was never run. Gates 7-8 are completed and archived.
- `quality-guidelines.md` Release Documentation Checklist (`.trellis/spec/backend/`)
  already records the executable contract including docs.rs parity,
  public-api/semver checks and `cargo package --allow-dirty`.

## Assumptions (temporary)

- 1.0.0 needs no semver-compat check against 0.1.0: the 0.x → 1.0 transition
  is allowed to break, and all breaking changes are already listed in the
  CHANGELOG Unreleased section.
- Backfill a `v0.1.0` tag at `bf9addb` so existing CHANGELOG compare links
  become real; tag `v1.0.0` at the release commit. Tags are created locally
  and pushed with the release.
- The API "soak period" requirement from the task description is the
  gate-sequence itself plus Lyne production usage — to be confirmed with the
  user (soak decision).
- Actual crates.io publish happens with the owner's token; this gate must
  leave `cargo publish --dry-run` green and a documented publish runbook.

## Open Questions

- ~~Soak period~~ **Decided (2026-08-11)**: publish immediately. The gate
  sequence itself (Aug 1 → Aug 11, surface frozen step by step) plus Lyne
  production usage is the soak; gates 1-8 landed with full validation.
- ~~Publish path~~ **Decided (2026-08-11)**: prepare-and-handoff. This gate
  delivers version bump, docs, tags, full validation, and a CONTRIBUTING
  publish runbook; the owner runs `cargo login` + `cargo publish` themselves
  (no token in this machine's environment, none exposed in chat).

## Requirements (evolving)

- Version `0.1.0` → `1.0.0` in Cargo.toml; `Cargo.lock` updated.
- CHANGELOG: `[Unreleased]` → `[1.0.0] - 2026-08-11`; pre-1.0 experimental
  paragraph replaced with the 1.x stability statement; entries that predate
  the released 0.1.0 (e.g. default-gains-soxr) positioned correctly (check
  whether the Unreleased heading postdates 0.1.0 — 0.1.0 was cut 2026-06-11
  and Unreleased covers everything after; soxr-default must be called out);
  link refs + `v1.0.0` compare block added.
- README: status banner → 1.0 stable wording; Quick Start dep `"0.1"` →
  `"1"`; Project Status section rewritten to the 1.0 statement with MSRV +
  SemVer policy references.
- CONTRIBUTING: Semantic versioning policy rewritten for 1.x (major =
  breaking, minor = feature, patch = fix); pre-1.0 "move to 1.0.0" promise
  removed; publish runbook section added (verify → dry-run → tag → publish →
  docs.rs check) mirroring the quality-guidelines checklist.
- Git tags: `v0.1.0` backfill at `bf9addb`, `v1.0.0` at the release commit
  (on the release branch, pushed to origin).
- Release verification evidence: local matrix green (per Release
  Documentation Checklist), `cargo semver-checks` against committed baselines,
  `cargo package --allow-dirty` verified, publish dry-run.
- Release prerequisites: gates 3-6 marked completed and archived (already
  landed code-wise); v0.1.0 tag decision recorded.
- Publish runbook recorded in CONTRIBUTING.md (+ task research file) so the
  actual publish is a documented, repeatable path.
- ~~Publish itself~~ **handed off**: actual `cargo publish` is executed by the
  owner after this gate (per the runbook); out of scope for the session.

## Acceptance Criteria (evolving)

- [x] Soak decision recorded: immediate release; publish path decision:
      prepare + runbook handoff (user answers 2026-08-11).

- [x] `Cargo.toml` version 1.0.0; build/test matrices green (all-features 480
      lib + 20/25/3/7 suites; rubato 500 + same; 1 ignored is the
      by-design native-shim evidence test); `rust-version = "1.87"` unchanged;
      `Cargo.lock` root at 1.0.0; clippy/fmt/doc `-D warnings` green both
      matrices (2026-08-11 local run).
- [x] CHANGELOG 1.0.0 entry dated 2026-08-11 with structured Added/Removed/
      Changed/Fixed content; header policy now the 1.x stability statement;
      grep shows zero residual 0.1.x/experimental wording in
      README/CHANGELOG/CONTRIBUTING/NOTICE/docs (only historically accurate
      "pre-1.0" phrases inside the entry describing the 0.1 → 1.0 transition).
- [x] README Quick Start shows `audio-engine-core = "1"` (also the rubato
      snippet); banner is the stable 1.0.0 statement; Project Status states
      1.0 stability + MSRV 1.87 + SemVer policy pointer.
- [x] CONTRIBUTING SemVer section rewritten to the 1.x contract (major =
      breaking, minor = feature, patch = fix; default-on dependency changes
      reviewed as breaking); publish runbook present and matching the
      quality-guidelines checklist commands.
- [x] `v0.1.0` backfilled at `bf9addb` and `v1.0.0` tagged at the release
      commit (created locally; push per user confirmation 2026-08-11);
      CHANGELOG compare links updated.
- [x] Release Documentation Checklist verified locally 2026-08-11: both
      matrices check/clippy/doc/tests, public_api 2/2, semver-checks
      223/223 both matrices, `cargo package --allow-dirty` (776 files) with
      in-package verification compiled successfully.
- [x] Publish evidence: dry-run via `cargo package` verification; actual
      `cargo publish` handed off to the owner per the CONTRIBUTING runbook
      (no token on this machine).
- [x] Gates 3-6 archived as completed (finish-work closeout, 2026-08-11).

## Definition of Done

- Release branch carries the version/doc/tag commit; CI-green state claimed
  only for what was locally verified; publish executed or runbook handed off
  per the user decision.
- Changes committed coherently; journal recorded; nothing pushed without
  explicit user confirmation (push/tag/publish are irreversible-ish actions).

## Technical Notes

- Files to touch: `Cargo.toml`, `Cargo.lock`, `README.md`, `CHANGELOG.md`,
  `CONTRIBUTING.md`; tag ops via git; publish per decision.
- Published 0.1.0 facts from crates.io API (2026-08-11): num 0.1.0,
  features default = [http, loudness-db], max_stable 0.1.0, yanked=false.
- Current default features `[http, loudness-db, soxr]` — the soxr in-default
  change happened after 0.1.0, so 1.0.0 default-build users gain the LGPL-2.1
  native libsoxr dependency; NOTICE already covers licensing.