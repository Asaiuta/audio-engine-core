//! Loudness Database Persistence
//!
//! SQLite storage for track loudness metadata following EBU R128 standard.
//! Enables pre-computed gain values for fast playback without real-time analysis.
//!
//! [`LoudnessSourceIdentity`] separates local and HTTP cache namespaces. Local
//! identity preserves the platform-native path representation; HTTP identity
//! hashes the validated request URL and persists only its origin. HTTP rows have
//! no content validator in this API, so [`LoudnessDatabase::needs_scan`] always
//! reports them stale instead of claiming scanner-version equality proves
//! freshness.

use crate::decoder::{MediaLocation, MediaLocationKind};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use thiserror::Error;

/// Current scanner algorithm version
/// Increment when measurement algorithm changes to trigger rescan
pub const CURRENT_SCAN_VERSION: i32 = 1;

const LOUDNESS_DB_SCHEMA_VERSION: i64 = 2;
const LOCAL_SOURCE_KIND: &str = "local";
const HTTP_SOURCE_KIND: &str = "http";
const CACHE_ID_DOMAIN: &[u8] = b"audio-engine-core:loudness-source:v1\0";

/// Default target loudness for streaming (LUFS)
///
/// Supported as a reference value for a consuming application choosing a
/// normalization target. Nothing in this crate applies it by default — the
/// target is always supplied explicitly through [`LoudnessConfig`].
///
/// [`LoudnessConfig`]: crate::config::LoudnessConfig
pub const DEFAULT_STREAMING_TARGET_LUFS: f64 = -14.0;

/// A safe, persisted identity for one local or HTTP media location.
///
/// HTTP identities contain a SHA-256 cache key and origin only. They never
/// retain URL userinfo, path, query, or fragment data. Local identities retain
/// the native [`PathBuf`] so freshness checks do not pass through UTF-8.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct LoudnessSourceIdentity {
    kind: MediaLocationKind,
    cache_id: String,
    local_path: Option<PathBuf>,
    display_label: String,
}

impl LoudnessSourceIdentity {
    /// Derive a namespaced cache identity from a validated media location.
    pub fn from_location(location: &MediaLocation) -> Self {
        match location {
            MediaLocation::Local(path) => {
                let encoded_path = encode_local_path(path);
                Self {
                    kind: MediaLocationKind::Local,
                    cache_id: cache_id(LOCAL_SOURCE_KIND, &encoded_path),
                    local_path: Some(path.clone()),
                    display_label: path.to_string_lossy().into_owned(),
                }
            }
            MediaLocation::Http(http) => Self {
                kind: MediaLocationKind::Http,
                cache_id: cache_id(HTTP_SOURCE_KIND, http.url().as_str().as_bytes()),
                local_path: None,
                display_label: http.log_identity(),
            },
        }
    }

    /// Return the source namespace.
    pub fn kind(&self) -> MediaLocationKind {
        self.kind
    }

    /// Return the non-secret, namespaced cache key.
    pub fn cache_id(&self) -> &str {
        &self.cache_id
    }

    /// Borrow the local path when this identity describes a local source.
    pub fn local_path(&self) -> Option<&Path> {
        self.local_path.as_deref()
    }

    /// Return the local display path or HTTP origin persisted with this row.
    pub fn display_label(&self) -> &str {
        &self.display_label
    }

    fn persisted_kind(&self) -> &'static str {
        match self.kind {
            MediaLocationKind::Local => LOCAL_SOURCE_KIND,
            MediaLocationKind::Http => HTTP_SOURCE_KIND,
        }
    }

    fn persisted_locator(&self) -> Option<Vec<u8>> {
        self.local_path.as_deref().map(encode_local_path)
    }

    fn from_persisted(
        persisted_cache_id: String,
        source_kind: &str,
        locator: Option<Vec<u8>>,
        display_label: String,
    ) -> Result<Self, PersistedSourceError> {
        match source_kind {
            LOCAL_SOURCE_KIND => {
                let encoded_path = locator.ok_or(PersistedSourceError::MissingLocalPath)?;
                let path = decode_local_path(&encoded_path)?;
                let expected_id = cache_id(LOCAL_SOURCE_KIND, &encoded_path);
                if persisted_cache_id != expected_id {
                    return Err(PersistedSourceError::CacheIdMismatch);
                }
                Ok(Self {
                    kind: MediaLocationKind::Local,
                    cache_id: persisted_cache_id,
                    display_label: path.to_string_lossy().into_owned(),
                    local_path: Some(path),
                })
            }
            HTTP_SOURCE_KIND => {
                if locator.is_some() {
                    return Err(PersistedSourceError::UnexpectedHttpLocator);
                }
                if !valid_cache_id(HTTP_SOURCE_KIND, &persisted_cache_id) {
                    return Err(PersistedSourceError::CacheIdMismatch);
                }
                let parsed = url::Url::parse(&display_label)
                    .map_err(|_| PersistedSourceError::UnsafeHttpLabel)?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || parsed.origin().ascii_serialization() != display_label
                {
                    return Err(PersistedSourceError::UnsafeHttpLabel);
                }
                Ok(Self {
                    kind: MediaLocationKind::Http,
                    cache_id: persisted_cache_id,
                    local_path: None,
                    display_label,
                })
            }
            _ => Err(PersistedSourceError::UnknownKind),
        }
    }
}

