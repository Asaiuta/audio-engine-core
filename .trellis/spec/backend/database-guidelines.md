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
- Public surface (feature-gated): `LoudnessDatabase`,
  `LoudnessDatabaseError`, `LoudnessSourceIdentity`, `TrackLoudness`,
  `DatabaseStats`, `CURRENT_SCAN_VERSION`, and the default target constants.

## Typed Operation Boundary

### 1. Scope / Trigger

Apply this contract when changing database opening, schema migration, queries,
transactions, or the connection-lock boundary.

### 2. Signatures

```rust
#[non_exhaustive]
pub enum LoudnessDatabaseError {
    CreateDirectory(std::io::Error),
    Database(rusqlite::Error),
    LockPoisoned,
}

LoudnessDatabase::open(path) -> Result<LoudnessDatabase, LoudnessDatabaseError>
LoudnessDatabase::in_memory() -> Result<LoudnessDatabase, LoudnessDatabaseError>
// Every query and mutation returns Result<_, LoudnessDatabaseError>.
```

### 3. Contracts

- Directory creation retains the original `std::io::Error` as its source.
- SQLite open, migration, query, row-decode, and transaction failures retain
  the original `rusqlite::Error` as their source.
- A poisoned `Mutex<Connection>` maps explicitly to `LockPoisoned`; the
  guard-dependent `PoisonError` is not exposed or stringified.
- Callers decide whether a typed failure means rebuild, retry, or report. The
  library does not silently convert a corrupt/missing cache into an empty row.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Parent directory creation fails | `LoudnessDatabaseError::CreateDirectory` with I/O source |
| SQLite open/schema/query/row decode fails | `LoudnessDatabaseError::Database` with SQLite source |
| A prior panic poisoned the connection mutex | `LoudnessDatabaseError::LockPoisoned` |
| Query returns no matching row | `Ok(None)`, not an error |

### 5. Good / Base / Bad Cases

- Good: a caller matches `Database` to invalidate and rebuild a disposable
  cache while retaining the SQLite source for diagnostics.
- Base: a missing track returns `Ok(None)` and leaves the database usable.
- Bad: map every failure through `to_string()` or treat a row-decoding failure
  as an empty/outdated list.

### 6. Tests Required

- Match directory-I/O, SQLite, and poisoned-lock failures by variant without
  inspecting display text.
- Assert `std::error::Error::source` is present for I/O and SQLite variants.
- Keep the malformed-row regression for `get_outdated_tracks` so collection
  propagates row decoding failure.

### 7. Wrong vs Correct

#### Wrong

```rust
let conn = self.conn.lock().map_err(|error| error.to_string())?;
```

#### Correct

```rust
let conn = self
    .conn
    .lock()
    .map_err(|_| LoudnessDatabaseError::LockPoisoned)?;
```

## Scenario: Typed Source Identity, Schema, And Freshness

### 1. Scope / Trigger

- Trigger: changing `MediaLocation` consumption, cache-key derivation, persisted
  source columns, schema migration, freshness, deletion, album updates, or
  outdated-track enumeration.
- Loudness data is a disposable cache, but its identity is security- and
  correctness-sensitive: raw signed URLs must not be persisted, and local and
  remote sources must never share a namespace by textual coincidence.

### 2. Signatures

```rust
pub struct LoudnessSourceIdentity { /* private validated fields */ }

LoudnessSourceIdentity::from_location(&MediaLocation) -> LoudnessSourceIdentity
TrackLoudness::new(&MediaLocation, ...) -> TrackLoudness
LoudnessDatabase::get(&LoudnessSourceIdentity) -> Result<Option<TrackLoudness>, LoudnessDatabaseError>
LoudnessDatabase::get_fresh(&LoudnessSourceIdentity) -> Result<Option<TrackLoudness>, LoudnessDatabaseError>
LoudnessDatabase::needs_scan(&LoudnessSourceIdentity) -> Result<bool, LoudnessDatabaseError>
LoudnessDatabase::delete(&LoudnessSourceIdentity) -> Result<bool, LoudnessDatabaseError>
LoudnessDatabase::get_outdated_tracks() -> Result<Vec<LoudnessSourceIdentity>, LoudnessDatabaseError>
```

Schema version 2 stores:

```sql
track_id TEXT PRIMARY KEY,
source_kind TEXT NOT NULL CHECK(source_kind IN ('local', 'http')),
source_locator BLOB,
source_label TEXT NOT NULL,
-- loudness values, scanner version, timestamps, local mtime/size
CHECK (
  (source_kind = 'local' AND source_locator IS NOT NULL) OR
  (source_kind = 'http' AND source_locator IS NULL)
)
```

