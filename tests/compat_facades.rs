//! Compile-level facade identity checks for compatibility re-exports that the
//! rustdoc-JSON public-API renderer cannot see through.
//!
//! `public-api` attributes items to their defining module path, so a facade
//! module whose body is only `pub use` renders as nothing and disappears from
//! the snapshots even though the historical public path still resolves for
//! downstream code. These tests pin exactly that: every compatibility path
//! below must keep resolving to the *same item* as its canonical owner. The
//! ledger rows for these facades live in
//! `.trellis/spec/backend/analysis-compatibility.md`.

#[test]
fn diagnostics_facade_resolves_to_canonical_decode_budget_items() {
    use audio_engine_core::decode_budget::{
        decode_memory_budget as canonical_fn, DecodeMemoryBudget as CanonicalBudget,
        DEFAULT_DECODE_MAX_MEMORY_MB as CANONICAL_DEFAULT,
        ENV_DECODE_MAX_MEMORY_MB as CANONICAL_ENV, MAX_DECODE_MAX_MEMORY_MB as CANONICAL_MAX,
        MIN_DECODE_MAX_MEMORY_MB as CANONICAL_MIN,
    };

    use audio_engine_core::diagnostics::{
        decode_memory_budget, DecodeMemoryBudget, DEFAULT_DECODE_MAX_MEMORY_MB,
        ENV_DECODE_MAX_MEMORY_MB, MAX_DECODE_MAX_MEMORY_MB, MIN_DECODE_MAX_MEMORY_MB,
    };

    // Function-pointer coercions compile only when the paths name one type.
    let _: fn() -> CanonicalBudget = decode_memory_budget;
    let _: fn() -> CanonicalBudget = canonical_fn;

    let budget: CanonicalBudget = decode_memory_budget();
    let _ = DecodeMemoryBudget {
        limit_mb: budget.limit_mb,
        limit_bytes: budget.limit_bytes,
        source: budget.source,
    };

    assert_eq!(ENV_DECODE_MAX_MEMORY_MB, CANONICAL_ENV);
    assert_eq!(DEFAULT_DECODE_MAX_MEMORY_MB, CANONICAL_DEFAULT);
    assert_eq!(MIN_DECODE_MAX_MEMORY_MB, CANONICAL_MIN);
    assert_eq!(MAX_DECODE_MAX_MEMORY_MB, CANONICAL_MAX);
}

#[cfg(feature = "loudness-db")]
#[test]
fn processor_loudness_db_reexports_are_the_crate_root_types() {
    use audio_engine_core::loudness_db::{
        DatabaseStats as CanonicalStats, LoudnessDatabase as CanonicalDatabase,
        LoudnessDatabaseError as CanonicalError, LoudnessSourceIdentity as CanonicalIdentity,
        TrackLoudness as CanonicalTrack, CURRENT_SCAN_VERSION as CANONICAL_VERSION,
    };
    use audio_engine_core::processor::{
        DatabaseStats, LoudnessDatabase, LoudnessDatabaseError, LoudnessSourceIdentity,
        TrackLoudness, CURRENT_SCAN_VERSION,
    };

    // Coercions compile only when the compatibility paths name the same types
    // as the crate-root `loudness_db` module.
    let _: fn(&CanonicalIdentity) -> Result<bool, CanonicalError> =
        |identity| LoudnessDatabase::delete(&LoudnessDatabase::in_memory().unwrap(), identity);
    let _: fn() -> i32 = || CURRENT_SCAN_VERSION;

    let _ = (
        std::mem::size_of::<DatabaseStats>(),
        std::mem::size_of::<CanonicalStats>(),
        std::mem::size_of::<TrackLoudness>(),
        std::mem::size_of::<CanonicalTrack>(),
        std::mem::size_of::<LoudnessSourceIdentity>(),
        std::mem::size_of::<CanonicalIdentity>(),
    );
    fn assert_same_error_type(_: &LoudnessDatabaseError, _: &CanonicalError) {}
    fn assert_same_database_type(_: &LoudnessDatabase, _: &CanonicalDatabase) {}
    let _ = (assert_same_error_type, assert_same_database_type);
}