impl fmt::Debug for LoudnessSourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("LoudnessSourceIdentity");
        debug
            .field("kind", &self.kind)
            .field("cache_id", &self.cache_id);
        match self.kind {
            MediaLocationKind::Local => debug.field("path", &self.local_path),
            MediaLocationKind::Http => debug.field("origin", &self.display_label),
        };
        debug.finish()
    }
}

impl fmt::Display for LoudnessSourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display_label)
    }
}

#[derive(Debug, Error)]
enum PersistedSourceError {
    #[error("unknown persisted loudness source kind")]
    UnknownKind,
    #[error("persisted local loudness source has no path")]
    MissingLocalPath,
    #[error("persisted HTTP loudness source unexpectedly contains a locator")]
    UnexpectedHttpLocator,
    #[error("persisted loudness source cache ID does not match its identity")]
    CacheIdMismatch,
    #[error("persisted HTTP loudness source label is not an origin")]
    UnsafeHttpLabel,
    #[error("persisted loudness source does not match the requested identity")]
    RequestedIdentityMismatch,
    #[error("persisted local loudness path encoding is invalid")]
    InvalidLocalPathEncoding,
    #[error("persisted local loudness path was written for another platform")]
    UnsupportedLocalPathEncoding,
}

fn cache_id(namespace: &str, payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_ID_DOMAIN);
    hasher.update(namespace.as_bytes());
    hasher.update([0]);
    hasher.update(payload);
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(namespace.len() + 8 + digest.len() * 2);
    result.push_str(namespace);
    result.push_str(":sha256:");
    for byte in digest {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn valid_cache_id(namespace: &str, value: &str) -> bool {
    let prefix = format!("{namespace}:sha256:");
    value.strip_prefix(&prefix).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(unix)]
fn encode_local_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    let mut encoded = Vec::with_capacity(path.as_os_str().as_bytes().len() + 1);
    encoded.push(b'U');
    encoded.extend_from_slice(path.as_os_str().as_bytes());
    encoded
}

#[cfg(windows)]
fn encode_local_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    let mut encoded = Vec::new();
    encoded.push(b'W');
    for unit in path.as_os_str().encode_wide() {
        encoded.extend_from_slice(&unit.to_le_bytes());
    }
    encoded
}

#[cfg(not(any(unix, windows)))]
fn encode_local_path(path: &Path) -> Vec<u8> {
    let mut encoded = vec![b'S'];
    encoded.extend_from_slice(path.to_string_lossy().as_bytes());
    encoded
}

#[cfg(unix)]
fn decode_local_path(encoded: &[u8]) -> Result<PathBuf, PersistedSourceError> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    match encoded.split_first() {
        Some((&b'U', bytes)) => Ok(PathBuf::from(OsString::from_vec(bytes.to_vec()))),
        Some((&b'S', bytes)) => std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| PersistedSourceError::InvalidLocalPathEncoding),
        Some(_) => Err(PersistedSourceError::UnsupportedLocalPathEncoding),
        None => Err(PersistedSourceError::InvalidLocalPathEncoding),
    }
}

#[cfg(windows)]
fn decode_local_path(encoded: &[u8]) -> Result<PathBuf, PersistedSourceError> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let Some((&marker, bytes)) = encoded.split_first() else {
        return Err(PersistedSourceError::InvalidLocalPathEncoding);
    };
    if marker == b'S' {
        return std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| PersistedSourceError::InvalidLocalPathEncoding);
    }
    if marker != b'W' {
        return Err(PersistedSourceError::UnsupportedLocalPathEncoding);
    }
    let mut chunks = bytes.chunks_exact(2);
    let units = chunks
        .by_ref()
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    if !chunks.remainder().is_empty() {
        return Err(PersistedSourceError::InvalidLocalPathEncoding);
    }
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(not(any(unix, windows)))]
fn decode_local_path(encoded: &[u8]) -> Result<PathBuf, PersistedSourceError> {
    match encoded.split_first() {
        Some((&b'S', bytes)) => std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| PersistedSourceError::InvalidLocalPathEncoding),
        Some(_) => Err(PersistedSourceError::UnsupportedLocalPathEncoding),
        None => Err(PersistedSourceError::InvalidLocalPathEncoding),
    }
}

// ============================================================================
// Track Loudness Record
// ============================================================================

/// Loudness metadata for a single track
#[derive(Debug, Clone)]
pub struct TrackLoudness {
    /// Typed, non-secret cache identity for the analyzed source.
    pub source: LoudnessSourceIdentity,
    /// Integrated loudness in LUFS
    pub integrated_lufs: f64,
    /// True peak in dBTP
    pub true_peak_dbtp: f64,
    /// Loudness range in LU (optional)
    pub loudness_range: Option<f64>,
    /// Pre-computed track gain in dB (target - integrated)
    pub track_gain_db: f64,
    /// Album gain in dB (optional, for album mode)
    pub album_gain_db: Option<f64>,
    /// Scanner algorithm version
    pub scan_version: i32,
    /// Unix timestamp of scan
    pub scanned_at: i64,
    /// File modification time (Unix timestamp, for change detection)
    pub file_mtime: Option<i64>,
    /// File size in bytes (for change detection)
    pub file_size: Option<i64>,
    cached_gain_target_lufs: Cell<Option<f64>>,
    cached_gain_linear: Cell<f32>,
}

