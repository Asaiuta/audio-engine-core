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
  `version != CURRENT_SCAN_VERSION` is treated as stale and re-scanned rather
  than trusted. The comparison is exact, not `<`: a row written by a *newer*
  scanner is no more verifiable by this build than an older one.
- Additive column changes are reconciled by inspecting
  `PRAGMA table_info(track_loudness)` rather than assuming the column set. Bump
  `CURRENT_SCAN_VERSION` when the measurement meaning changes so stale rows are
  invalidated.

## Cache Freshness Contract

`LoudnessDatabase::needs_scan` is the single freshness gate; `get_fresh` is
`needs_scan` plus `get`. Its evidence differs by identity, and the difference is
part of the contract rather than an implementation detail:

- **Local identity** — the scanner version, whole-second mtime, and size must
  all match. A file that cannot be stat-ed (deleted, renamed, unmounted volume,
  permission denied) reports "needs scan": there is no evidence the stored
  measurement still describes that path, and reporting it fresh served a stale
  gain for content nobody could confirm.
- **Remote identity** — `http://` / `https://`, matched case-insensitively so it
  agrees with the decoder's `MediaLocation` router. Only the scanner version is
  checked, because no mtime or size is stored. A replaced remote body is
  therefore **not** detected; a caller that must notice replacement has to
  invalidate the entry itself.

A query that returns rows must not silently drop undecodable ones.
`get_outdated_tracks` propagates a row-decoding failure, because a silently
short list reads as "nothing left to rescan" — the opposite of what the failure
means.

Known limitations, recorded rather than silently accepted: whole-second mtime
plus size misses a same-size replacement inside one second; `compute_track_id`
normalizes separators and folds case on Windows only, but does not canonicalize
local paths, so `./a.flac` and `/music/a.flac` are two rows for one file.
Closing these needs distinct typed local/remote identities.

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
