# Trellis Backend Spec Bootstrap for Audio Core

## Goal

Replace the bootstrap-level placeholder text in the Trellis backend spec with the actual conventions of this Rust audio-core crate, so later implementation tasks get accurate, source-backed context instead of generic guidance.

## Requirements

- Audit the current `.trellis/spec/backend/*` files and identify every placeholder or generic statement that does not match this crate.
- Capture the real directory layout (`src/decoder`, `src/processor`, `src/processor/loudness`, benches, examples) in `directory-structure.md`.
- Document the actual error model used by the crate (decoder error enum, `Result` conventions, no panics in hot paths) in `error-handling.md`.
- Add a dedicated `realtime-safety.md` spec file capturing the core RT invariant: no heap allocation, locks, logging, file I/O, network I/O, or unbounded work inside the audio callback / DSP hot path. Link it from `index.md`.
- Keep `logging-guidelines.md` focused on non-RT logging conventions (diagnostics, decode/setup paths) and cross-reference `realtime-safety.md` for the hot-path prohibition.
- Document the quality/evidence policy in `quality-guidelines.md`: claims must be backed by a test, current benchmark output, or an explicit limitation note.
- Repurpose `database-guidelines.md` as the `loudness-db` feature conventions file: it documents the optional `loudness-db` (rusqlite/SQLite) persistence layer for EBU R128 loudness metadata — schema/migration approach, feature-flag gating (`loudness-db` is default-on but optional), and the rule that DB access never happens on the realtime path. State explicitly that the crate has no general-purpose/business database.
- Keep `index.md` as an accurate entry point that links the other spec files.
- Cross-check spec statements against `CONTRIBUTING.md` and `README.md` and resolve contradictions instead of duplicating stale text.

## Acceptance Criteria

- [ ] No backend spec file contains template/placeholder language that contradicts the real crate.
- [ ] `directory-structure.md` reflects the live `src/` tree, including the `processor/loudness` submodule split.
- [ ] `error-handling.md` describes the decoder error type and the no-panic-in-callback rule.
- [ ] `realtime-safety.md` exists and states the hot-path prohibitions (no alloc/lock/log/IO/unbounded work) explicitly; `index.md` links it.
- [ ] `logging-guidelines.md` covers non-RT logging conventions and cross-references `realtime-safety.md` for the hot-path rule.
- [ ] `database-guidelines.md` documents the optional `loudness-db` persistence layer (not a business DB) and its feature-flag gating.
- [ ] `quality-guidelines.md` encodes the evidence policy from the algorithm audit.
- [ ] `index.md` links every backend spec file and matches their actual content.
- [ ] `task.py validate` passes for this task.

## Validation Commands

- `python .trellis/scripts/task.py validate --task 06-12-audio-engine-trellis-spec-bootstrap`
- `cargo build`
- `cargo test --lib`

## Out of Scope

- Writing or changing any DSP/decoder implementation code.
- Adding new spec domains beyond the backend layer (the only new file is `realtime-safety.md`; no new package/layer trees).
- Frontend/UI or application-integration conventions outside this crate.
- Restating CLAUDE.md/AGENTS.md content that is already authoritative elsewhere.

## Technical Notes

- The current backend spec is mostly bootstrap text; implementation tasks should inspect live Rust source and not rely on placeholders.
- This task is the documentation foundation for the rest of the feature-upgrade roadmap and is marked P1 because it precedes complex implementation work.
- Source anchors: `.trellis/spec/backend/*.md`, `src/decoder/error.rs`, `src/processor/mod.rs`, `CONTRIBUTING.md`, `README.md`.
- Shared audit: `../06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