impl TrackLoudness {
    /// Create a new loudness record from measurement results
    ///
    /// Local metadata is captured for freshness checks. HTTP records carry no
    /// validator evidence and therefore remain stale by policy.
    pub fn new(
        location: &MediaLocation,
        integrated_lufs: f64,
        true_peak_dbtp: f64,
        loudness_range: Option<f64>,
        target_lufs: f64,
    ) -> Self {
        let source = LoudnessSourceIdentity::from_location(location);
        let track_gain_db = target_lufs - integrated_lufs;
        let (file_mtime, file_size) = Self::get_file_metadata(location);

        Self {
            source,
            integrated_lufs,
            true_peak_dbtp,
            loudness_range,
            track_gain_db,
            album_gain_db: None,
            scan_version: CURRENT_SCAN_VERSION,
            scanned_at: chrono_timestamp(),
            file_mtime,
            file_size,
            cached_gain_target_lufs: Cell::new(None),
            cached_gain_linear: Cell::new(1.0),
        }
    }

    fn get_file_metadata(location: &MediaLocation) -> (Option<i64>, Option<i64>) {
        let MediaLocation::Local(path) = location else {
            return (None, None);
        };
        std::fs::metadata(path)
            .ok()
            .map(|m| {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);
                let size = i64::try_from(m.len()).ok();
                (mtime, size)
            })
            .unwrap_or((None, None))
    }

    /// Get gain in dB for a specific target loudness
    pub fn gain_for_target(&self, target_lufs: f64) -> f64 {
        target_lufs - self.integrated_lufs
    }

    /// Convert dB gain to linear coefficient
    pub fn gain_linear(&self, target_lufs: f64) -> f32 {
        if self.cached_gain_target_lufs.get() == Some(target_lufs) {
            return self.cached_gain_linear.get();
        }

        let gain_db = self.gain_for_target(target_lufs);
        let gain = 10.0_f64.powf(gain_db / 20.0) as f32;
        self.cached_gain_target_lufs.set(Some(target_lufs));
        self.cached_gain_linear.set(gain);
        gain
    }
}

// ============================================================================
// Loudness Database
// ============================================================================

