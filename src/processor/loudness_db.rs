//! Loudness Database Persistence
//!
//! SQLite storage for track loudness metadata following EBU R128 standard.
//! Enables pre-computed gain values for fast playback without real-time analysis.
//!
//! # Known freshness limitations
//!
//! [`LoudnessDatabase::needs_scan`] is the single freshness contract, but its
//! evidence is not equally strong for every identity:
//!
//! - A remote (HTTP) identity stores no mtime or size, so a replaced remote
//!   body is not detected. Only the scanner version guards it.
//! - Local change detection uses whole-second mtime plus size, so a same-size
//!   replacement inside one second of the recorded mtime is missed.
//! - [`TrackLoudness::compute_track_id`] normalizes separators but does not
//!   canonicalize local paths, so `./a.flac` and `/music/a.flac` are two rows
//!   for one file.
//!
//! These are recorded rather than silently accepted; fixing them needs distinct
//! typed local/remote identities, which is a larger change than the freshness
//! logic itself.

use rusqlite::{params, Connection, OptionalExtension};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Current scanner algorithm version
/// Increment when measurement algorithm changes to trigger rescan
pub const CURRENT_SCAN_VERSION: i32 = 1;

/// Default target loudness for streaming (LUFS)
///
/// Supported as a reference value for a consuming application choosing a
/// normalization target. Nothing in this crate applies it by default — the
/// target is always supplied explicitly through [`LoudnessConfig`].
///
/// [`LoudnessConfig`]: crate::config::LoudnessConfig
pub const DEFAULT_STREAMING_TARGET_LUFS: f64 = -14.0;

/// Schemes stored as remote track identities.
///
/// Matched case-insensitively, because RFC 3986 defines schemes that way and
/// the decoder's own source router already does; a case-sensitive check here
/// would classify `HTTPS://host/track.flac` as a local path and then look for
/// a file by that name.
const REMOTE_SCHEMES: [&str; 2] = ["http://", "https://"];

fn is_remote_path(path: &str) -> bool {
    REMOTE_SCHEMES.iter().any(|scheme| {
        path.len() >= scheme.len() && path[..scheme.len()].eq_ignore_ascii_case(scheme)
    })
}

// ============================================================================
// Track Loudness Record
// ============================================================================

/// Loudness metadata for a single track
#[derive(Debug, Clone)]
pub struct TrackLoudness {
    /// Unique identifier (file path or hash)
    pub track_id: String,
    /// Original file path
    pub file_path: String,
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
    /// FIX for Defect 40: Track file changes
    pub file_mtime: Option<i64>,
    /// File size in bytes (for change detection)
    /// FIX for Defect 40: Track file changes
    pub file_size: Option<i64>,
    cached_gain_target_lufs: Cell<Option<f64>>,
    cached_gain_linear: Cell<f32>,
}