### 3. Contracts

- Derive identity only from `MediaLocation`; database APIs never guess source
  kind from `&str`.
- Cache IDs use SHA-256 with a crate/domain prefix and explicit source
  namespace. They render as `local:sha256:<digest>` or
  `http:sha256:<digest>`, so equal textual payloads cannot collide across
  namespaces.
- A local identity hashes and persists the platform-native path encoding in
  `source_locator`. It does not lowercase, canonicalize, or round-trip through
  UTF-8. Filesystem alias resolution remains the caller's responsibility.
- An HTTP identity hashes the full validated request URL, preserving
  case-sensitive path/query distinctions, but persists no plaintext locator.
  `source_label` is origin-only. Userinfo, path, query, and fragment never
  appear in library-controlled rows, `Debug`, or `Display`.
- Row decoding reconstructs the typed identity and verifies its namespace,
  locator policy, safe label, and cache ID. A hash hit that reconstructs to a
  different requested identity is an error, not a cache hit.
- `needs_scan` is the single freshness gate. A local row is fresh only when
  scanner version, whole-second mtime, and size all match and the native path
  is readable. An HTTP row is always stale because this API stores no ETag,
  Last-Modified value, digest, or caller revision. `get_fresh` therefore never
  returns an HTTP row.
- `CURRENT_SCAN_VERSION` versions measurement semantics; SQLite
  `PRAGMA user_version = 2` versions identity/schema semantics. They are
  independent gates.
- On open, inspect both `user_version` and the full required column set. Any
  pre-v2 or incompatible table is dropped and recreated transactionally, then
  `VACUUM` runs after commit to remove legacy signed-URL bytes. Reopening the
  current schema is idempotent and preserves rows.
- Queries returning multiple identities propagate any row-decoding error;
  they never silently shorten the result.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Same text used as a local path and HTTP URL | distinct local/http cache IDs |
| HTTP path/query differs only by case | distinct HTTP cache IDs |
| Signed HTTP URL | hash plus origin-only label; no plaintext locator or secret |
| Local native path cannot be stat-ed | `needs_scan == true`; row remains queryable with `get` |
| Local mtime/size or scanner version differs | stale |
| HTTP row at current scanner version | stale and included in outdated results |
| Persisted kind/locator/cache ID/safe label is inconsistent | typed database error; no row returned |
| Legacy string-identity table or wrong `user_version` | drop, recreate v2, `VACUUM`; no legacy rows retained |
| Current v2 database reopened | schema and rows preserved |

### 5. Good / Base / Bad Cases

- Good: construct one identity from the playback `MediaLocation`, use it for
  upsert/get/freshness/delete, and rescan every remote source until revision
  evidence is added explicitly.
- Base: a missing identity returns `Ok(None)` from `get` and `true` from
  `needs_scan`.
- Bad: persist a raw signed URL, infer remote identity from a case-insensitive
  prefix, treat scanner-version equality as remote freshness, or reinterpret a
  legacy string row under the v2 contract.

### 6. Tests Required

- Prove local and HTTP namespaces differ and HTTP path/query case remains
  significant.
- Persist a credentialed signed URL and search the record, formatted values,
  and SQLite columns for username, password, path token, query secret, and
  fragment secret; none may appear.
- Round-trip a non-UTF-8 local path on supported platforms and assert its cache
  ID and native path are unchanged.
- Cover present, replaced, unreadable, and newer-scanner local rows. Assert
  every validator-less HTTP row is stale, absent from `get_fresh`, and present
  in outdated results.
- Create a real legacy SQLite table containing a secret marker, open it twice,
  and assert v2 schema/version, zero legacy rows, secret removal from database
  bytes after `VACUUM`, and idempotent second open.
- Insert malformed typed rows and assert query/enumeration propagates a
  database error rather than omitting them.

### 7. Wrong vs Correct

#### Wrong

```rust
let remote = path.to_ascii_lowercase().starts_with("http://");
let track_id = normalize_path(path);
let fresh = row.scan_version == CURRENT_SCAN_VERSION;
```

#### Correct

```rust
let source = LoudnessSourceIdentity::from_location(location);
let fresh = match source.kind() {
    MediaLocationKind::Local => local_metadata_matches(&source, &row),
    MediaLocationKind::Http => false,
};
```

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