/// Failures from the optional SQLite loudness cache.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LoudnessDatabaseError {
    /// The parent directory for an on-disk database could not be created.
    #[error("failed to create loudness database directory")]
    CreateDirectory(#[from] std::io::Error),
    /// SQLite rejected an open, schema, query, or transaction operation.
    #[error("loudness database operation failed")]
    Database(#[from] rusqlite::Error),
    /// A previous panic poisoned the connection lock.
    #[error("loudness database lock poisoned")]
    LockPoisoned,
}

/// SQLite database for track loudness metadata
pub struct LoudnessDatabase {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

const TRACK_COLUMNS: &str = r#"
    track_id, source_kind, source_locator, source_label,
    integrated_lufs, true_peak_dbtp, loudness_range, track_gain_db,
    album_gain_db, scan_version, scanned_at, file_mtime, file_size
"#;

const UPSERT_TRACK_SQL: &str = r#"
    INSERT INTO track_loudness
        (track_id, source_kind, source_locator, source_label,
         integrated_lufs, true_peak_dbtp, loudness_range, track_gain_db,
         album_gain_db, scan_version, scanned_at, file_mtime, file_size)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
    ON CONFLICT(track_id) DO UPDATE SET
        source_kind = excluded.source_kind,
        source_locator = excluded.source_locator,
        source_label = excluded.source_label,
        integrated_lufs = excluded.integrated_lufs,
        true_peak_dbtp = excluded.true_peak_dbtp,
        loudness_range = excluded.loudness_range,
        track_gain_db = excluded.track_gain_db,
        album_gain_db = excluded.album_gain_db,
        scan_version = excluded.scan_version,
        scanned_at = excluded.scanned_at,
        file_mtime = excluded.file_mtime,
        file_size = excluded.file_size
"#;

fn row_source_identity(row: &rusqlite::Row<'_>) -> Result<LoudnessSourceIdentity, rusqlite::Error> {
    let cache_id: String = row.get(0)?;
    let source_kind: String = row.get(1)?;
    let locator: Option<Vec<u8>> = row.get(2)?;
    let display_label: String = row.get(3)?;
    LoudnessSourceIdentity::from_persisted(cache_id, &source_kind, locator, display_label)
        .map_err(persisted_source_sql_error)
}

fn persisted_source_sql_error(error: PersistedSourceError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn row_track_loudness(row: &rusqlite::Row<'_>) -> Result<TrackLoudness, rusqlite::Error> {
    Ok(TrackLoudness {
        source: row_source_identity(row)?,
        integrated_lufs: row.get(4)?,
        true_peak_dbtp: row.get(5)?,
        loudness_range: row.get(6)?,
        track_gain_db: row.get(7)?,
        album_gain_db: row.get(8)?,
        scan_version: row.get(9)?,
        scanned_at: row.get(10)?,
        file_mtime: row.get(11)?,
        file_size: row.get(12)?,
        cached_gain_target_lufs: Cell::new(None),
        cached_gain_linear: Cell::new(1.0),
    })
}

impl LoudnessDatabase {
    /// Open or create the loudness database
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, LoudnessDatabaseError> {
        let db_path = path.as_ref().to_path_buf();

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let conn = Connection::open(&db_path)?;

        let db = Self {
            conn: Mutex::new(conn),
            db_path,
        };

        db.init_schema()?;
        Ok(db)
    }

    /// Create an in-memory database (for testing)
    pub fn in_memory() -> Result<Self, LoudnessDatabaseError> {
        let conn = Connection::open_in_memory()?;

        let db = Self {
            conn: Mutex::new(conn),
            db_path: PathBuf::from(":memory:"),
        };

        db.init_schema()?;
        Ok(db)
    }

    /// Initialize the current typed-identity schema.
    ///
    /// The pre-1.0 string-path schema cannot distinguish local paths from HTTP
    /// URLs without guessing and may contain signed URLs. It is deliberately
    /// dropped rather than reinterpreted; loudness data is a disposable cache.
    fn init_schema(&self) -> Result<(), LoudnessDatabaseError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let user_version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let table_exists = tx
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'track_loudness'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let mut reset_legacy_schema = false;

        if table_exists {
            let mut stmt = tx.prepare("PRAGMA table_info(track_loudness)")?;
            let existing_columns: std::collections::HashSet<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<_, _>>()?;
            let required_columns = [
                "track_id",
                "source_kind",
                "source_locator",
                "source_label",
                "integrated_lufs",
                "true_peak_dbtp",
                "loudness_range",
                "track_gain_db",
                "album_gain_db",
                "scan_version",
                "scanned_at",
                "file_mtime",
                "file_size",
            ];
            let schema_matches = user_version == LOUDNESS_DB_SCHEMA_VERSION
                && required_columns
                    .iter()
                    .all(|column| existing_columns.contains(*column));
            drop(stmt);
            if !schema_matches {
                tx.execute("DROP TABLE track_loudness", [])?;
                reset_legacy_schema = true;
            }
        }

        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS track_loudness (
                track_id        TEXT PRIMARY KEY,
                source_kind     TEXT NOT NULL CHECK(source_kind IN ('local', 'http')),
                source_locator  BLOB,
                source_label    TEXT NOT NULL,
                integrated_lufs REAL NOT NULL,
                true_peak_dbtp  REAL NOT NULL,
                loudness_range  REAL,
                track_gain_db   REAL NOT NULL,
                album_gain_db   REAL,
                scan_version    INTEGER NOT NULL,
                scanned_at      INTEGER NOT NULL,
                file_mtime      INTEGER,
                file_size       INTEGER,
                CHECK (
                    (source_kind = 'local' AND source_locator IS NOT NULL) OR
                    (source_kind = 'http' AND source_locator IS NULL)
                )
            );

            CREATE INDEX IF NOT EXISTS idx_source_kind ON track_loudness(source_kind);
            CREATE INDEX IF NOT EXISTS idx_scan_version ON track_loudness(scan_version);
        "#,
        )?;
        tx.pragma_update(None, "user_version", LOUDNESS_DB_SCHEMA_VERSION)?;
        tx.commit()?;
        if reset_legacy_schema {
            conn.execute_batch("VACUUM")?;
        }
        Ok(())
    }

    /// Insert or update a track's loudness data
    pub fn upsert(&self, track: &TrackLoudness) -> Result<(), LoudnessDatabaseError> {
        let conn = self.connection()?;

        conn.execute(
            UPSERT_TRACK_SQL,
            params![
                track.source.cache_id(),
                track.source.persisted_kind(),
                track.source.persisted_locator(),
                track.source.display_label(),
                track.integrated_lufs,
                track.true_peak_dbtp,
                track.loudness_range,
                track.track_gain_db,
                track.album_gain_db,
                track.scan_version,
                track.scanned_at,
                track.file_mtime,
                track.file_size,
            ],
        )?;

        Ok(())
    }

    /// Get loudness data for a typed source identity.
    pub fn get(
        &self,
        source: &LoudnessSourceIdentity,
    ) -> Result<Option<TrackLoudness>, LoudnessDatabaseError> {
        let conn = self.connection()?;
        let query = format!("SELECT {TRACK_COLUMNS} FROM track_loudness WHERE track_id = ?1");
        let result = conn
            .query_row(&query, params![source.cache_id()], |row| {
                let track = row_track_loudness(row)?;
                if track.source != *source {
                    return Err(persisted_source_sql_error(
                        PersistedSourceError::RequestedIdentityMismatch,
                    ));
                }
                Ok(track)
            })
            .optional()?;

        Ok(result)
    }

    /// Get loudness data only when the cached record is still fresh.
    ///
    /// This centralizes the cache-hit contract used by both HTTP analysis
    /// handlers and playback loading: scan version, file mtime, and file size
    /// must all still match before a record may skip EBU R128 analysis.
    pub fn get_fresh(
        &self,
        source: &LoudnessSourceIdentity,
    ) -> Result<Option<TrackLoudness>, LoudnessDatabaseError> {
        if self.needs_scan(source)? {
            return Ok(None);
        }

        self.get(source)
    }

    /// Check if a track needs scanning (not in DB, wrong scanner version, file
    /// changed, or local file no longer readable).
    ///
    /// Freshness evidence differs by identity:
    ///
    /// - **Local**: scanner version, mtime, and size must all match. A file
    ///   that cannot be stat-ed — deleted, renamed, on an unmounted volume, or
    ///   permission-denied — is reported as needing a scan, because there is no
    ///   evidence the stored measurement still describes it.
    /// - **HTTP**: always needs a scan because this API stores no ETag,
    ///   Last-Modified value, content digest, or explicit caller revision.
    ///
    /// The version comparison is exact rather than `<`: a record written by a
    /// newer scanner is not something this build can vouch for either, so it is
    /// rescanned instead of trusted.
    pub fn needs_scan(
        &self,
        source: &LoudnessSourceIdentity,
    ) -> Result<bool, LoudnessDatabaseError> {
        let conn = self.connection()?;
        let result: Option<(String, i32, Option<i64>, Option<i64>)> = conn
            .query_row(
                "SELECT source_kind, scan_version, file_mtime, file_size \
                 FROM track_loudness WHERE track_id = ?1",
                params![source.cache_id()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;

        match result {
            None => Ok(true), // Not in database
            Some((stored_kind, version, db_mtime, db_size)) => {
                if stored_kind != source.persisted_kind() || version != CURRENT_SCAN_VERSION {
                    return Ok(true); // Recorded by a different scanner
                }

                if source.kind() == MediaLocationKind::Http {
                    return Ok(true);
                }

                let Some(path) = source.local_path() else {
                    return Ok(true);
                };
                let Ok(metadata) = std::fs::metadata(path) else {
                    log::info!(
                        "File no longer readable, needs rescan before its cached loudness is used: {}",
                        source
                    );
                    return Ok(true);
                };

                let current_mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);
                let current_size = i64::try_from(metadata.len()).ok();

                if current_mtime != db_mtime || current_size != db_size {
                    log::info!(
                        "File changed, needs rescan: {} (mtime: {:?} -> {:?}, size: {:?} -> {:?})",
                        source,
                        db_mtime,
                        current_mtime,
                        db_size,
                        current_size
                    );
                    return Ok(true);
                }

                Ok(false)
            }
        }
    }

    /// Get identities known to be stale without probing local files.
    ///
    /// This includes every HTTP row and every scanner-version mismatch. A row
    /// whose typed identity cannot be decoded fails the whole call rather than
    /// being silently omitted.
    pub fn get_outdated_tracks(
        &self,
    ) -> Result<Vec<LoudnessSourceIdentity>, LoudnessDatabaseError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT track_id, source_kind, source_locator, source_label \
             FROM track_loudness WHERE scan_version != ?1 OR source_kind = ?2",
        )?;
        let tracks = stmt
            .query_map(
                params![CURRENT_SCAN_VERSION, HTTP_SOURCE_KIND],
                row_source_identity,
            )?
            .collect::<Result<Vec<LoudnessSourceIdentity>, _>>()?;

        Ok(tracks)
    }

    /// Batch insert multiple tracks (for initial scan)
    pub fn batch_upsert(&self, tracks: &[TrackLoudness]) -> Result<usize, LoudnessDatabaseError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;

        let mut count = 0;
        for track in tracks {
            tx.execute(
                UPSERT_TRACK_SQL,
                params![
                    track.source.cache_id(),
                    track.source.persisted_kind(),
                    track.source.persisted_locator(),
                    track.source.display_label(),
                    track.integrated_lufs,
                    track.true_peak_dbtp,
                    track.loudness_range,
                    track.track_gain_db,
                    track.album_gain_db,
                    track.scan_version,
                    track.scanned_at,
                    track.file_mtime,
                    track.file_size,
                ],
            )?;
            count += 1;
        }

        tx.commit()?;
        Ok(count)
    }

    /// Update album gain for multiple tracks (same album)
    ///
    /// FIX for Defect 41: Wrap in transaction for atomicity.
    /// If any update fails or process crashes, all changes are rolled back.
    pub fn set_album_gain(
        &self,
        sources: &[LoudnessSourceIdentity],
        album_gain_db: f64,
    ) -> Result<(), LoudnessDatabaseError> {
        let mut conn = self.connection()?;

        // FIX for Defect 41: Use transaction for atomic batch update
        let tx = conn.transaction()?;

        for source in sources {
            tx.execute(
                "UPDATE track_loudness SET album_gain_db = ?1 WHERE track_id = ?2",
                params![album_gain_db, source.cache_id()],
            )?;
        }

        tx.commit()?;

        Ok(())
    }

    /// Delete a track from the database by typed source identity.
    pub fn delete(&self, source: &LoudnessSourceIdentity) -> Result<bool, LoudnessDatabaseError> {
        let conn = self.connection()?;
        let affected = conn.execute(
            "DELETE FROM track_loudness WHERE track_id = ?1",
            params![source.cache_id()],
        )?;

        Ok(affected > 0)
    }

    /// Get database statistics
    pub fn stats(&self) -> Result<DatabaseStats, LoudnessDatabaseError> {
        let conn = self.connection()?;

        let total_tracks: i64 =
            conn.query_row("SELECT COUNT(*) FROM track_loudness", [], |row| row.get(0))?;

        let outdated_tracks: i64 = conn.query_row(
            "SELECT COUNT(*) FROM track_loudness \
             WHERE scan_version != ?1 OR source_kind = ?2",
            params![CURRENT_SCAN_VERSION, HTTP_SOURCE_KIND],
            |row| row.get(0),
        )?;

        let with_album_gain: i64 = conn.query_row(
            "SELECT COUNT(*) FROM track_loudness WHERE album_gain_db IS NOT NULL",
            [],
            |row| row.get(0),
        )?;

        Ok(DatabaseStats {
            total_tracks,
            outdated_tracks,
            with_album_gain,
            current_scan_version: CURRENT_SCAN_VERSION,
        })
    }

    /// Get database path
    pub fn path(&self) -> &Path {
        &self.db_path
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, LoudnessDatabaseError> {
        self.conn
            .lock()
            .map_err(|_| LoudnessDatabaseError::LockPoisoned)
    }
}

