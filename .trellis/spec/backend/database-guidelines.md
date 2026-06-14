# Database Guidelines (`loudness-db` feature)

> This crate has **no general-purpose or business database.** The only
> persistence is an optional loudness-metadata cache behind the `loudness-db`
> feature. Source of truth: `src/processor/loudness_db.rs`.

---

## Scope

The `loudness-db` feature adds a small SQLite-backed cache for EBU R128 loudness
measurements so a track does not have to be re-analyzed on every run. It stores
loudness metadata only — there is no user data, no business entities, no ORM.

- Backend: `rusqlite` with the `bundled` SQLite, pulled in **only** when the
  feature is on (`rusqlite = { ..., optional = true }`,
  `loudness-db = ["dep:rusqlite"]`).
- Feature status: `loudness-db` is **default-on but optional**. With it off,
  the EBU R128 measurement helpers (`LoudnessMeter`, `LoudnessNormalizer`,
  `TruePeakDetector`) still work fully; only the on-disk cache disappears.
- Public surface (feature-gated): `LoudnessDatabase`, `TrackLoudness`,
  `DatabaseStats`, `CURRENT_SCAN_VERSION`, and the default target constants.

## Schema & Migration

- Single table, created idempotently: `CREATE TABLE IF NOT EXISTS
  track_loudness (...)`.
- Schema evolution is version-gated by `CURRENT_SCAN_VERSION` (currently `1`).
  Rows carry the `scan_version` they were written with; a row whose
  `version < CURRENT_SCAN_VERSION` is treated as stale and re-scanned rather
  than trusted.
- Additive column changes are reconciled by inspecting
  `PRAGMA table_info(track_loudness)` rather than assuming the column set. Bump
  `CURRENT_SCAN_VERSION` when the measurement meaning changes so stale rows are
  invalidated.

## Hard Rule: Never On The Realtime Path

Database access (open, query, insert, migrate) is a setup/control-thread
operation. **It must never happen inside an audio callback or the DSP chain** —
SQLite does file I/O and can lock, both forbidden on the hot path
(`realtime-safety.md`). Measure loudness on the analysis path, persist the
result off the callback, and read cached values before entering realtime.

## Conventions

- All `loudness-db` code stays behind `#[cfg(feature = "loudness-db")]`; the
  crate must build and test with `--no-default-features`.
- Keep DB errors typed and returned to the caller; do not panic on a missing or
  corrupt cache — treat it as "no cached value, re-measure".
