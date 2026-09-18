//! Package-specific public API snapshots using the shared workspace test helper.
#[path = "../../../tests/support/public_api.rs"]
mod public_api;
use public_api::{assert_matrix_matches_baseline, Matrix};
use std::path::PathBuf;

const ALL_FEATURES: Matrix = Matrix {
    label: "--all-features",
    snapshot: "tests/public-api-all-features.txt",
    all_features: true,
    no_default_features: false,
    features: &[],
    target_dir: "../../target/public-api/automix-all-features",
};

const RUBATO_ONLY: Matrix = Matrix {
    label: "--no-default-features --features rubato",
    snapshot: "tests/public-api-rubato.txt",
    all_features: false,
    no_default_features: true,
    features: &["rubato"],
    target_dir: "../../target/public-api/automix-rubato",
};

// Default enables HTTP + Rubato; SQLite is never selected by this package.
const DEFAULT_FEATURES: Matrix = Matrix {
    label: "default features",
    snapshot: "tests/public-api-default.txt",
    all_features: false,
    no_default_features: false,
    features: &[],
    target_dir: "../../target/public-api/automix-default",
};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn all_features_surface_matches_the_committed_baseline() {
    assert_matrix_matches_baseline(&crate_root(), "audio-automix", &ALL_FEATURES);
}

#[test]
fn rubato_only_surface_matches_the_committed_baseline() {
    assert_matrix_matches_baseline(&crate_root(), "audio-automix", &RUBATO_ONLY);
}

#[test]
fn default_feature_surface_matches_the_committed_baseline() {
    assert_matrix_matches_baseline(&crate_root(), "audio-automix", &DEFAULT_FEATURES);
}