// ============================================================================
// Database Statistics
// ============================================================================

/// Statistics about the loudness database
#[derive(Debug, Clone, serde::Serialize)]
pub struct DatabaseStats {
    /// Total number of tracked media entries.
    pub total_tracks: i64,
    /// Entries whose cached loudness metadata is stale.
    pub outdated_tracks: i64,
    /// Entries carrying an album-gain value.
    pub with_album_gain: i64,
    /// Scan format version the cached rows were written with.
    pub current_scan_version: i32,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get current Unix timestamp in seconds
fn chrono_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn local(path: impl Into<PathBuf>) -> MediaLocation {
        MediaLocation::local(path)
    }

    fn http(url: &str) -> MediaLocation {
        MediaLocation::http(url).expect("test HTTP URL must be valid")
    }

    #[test]
    fn test_database_basic_operations() {
        let db = LoudnessDatabase::in_memory().unwrap();

        let location = local("/music/test.flac");
        let track = TrackLoudness::new(
            &location,
            -18.5,     // integrated_lufs
            -0.5,      // true_peak_dbtp
            Some(6.2), // loudness_range
            DEFAULT_STREAMING_TARGET_LUFS,
        );

        // Insert
        db.upsert(&track).unwrap();

        // Retrieve
        let retrieved = db.get(&track.source).unwrap().unwrap();
        assert_eq!(retrieved.integrated_lufs, -18.5);
        assert_eq!(retrieved.track_gain_db, 4.5); // -14 - (-18.5)

        // Check needs_scan. `/music/test.flac` does not exist, so there is no
        // evidence the stored measurement still describes it.
        assert!(db.needs_scan(&track.source).unwrap());
        let other = LoudnessSourceIdentity::from_location(&local("/music/other.flac"));
        assert!(db.needs_scan(&other).unwrap());
    }