impl TrackLoudness {
    /// Create a new loudness record from measurement results
    ///
    /// FIX for Defect 40: Record file mtime and size for change detection.
    /// If file metadata cannot be read (e.g., HTTP URL), these will be None.
    pub fn new(
        file_path: &str,
        integrated_lufs: f64,
        true_peak_dbtp: f64,
        loudness_range: Option<f64>,
        target_lufs: f64,
    ) -> Self {
        let track_id = Self::compute_track_id(file_path);
        let track_gain_db = target_lufs - integrated_lufs;

        // FIX for Defect 40: Get file metadata for change detection
        let (file_mtime, file_size) = Self::get_file_metadata(file_path);

        Self {
            track_id,
            file_path: file_path.to_string(),
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

    /// Get file modification time and size for change detection
    /// Returns (mtime, size) or (None, None) if file is not local
    fn get_file_metadata(path: &str) -> (Option<i64>, Option<i64>) {
        if is_remote_path(path) {
            return (None, None);
        }

        std::fs::metadata(path)
            .ok()
            .map(|m| {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);
                let size = Some(m.len() as i64);
                (mtime, size)
            })
            .unwrap_or((None, None))
    }

    /// Compute a unique track ID from file path.
    ///
    /// Path separators are normalized to `/` on every platform. Case is only
    /// folded on Windows, whose filesystem is case-insensitive: on Linux/macOS
    /// `Track.flac` and `track.flac` are distinct files and must not collide on
    /// one loudness record. Windows keys are unchanged from the previous
    /// always-lowercase behavior, so existing databases stay valid.
    fn compute_track_id(path: &str) -> String {
        let normalized = path.replace('\\', "/");
        #[cfg(windows)]
        {
            normalized.to_lowercase()
        }
        #[cfg(not(windows))]
        {
            normalized
        }
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

/// SQLite database for track loudness metadata
pub struct LoudnessDatabase {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

impl LoudnessDatabase {
    /// Open or create the loudness database
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let db_path = path.as_ref().to_path_buf();

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create database directory: {}", e))?;
            }
        }

        let conn =
            Connection::open(&db_path).map_err(|e| format!("Failed to open database: {}", e))?;

        let db = Self {
            conn: Mutex::new(conn),
            db_path,
        };

        db.init_schema()?;
        Ok(db)
    }

    /// Create an in-memory database (for testing)
    pub fn in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory()
            .map_err(|e| format!("Failed to create in-memory database: {}", e))?;

        let db = Self {
            conn: Mutex::new(conn),
            db_path: PathBuf::from(":memory:"),
        };

        db.init_schema()?;
        Ok(db)
    }

    /// Initialize database schema
    fn init_schema(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS track_loudness (
                track_id        TEXT PRIMARY KEY,
                file_path       TEXT NOT NULL,
                integrated_lufs REAL NOT NULL,
                true_peak_dbtp  REAL NOT NULL,
                loudness_range  REAL,
                track_gain_db   REAL NOT NULL,
                album_gain_db   REAL,
                scan_version    INTEGER NOT NULL,
                scanned_at      INTEGER NOT NULL,
                file_mtime      INTEGER,
                file_size       INTEGER
            );
            
            CREATE INDEX IF NOT EXISTS idx_file_path ON track_loudness(file_path);
            CREATE INDEX IF NOT EXISTS idx_scan_version ON track_loudness(scan_version);
        "#,
        )
        .map_err(|e| format!("Failed to initialize schema: {}", e))?;

