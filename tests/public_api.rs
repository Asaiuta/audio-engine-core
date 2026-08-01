//! Committed public-surface baseline.
//!
//! Gates 2-7 on the road to 1.0 each intentionally reshape the public API. This
//! test renders the surface of both supported feature matrices and compares it
//! against a snapshot committed next to this file, so an intentional change
//! shows up as a reviewable text diff in the commit that makes it, and an
//! unintentional one fails here.
//!
//! Refresh a snapshot after an intentional change:
//!
//! ```text
//! UPDATE_SNAPSHOTS=1 cargo test --test public_api
//! ```
//!
//! Rustdoc JSON is nightly-only (`-Z unstable-options --output-format json` has
//! no stable equivalent), so this test **skips** rather than fails when the
//! pinned toolchain is absent. CI installs it and runs the test for real. The
//! date is pinned because the rustdoc JSON format is unstable: a floating
//! `nightly` would let an unrelated format change break the build.

use std::path::PathBuf;
use std::process::Command;

/// The nightly this baseline was generated with.
///
/// Bump this together with a `UPDATE_SNAPSHOTS=1` refresh and the matching
/// `toolchain:` value in `.github/workflows/ci.yml`; a rustdoc JSON format
/// change can alter the rendering even when the API itself did not move.
const PINNED_NIGHTLY: &str = "nightly-2026-07-09";

/// One supported feature matrix and the snapshot that records its surface.
struct Matrix {
    /// Human-readable name used in skip/failure messages.
    label: &'static str,
    /// Snapshot path, relative to this crate's root.
    snapshot: &'static str,
    /// Passed through to `cargo rustdoc`.
    all_features: bool,
    no_default_features: bool,
    features: &'static [&'static str],
    /// Private scratch directory, so building under a second toolchain and a
    /// second feature set does not invalidate the ordinary build cache.
    target_dir: &'static str,
}

const ALL_FEATURES: Matrix = Matrix {
    label: "--all-features",
    snapshot: "tests/public-api-all-features.txt",
    all_features: true,
    no_default_features: false,
    features: &[],
    target_dir: "target/public-api/all-features",
};

const RUBATO_ONLY: Matrix = Matrix {
    label: "--no-default-features --features rubato",
    snapshot: "tests/public-api-rubato.txt",
    all_features: false,
    no_default_features: true,
    features: &["rubato"],
    target_dir: "target/public-api/rubato",
};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// What the toolchain probe could establish.
///
/// Deliberately three-valued. An earlier two-valued version treated every
/// probe failure as "not installed" and skipped, which turned a transient
/// `rustup` lock collision -- another `rustup toolchain install` running
/// concurrently is enough -- into a silently passing test. A gate that reports
/// green because it could not run is worse than no gate.
enum ToolchainProbe {
    Installed,
    /// `rustup` ran successfully and did not list the pinned toolchain.
    Absent,
    /// The probe established nothing. Never skip on this.
    Indeterminate(String),
}

/// Ask `rustup` which toolchains are installed.
///
/// `rustup toolchain list` only reads the toolchain directory, so it still
/// answers correctly while an unrelated install holds the download lock;
/// `rustup run <toolchain> rustc --version` does not, and reports a spurious
/// "missing manifest" in that window.
fn probe_pinned_nightly() -> ToolchainProbe {
    let output = match Command::new("rustup").args(["toolchain", "list"]).output() {
        Ok(output) => output,
        Err(error) => {
            return ToolchainProbe::Indeterminate(format!(
                "could not run `rustup toolchain list`: {error}"
            ));
        }
    };

    if !output.status.success() {
        return ToolchainProbe::Indeterminate(format!(
            "`rustup toolchain list` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    // Entries are `<name>[-<host triple>] [(active, default)]`.
    let installed = String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.split_whitespace().next().is_some_and(|name| {
            name == PINNED_NIGHTLY || name.starts_with(&format!("{PINNED_NIGHTLY}-"))
        })
    });

    if installed {
        ToolchainProbe::Installed
    } else {
        ToolchainProbe::Absent
    }
}

fn assert_matrix_matches_baseline(matrix: &Matrix) {
    match probe_pinned_nightly() {
        ToolchainProbe::Installed => {}
        ToolchainProbe::Absent => {
            eprintln!(
                "skipping the {} public API baseline: {PINNED_NIGHTLY} is not installed.\n\
                 Install it with `rustup toolchain install {PINNED_NIGHTLY} --profile minimal` \
                 to run this check locally; CI always runs it.",
                matrix.label
            );
            return;
        }
        ToolchainProbe::Indeterminate(reason) => panic!(
            "cannot tell whether {PINNED_NIGHTLY} is installed, so the {} public API baseline \
             was neither checked nor legitimately skipped: {reason}",
            matrix.label
        ),
    }

    let root = crate_root();
    let rustdoc_json = rustdoc_json::Builder::default()
        .toolchain(PINNED_NIGHTLY)
        .manifest_path(root.join("Cargo.toml"))
        .target_dir(root.join(matrix.target_dir))
        .all_features(matrix.all_features)
        .no_default_features(matrix.no_default_features)
        .features(matrix.features)
        .quiet(true)
        .build()
        .unwrap_or_else(|error| {
            panic!("failed to build rustdoc JSON for {}: {error}", matrix.label)
        });

    public_api::Builder::from_rustdoc_json(rustdoc_json)
        // Blanket impls come from core/std generics, are identical for every
        // type, and never carry a meaningful diff. Auto trait and auto-derived
        // impls are deliberately kept: losing `Send` on `PlaybackPipeline` or a
        // `Debug` derive on a public config is a real breaking change.
        .omit_blanket_impls(true)
        .build()
        .unwrap_or_else(|error| panic!("failed to render the {} API: {error}", matrix.label))
        .assert_eq_or_update(root.join(matrix.snapshot));
}

#[test]
fn all_features_surface_matches_the_committed_baseline() {
    assert_matrix_matches_baseline(&ALL_FEATURES);
}

#[test]
fn rubato_only_surface_matches_the_committed_baseline() {
    assert_matrix_matches_baseline(&RUBATO_ONLY);
}