    #[test]
    fn test_gain_calculation() {
        let track = TrackLoudness::new(&local("/test.flac"), -20.0, -1.0, None, -14.0);

        assert_eq!(track.track_gain_db, 6.0); // -14 - (-20)
        assert!((track.gain_linear(-14.0) - 1.995).abs() < 0.01);

        // Different target
        assert_eq!(track.gain_for_target(-23.0), -3.0);
    }

    #[test]
    fn track_gain_linear_reuses_same_target_and_invalidates_on_change() {
        let track = TrackLoudness::new(&local("/test.flac"), -20.0, -1.0, None, -14.0);

        let first = track.gain_linear(-14.0);
        let second = track.gain_linear(-14.0);
        let third = track.gain_linear(-23.0);

        assert_eq!(first.to_bits(), second.to_bits());
        assert_eq!(first.to_bits(), track.gain_linear(-14.0).to_bits());
        assert_eq!(third.to_bits(), track.gain_linear(-23.0).to_bits());
        assert_ne!(first.to_bits(), third.to_bits());
    }

    #[test]
    fn get_fresh_rejects_changed_local_file() {
        let db = LoudnessDatabase::in_memory().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "audio_player_loudness_fresh_{}_{}.flac",
            std::process::id(),
            unique
        ));
        std::fs::write(&path, b"initial").unwrap();
        let location = local(path.clone());
        let track = TrackLoudness::new(&location, -18.0, -1.0, None, -14.0);
        db.upsert(&track).unwrap();
        assert!(db.get_fresh(&track.source).unwrap().is_some());

        std::fs::write(&path, b"changed file contents").unwrap();
        assert!(db.get_fresh(&track.source).unwrap().is_none());

        let _ = std::fs::remove_file(path);
    }

    fn unique_temp_path(tag: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "audio_player_loudness_{}_{}_{}.flac",
            tag,
            std::process::id(),
            unique
        ))
    }

    /// A file that vanished, moved, or became unreadable has no freshness
    /// evidence at all. Reporting it fresh served a stale gain for a path whose
    /// contents nobody could confirm.
    #[test]
    fn an_unreadable_local_file_needs_a_rescan() {
        let db = LoudnessDatabase::in_memory().unwrap();
        let path = unique_temp_path("missing");
        std::fs::write(&path, b"initial").unwrap();
        let track = TrackLoudness::new(&local(path.clone()), -18.0, -1.0, None, -14.0);
        db.upsert(&track).unwrap();
        assert!(!db.needs_scan(&track.source).unwrap());

        std::fs::remove_file(&path).unwrap();

        assert!(db.needs_scan(&track.source).unwrap());
        assert!(db.get_fresh(&track.source).unwrap().is_none());
        // The record itself survives; only its freshness claim is withdrawn.
        assert!(db.get(&track.source).unwrap().is_some());
    }

    /// A record written by a *newer* scanner is no more trustworthy to this
    /// build than an older one, so exact version matching is required.
    #[test]
    fn a_newer_scanner_version_also_needs_a_rescan() {
        let db = LoudnessDatabase::in_memory().unwrap();
        let path = unique_temp_path("version");
        std::fs::write(&path, b"initial").unwrap();
        let mut track = TrackLoudness::new(&local(path.clone()), -18.0, -1.0, None, -14.0);
        track.scan_version = CURRENT_SCAN_VERSION + 1;
        db.upsert(&track).unwrap();

        assert!(db.needs_scan(&track.source).unwrap());
        assert!(db.get_fresh(&track.source).unwrap().is_none());
        assert_eq!(db.get_outdated_tracks().unwrap(), vec![track.source]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn validator_less_http_records_are_always_stale() {
        let db = LoudnessDatabase::in_memory().unwrap();

        for url in [
            "http://host/track.flac",
            "HTTPS://host/track.flac",
            "HtTp://host/track.flac",
        ] {
            let track = TrackLoudness::new(&http(url), -18.0, -1.0, None, -14.0);
            db.upsert(&track).unwrap();
            assert!(db.needs_scan(&track.source).unwrap());
            assert!(db.get_fresh(&track.source).unwrap().is_none());
            assert!(db.get(&track.source).unwrap().is_some());
            assert!(db.get_outdated_tracks().unwrap().contains(&track.source));
        }
    }

    #[test]
    fn cache_namespaces_and_case_sensitive_url_components_do_not_collide() {
        let text = "https://host/Track.flac?Token=ABC";
        let local_identity = LoudnessSourceIdentity::from_location(&local(text));
        let http_identity = LoudnessSourceIdentity::from_location(&http(text));
        let changed_http =
            LoudnessSourceIdentity::from_location(&http("https://host/track.flac?Token=abc"));
        let changed_local =
            LoudnessSourceIdentity::from_location(&local("https://host/track.flac?Token=ABC"));

        assert!(local_identity.cache_id().starts_with("local:sha256:"));
        assert!(http_identity.cache_id().starts_with("http:sha256:"));
        assert_ne!(local_identity.cache_id(), http_identity.cache_id());
        assert_ne!(http_identity.cache_id(), changed_http.cache_id());
        // Local cache identity uses exact native path spelling on every target;
        // it does not guess filesystem aliasing or canonicalize through I/O.
        assert_ne!(local_identity.cache_id(), changed_local.cache_id());
    }

    #[test]
    fn signed_http_url_secrets_are_absent_from_records_and_rows() {
        let db = LoudnessDatabase::in_memory().unwrap();
        let location = http(
            "https://alice:password@host:8443/private/path-token.flac?signature=query-secret#fragment-secret",
        );
        let track = TrackLoudness::new(&location, -18.0, -1.0, None, -14.0);

        let rendered = format!("{:?} {}", track.source, track.source);
        for secret in [
            "alice",
            "password",
            "private",
            "path-token",
            "query-secret",
            "fragment-secret",
        ] {
            assert!(!rendered.contains(secret));
            assert!(!track.source.cache_id().contains(secret));
        }
        assert_eq!(track.source.display_label(), "https://host:8443");

        db.upsert(&track).unwrap();
        let conn = db.connection().unwrap();
        let (cache_id, label, locator): (String, String, Option<Vec<u8>>) = conn
            .query_row(
                "SELECT track_id, source_label, source_locator FROM track_loudness",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(cache_id, track.source.cache_id());
        assert_eq!(label, "https://host:8443");
        assert!(locator.is_none());
        drop(conn);

        db.connection()
            .unwrap()
            .execute(
                "UPDATE track_loudness SET source_label = 'https://other.invalid'",
                [],
            )
            .unwrap();
        assert!(matches!(
            db.get(&track.source),
            Err(LoudnessDatabaseError::Database(_))
        ));
    }

    #[test]
    fn native_local_path_round_trips_through_the_database() {
        #[cfg(unix)]
        let path = {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(OsString::from_vec(b"/tmp/non-utf8-\xff.flac".to_vec()))
        };
        #[cfg(windows)]
        let path = {
            use std::ffi::OsString;
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(OsString::from_wide(&[
                b'C' as u16,
                b':' as u16,
                b'\\' as u16,
                0xd800,
                b'.' as u16,
                b'f' as u16,
                b'l' as u16,
                b'a' as u16,
                b'c' as u16,
            ]))
        };
        #[cfg(not(any(unix, windows)))]
        let path = PathBuf::from("native-path.flac");

        let db = LoudnessDatabase::in_memory().unwrap();
        let track = TrackLoudness::new(&local(path.clone()), -18.0, -1.0, None, -14.0);
        db.upsert(&track).unwrap();

        let restored = db.get(&track.source).unwrap().unwrap();
        assert_eq!(restored.source.local_path(), Some(path.as_path()));
        assert_eq!(restored.source.cache_id(), track.source.cache_id());
    }

    #[test]
    fn legacy_string_schema_is_invalidated_idempotently() {
        let path = unique_temp_path("legacy_schema").with_extension("sqlite");
        let legacy_secret = "legacy-query-secret";
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE track_loudness (
                    track_id TEXT PRIMARY KEY,
                    file_path TEXT NOT NULL,
                    integrated_lufs REAL NOT NULL,
                    true_peak_dbtp REAL NOT NULL,
                    loudness_range REAL,
                    track_gain_db REAL NOT NULL,
                    album_gain_db REAL,
                    scan_version INTEGER NOT NULL,
                    scanned_at INTEGER NOT NULL,
                    file_mtime INTEGER,
                    file_size INTEGER
                );
                "#,
            )
            .unwrap();
            conn.execute(
                "INSERT INTO track_loudness VALUES (?1, ?2, -18.0, -1.0, NULL, 4.0, NULL, 1, 0, NULL, NULL)",
                params!["legacy-id", format!("https://host/a.flac?{legacy_secret}")],
            )
            .unwrap();
        }

        {
            let db = LoudnessDatabase::open(&path).unwrap();
            assert_eq!(db.stats().unwrap().total_tracks, 0);
            assert_eq!(
                db.connection()
                    .unwrap()
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                LOUDNESS_DB_SCHEMA_VERSION
            );
        }
        // Opening the current schema again must preserve it rather than reset it.
        {
            let db = LoudnessDatabase::open(&path).unwrap();
            let track = TrackLoudness::new(&local("stable.flac"), -18.0, -1.0, None, -14.0);
            db.upsert(&track).unwrap();
        }
        {
            let db = LoudnessDatabase::open(&path).unwrap();
            assert_eq!(db.stats().unwrap().total_tracks, 1);
        }

        let database_bytes = std::fs::read(&path).unwrap();
        assert!(!database_bytes
            .windows(legacy_secret.len())
            .any(|window| window == legacy_secret.as_bytes()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn malformed_outdated_identity_is_not_silently_dropped() {
        let db = LoudnessDatabase::in_memory().unwrap();
        db.connection()
            .unwrap()
            .execute(
                r#"
                INSERT INTO track_loudness (
                    track_id, source_kind, source_locator, source_label,
                    integrated_lufs, true_peak_dbtp, loudness_range, track_gain_db,
                    album_gain_db, scan_version, scanned_at, file_mtime, file_size
                ) VALUES (?1, 'http', NULL, ?2, -18.0, -1.0, NULL, 4.0, NULL, 1, 0, NULL, NULL)
                "#,
                params![
                    cache_id(HTTP_SOURCE_KIND, b"corrupt"),
                    "https://host/private"
                ],
            )
            .unwrap();

        assert!(matches!(
            db.get_outdated_tracks(),
            Err(LoudnessDatabaseError::Database(_))
        ));
    }

    #[test]
    fn open_preserves_io_and_database_error_classes() {
        let file = unique_temp_path("error_classes");
        std::fs::write(&file, b"not a database").unwrap();

        let nested_database = file.join("missing").join("loudness.sqlite");
        let io_error = match LoudnessDatabase::open(nested_database) {
            Err(error) => error,
            Ok(_) => panic!("directory creation unexpectedly succeeded"),
        };
        assert!(matches!(
            &io_error,
            LoudnessDatabaseError::CreateDirectory(_)
        ));
        assert!(std::error::Error::source(&io_error).is_some());

        let database_error = match LoudnessDatabase::open(&file) {
            Err(error) => error,
            Ok(_) => panic!("invalid SQLite file unexpectedly opened"),
        };
        assert!(matches!(
            &database_error,
            LoudnessDatabaseError::Database(_)
        ));
        assert!(std::error::Error::source(&database_error).is_some());

        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn poisoned_connection_has_a_stable_error_class() {
        let db = LoudnessDatabase::in_memory().unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = db.conn.lock().unwrap();
            panic!("poison loudness database lock");
        }));
        assert!(panic.is_err());
        assert!(matches!(
            db.stats(),
            Err(LoudnessDatabaseError::LockPoisoned)
        ));
    }
}