        let mut stmt = conn
            .prepare("PRAGMA table_info(track_loudness)")
            .map_err(|e| format!("Failed to inspect schema: {}", e))?;
        let existing_columns: std::collections::HashSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("Failed to read schema info: {}", e))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("Failed to collect schema info: {}", e))?;

        if !existing_columns.contains("file_mtime") {
            conn.execute(
                "ALTER TABLE track_loudness ADD COLUMN file_mtime INTEGER",
                [],
            )
            .map_err(|e| format!("Failed to migrate schema (file_mtime): {}", e))?;
        }
        if !existing_columns.contains("file_size") {
            conn.execute(
                "ALTER TABLE track_loudness ADD COLUMN file_size INTEGER",
                [],
            )
            .map_err(|e| format!("Failed to migrate schema (file_size): {}", e))?;
        }

        Ok(())
    }

    /// Insert or update a track's loudness data
    pub fn upsert(&self, track: &TrackLoudness) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;

        conn.execute(
            r#"
            INSERT INTO track_loudness 
                (track_id, file_path, integrated_lufs, true_peak_dbtp, 
                 loudness_range, track_gain_db, album_gain_db, scan_version, scanned_at,
                 file_mtime, file_size)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(track_id) DO UPDATE SET
                file_path = excluded.file_path,
                integrated_lufs = excluded.integrated_lufs,
                true_peak_dbtp = excluded.true_peak_dbtp,
                loudness_range = excluded.loudness_range,
                track_gain_db = excluded.track_gain_db,
                album_gain_db = excluded.album_gain_db,
                scan_version = excluded.scan_version,
                scanned_at = excluded.scanned_at,
                file_mtime = excluded.file_mtime,
                file_size = excluded.file_size
            "#,
            params![
                track.track_id,
                track.file_path,
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
        )
        .map_err(|e| format!("Failed to upsert track: {}", e))?;

        Ok(())
    }

    /// Get loudness data for a track by file path
    pub fn get(&self, file_path: &str) -> Result<Option<TrackLoudness>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let track_id = TrackLoudness::compute_track_id(file_path);

        let result = conn
            .query_row(
                r#"
            SELECT track_id, file_path, integrated_lufs, true_peak_dbtp,
                   loudness_range, track_gain_db, album_gain_db, scan_version, scanned_at,
                   file_mtime, file_size
            FROM track_loudness
            WHERE track_id = ?1
            "#,
                params![track_id],
                |row| {
                    Ok(TrackLoudness {
                        track_id: row.get(0)?,
                        file_path: row.get(1)?,
                        integrated_lufs: row.get(2)?,
                        true_peak_dbtp: row.get(3)?,
                        loudness_range: row.get(4)?,
                        track_gain_db: row.get(5)?,
                        album_gain_db: row.get(6)?,
                        scan_version: row.get(7)?,
                        scanned_at: row.get(8)?,
                        file_mtime: row.get(9)?,
                        file_size: row.get(10)?,
                        cached_gain_target_lufs: Cell::new(None),
                        cached_gain_linear: Cell::new(1.0),
                    })
                },
            )
            .optional()
            .map_err(|e| format!("Failed to query track: {}", e))?;

        Ok(result)
    }

    /// Get loudness data only when the cached record is still fresh.
    ///
    /// This centralizes the cache-hit contract used by both HTTP analysis
    /// handlers and playback loading: scan version, file mtime, and file size
    /// must all still match before a record may skip EBU R128 analysis.
    pub fn get_fresh(&self, file_path: &str) -> Result<Option<TrackLoudness>, String> {
        if self.needs_scan(file_path)? {
            return Ok(None);
        }

        self.get(file_path)
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
    /// - **Remote**: only the scanner version is checked. HTTP identities carry
    ///   no stored mtime/size, so a replaced remote body is *not* detected. A
    ///   caller that must notice remote replacement has to invalidate the entry
    ///   itself; see this module's known limitation.
    ///
    /// The version comparison is exact rather than `<`: a record written by a
    /// newer scanner is not something this build can vouch for either, so it is
    /// rescanned instead of trusted.
    pub fn needs_scan(&self, file_path: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let track_id = TrackLoudness::compute_track_id(file_path);

        let result: Option<(i32, Option<i64>, Option<i64>)> = conn.query_row(
            "SELECT scan_version, file_mtime, file_size FROM track_loudness WHERE track_id = ?1",
            params![track_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().map_err(|e| format!("Failed to check track: {}", e))?;

        match result {
            None => Ok(true), // Not in database
            Some((version, db_mtime, db_size)) => {
                if version != CURRENT_SCAN_VERSION {
                    return Ok(true); // Recorded by a different scanner
                }

                if is_remote_path(file_path) {
                    return Ok(false); // Version-only freshness; see the doc above
                }

                let Ok(metadata) = std::fs::metadata(file_path) else {
                    log::info!(
                        "File no longer readable, needs rescan before its cached loudness is used: {}",
                        file_path
                    );
                    return Ok(true);
                };

                let current_mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);
                let current_size = Some(metadata.len() as i64);

                if current_mtime != db_mtime || current_size != db_size {
                    log::info!(
                        "File changed, needs rescan: {} (mtime: {:?} -> {:?}, size: {:?} -> {:?})",
                        file_path,
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

    /// Get all tracks whose stored scan version differs from this build's.
    ///
    /// A row whose `file_path` cannot be decoded fails the whole call rather
    /// than being dropped: a silently short list reads as "nothing else to
    /// rescan", which is the opposite of what a decode failure means.
    pub fn get_outdated_tracks(&self) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;

        let mut stmt = conn
            .prepare("SELECT file_path FROM track_loudness WHERE scan_version != ?1")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let tracks = stmt
            .query_map(params![CURRENT_SCAN_VERSION], |row| row.get(0))
            .map_err(|e| format!("Failed to query outdated tracks: {}", e))?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| format!("Failed to decode outdated track row: {}", e))?;

        Ok(tracks)
    }

    /// Batch insert multiple tracks (for initial scan)
    pub fn batch_upsert(&self, tracks: &[TrackLoudness]) -> Result<usize, String> {
        let mut conn = self.conn.lock().map_err(|e| e.to_string())?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to begin transaction: {}", e))?;

        let mut count = 0;
        for track in tracks {
            tx.execute(
                r#"
                INSERT INTO track_loudness 
                    (track_id, file_path, integrated_lufs, true_peak_dbtp, 
                     loudness_range, track_gain_db, album_gain_db, scan_version, scanned_at,
                     file_mtime, file_size)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(track_id) DO UPDATE SET
                    file_path = excluded.file_path,
                    integrated_lufs = excluded.integrated_lufs,
                    true_peak_dbtp = excluded.true_peak_dbtp,
                    loudness_range = excluded.loudness_range,
                    track_gain_db = excluded.track_gain_db,
                    album_gain_db = excluded.album_gain_db,
                    scan_version = excluded.scan_version,
                    scanned_at = excluded.scanned_at,
                    file_mtime = excluded.file_mtime,
                    file_size = excluded.file_size
                "#,
                params![
                    track.track_id,
                    track.file_path,
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
            )
            .map_err(|e| format!("Failed to upsert track {}: {}", track.file_path, e))?;
            count += 1;
        }

        tx.commit()
            .map_err(|e| format!("Failed to commit transaction: {}", e))?;
        Ok(count)
    }

    /// Update album gain for multiple tracks (same album)
    ///
    /// FIX for Defect 41: Wrap in transaction for atomicity.
    /// If any update fails or process crashes, all changes are rolled back.
    pub fn set_album_gain(&self, track_ids: &[&str], album_gain_db: f64) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|e| e.to_string())?;

        // FIX for Defect 41: Use transaction for atomic batch update
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to begin transaction: {}", e))?;

        for track_id in track_ids {
            tx.execute(
                "UPDATE track_loudness SET album_gain_db = ?1 WHERE track_id = ?2",
                params![album_gain_db, track_id],
            )
            .map_err(|e| format!("Failed to update album gain for {}: {}", track_id, e))?;
        }

        tx.commit()
            .map_err(|e| format!("Failed to commit album gain transaction: {}", e))?;

        Ok(())
    }

    /// Delete a track from the database
    pub fn delete(&self, file_path: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let track_id = TrackLoudness::compute_track_id(file_path);

        let affected = conn
            .execute(
                "DELETE FROM track_loudness WHERE track_id = ?1",
                params![track_id],
            )
            .map_err(|e| format!("Failed to delete track: {}", e))?;

        Ok(affected > 0)
    }

    /// Get database statistics
    pub fn stats(&self) -> Result<DatabaseStats, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;

        let total_tracks: i64 = conn
            .query_row("SELECT COUNT(*) FROM track_loudness", [], |row| row.get(0))
            .map_err(|e| format!("Failed to count tracks: {}", e))?;

        let outdated_tracks: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM track_loudness WHERE scan_version < ?1",
                params![CURRENT_SCAN_VERSION],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count outdated tracks: {}", e))?;

        let with_album_gain: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM track_loudness WHERE album_gain_db IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count album gain tracks: {}", e))?;

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
}

// ============================================================================
// Database Statistics
// ============================================================================

/// Statistics about the loudness database
#[derive(Debug, Clone, serde::Serialize)]
pub struct DatabaseStats {
    pub total_tracks: i64,
    pub outdated_tracks: i64,
    pub with_album_gain: i64,
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

    #[test]
    fn test_database_basic_operations() {
        let db = LoudnessDatabase::in_memory().unwrap();

        let track = TrackLoudness::new(
            "/music/test.flac",
            -18.5,     // integrated_lufs
            -0.5,      // true_peak_dbtp
            Some(6.2), // loudness_range
            DEFAULT_STREAMING_TARGET_LUFS,
        );

        // Insert
        db.upsert(&track).unwrap();

        // Retrieve
        let retrieved = db.get("/music/test.flac").unwrap().unwrap();
        assert_eq!(retrieved.integrated_lufs, -18.5);
        assert_eq!(retrieved.track_gain_db, 4.5); // -14 - (-18.5)

        // Check needs_scan. `/music/test.flac` does not exist, so there is no
        // evidence the stored measurement still describes it.
        assert!(db.needs_scan("/music/test.flac").unwrap());
        assert!(db.needs_scan("/music/other.flac").unwrap());
    }

    #[test]
    fn test_gain_calculation() {
        let track = TrackLoudness::new("/test.flac", -20.0, -1.0, None, -14.0);

        assert_eq!(track.track_gain_db, 6.0); // -14 - (-20)
        assert!((track.gain_linear(-14.0) - 1.995).abs() < 0.01);

        // Different target
        assert_eq!(track.gain_for_target(-23.0), -3.0);
    }

    #[test]
    fn track_gain_linear_reuses_same_target_and_invalidates_on_change() {
        let track = TrackLoudness::new("/test.flac", -20.0, -1.0, None, -14.0);

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
        let path_string = path.to_string_lossy().to_string();

        let track = TrackLoudness::new(&path_string, -18.0, -1.0, None, -14.0);
        db.upsert(&track).unwrap();
        assert!(db.get_fresh(&path_string).unwrap().is_some());

        std::fs::write(&path, b"changed file contents").unwrap();
        assert!(db.get_fresh(&path_string).unwrap().is_none());

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
        let path_string = path.to_string_lossy().to_string();

        db.upsert(&TrackLoudness::new(&path_string, -18.0, -1.0, None, -14.0))
            .unwrap();
        assert!(!db.needs_scan(&path_string).unwrap());

        std::fs::remove_file(&path).unwrap();

        assert!(db.needs_scan(&path_string).unwrap());
        assert!(db.get_fresh(&path_string).unwrap().is_none());
        // The record itself survives; only its freshness claim is withdrawn.
        assert!(db.get(&path_string).unwrap().is_some());
    }

    /// A record written by a *newer* scanner is no more trustworthy to this
    /// build than an older one, so exact version matching is required.
    #[test]
    fn a_newer_scanner_version_also_needs_a_rescan() {
        let db = LoudnessDatabase::in_memory().unwrap();
        let path = unique_temp_path("version");
        std::fs::write(&path, b"initial").unwrap();
        let path_string = path.to_string_lossy().to_string();

        let mut track = TrackLoudness::new(&path_string, -18.0, -1.0, None, -14.0);
        track.scan_version = CURRENT_SCAN_VERSION + 1;
        db.upsert(&track).unwrap();

        assert!(db.needs_scan(&path_string).unwrap());
        assert!(db.get_fresh(&path_string).unwrap().is_none());
        assert_eq!(db.get_outdated_tracks().unwrap(), vec![path_string]);

        let _ = std::fs::remove_file(path);
    }

    /// Scheme matching must agree with the decoder's source router, or an
    /// uppercase-scheme URL is treated as a local path and stat-ed.
    #[test]
    fn remote_identities_are_recognized_case_insensitively() {
        let db = LoudnessDatabase::in_memory().unwrap();

        for url in [
            "http://host/track.flac",
            "HTTPS://host/track.flac",
            "HtTp://host/track.flac",
        ] {
            assert!(is_remote_path(url), "{url} must classify as remote");
            db.upsert(&TrackLoudness::new(url, -18.0, -1.0, None, -14.0))
                .unwrap();
            // Remote freshness is version-only: no stat is attempted, so the
            // record stays usable instead of being rescanned every lookup.
            assert!(!db.needs_scan(url).unwrap(), "{url} must stay fresh");
            assert!(db.get_fresh(url).unwrap().is_some());
        }

        assert!(!is_remote_path("/music/http_archive/track.flac"));
    }
}
