//! Whole-result contract first captured from d2816f2 before package extraction.
//! Numeric tolerance admits platform floating-point noise, never missing fields or null changes.
//! Structure S1/S2 intentionally re-baselines boundary/cut values and unavailable vocals; the
//! `source_revision` key still names the fixture/contract origin. Regenerate only for a
//! reviewed semantic change: `UPDATE_AUTOMIX_CONTRACT=1 cargo test -p audio-automix --test
//! automix_contract`, then review every changed value in the diff.

#[path = "../benches/support/automix_accuracy/corpus.rs"]
pub mod corpus;
#[path = "../benches/support/automix_accuracy/fixtures.rs"]
pub mod fixtures;
#[path = "../benches/support/automix_accuracy/metrics.rs"]
pub mod metrics;

use audio_automix::{
    analyze_automix, analyze_automix_with_descriptors, AutomixAnalysisMode, AutomixAnalysisOptions,
};
use audio_engine_core::decoder::MediaLocation;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

/// Relative tolerance for values that accumulate floating-point error.
///
/// The golden was captured on Windows/MSVC and the same source reproduces these
/// values only approximately elsewhere. Measured cross-platform spreads for
/// this exact source:
///
/// * Linux: `beat_grid_stability` off by 4.5e-9 absolute, 4.9e-9 relative.
/// * macOS: `bpm_confidence` off by 1.3e-9 absolute at one case and 2.2e-7
///   absolute (1.3e-6 relative) at another. Two orders of magnitude between
///   two cases of the same field is near-tie amplification in a softmax-style
///   aggregate, not uniform rounding noise, so a bound extrapolated from one
///   observation does not cover the others.
///
/// The budget is therefore being calibrated against a full divergence profile
/// rather than a single first failure: `compare` collects every field instead
/// of asserting on the first one it visits. Values the golden records as
/// exactly zero keep only the absolute floor, so a stray non-zero spill is
/// still caught.
const RELATIVE_TOLERANCE: f64 = 1e-6;
const ABSOLUTE_FLOOR: f64 = 1e-9;

/// The divergence profile of one comparison.
///
/// The first field that fails says nothing about the rest, so a run that
/// reports the whole profile replaces a chain of runs that each reveal one
/// more field.
#[derive(Default)]
struct Divergence {
    /// Fields that exceeded the budget: `(relative deviation, description)`.
    exceeded: Vec<(f64, String)>,
    /// The largest deviation among the fields that stayed inside the budget, so
    /// a report also shows how much headroom the rest actually had.
    closest: Option<(f64, String)>,
}

impl Divergence {
    fn exceeded(&mut self, relative: f64, description: String) {
        self.exceeded.push((relative, description));
    }

    fn within(&mut self, relative: f64, description: String) {
        if !relative.is_finite() {
            return;
        }
        if self
            .closest
            .as_ref()
            .is_none_or(|(best, _)| relative > *best)
        {
            self.closest = Some((relative, description));
        }
    }

    fn is_clean(&self) -> bool {
        self.exceeded.is_empty()
    }

    fn report(mut self) -> String {
        const SHOWN: usize = 25;
        let mut message = format!(
            "{} field(s) exceeded the golden budget ({:e} relative, {:e} absolute), \
             worst first:\n",
            self.exceeded.len(),
            RELATIVE_TOLERANCE,
            ABSOLUTE_FLOOR
        );
        self.exceeded
            .sort_by(|left, right| right.0.total_cmp(&left.0));
        for (relative, description) in self.exceeded.iter().take(SHOWN) {
            message.push_str(&format!("  {relative:.3e}  {description}\n"));
        }
        if self.exceeded.len() > SHOWN {
            message.push_str(&format!("  ... {} more\n", self.exceeded.len() - SHOWN));
        }
        if let Some((relative, description)) = &self.closest {
            message.push_str(&format!(
                "closest field inside the budget: {relative:.3e}  {description}\n"
            ));
        }
        message
    }
}

/// Numeric comparison. `exact` is for the descriptor opt-in parity check, which
/// compares two runs of this same binary and so must agree bit for bit.
fn compare(actual: &Value, expected: &Value, path: &str, exact: bool, divergence: &mut Divergence) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            if exact {
                if a.to_bits() != b.to_bits() {
                    divergence.exceeded(f64::INFINITY, format!("{path}: {a} != {b}"));
                }
            } else {
                let tolerance = ABSOLUTE_FLOOR.max(RELATIVE_TOLERANCE * b.abs());
                let delta = (a - b).abs();
                // A field the golden pins at zero has no meaningful relative
                // deviation. Report it as unbounded: `within` drops non-finite
                // values, so an exactly-zero pair never becomes the "closest".
                let relative = if b == 0.0 {
                    f64::INFINITY
                } else {
                    delta / b.abs()
                };
                let description =
                    format!("{path}: {a} != {b} (delta {delta:.3e}, tolerance {tolerance:.3e})");
                if delta > tolerance {
                    divergence.exceeded(relative, description);
                } else {
                    divergence.within(relative, description);
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                divergence.exceeded(
                    f64::INFINITY,
                    format!("{path}: {} elements != {}", a.len(), b.len()),
                );
            }
            // Compare the shared prefix even on a length mismatch, so one slip
            // does not hide every element behind it.
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                compare(a, b, &format!("{path}[{i}]"), exact, divergence);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            if a.keys().collect::<Vec<_>>() != b.keys().collect::<Vec<_>>() {
                divergence.exceeded(f64::INFINITY, format!("{path}: keys changed"));
            }
            for (key, value) in a {
                if let Some(expected) = b.get(key) {
                    compare(value, expected, &format!("{path}.{key}"), exact, divergence);
                }
            }
        }
        _ => {
            if actual != expected {
                divergence.exceeded(f64::INFINITY, format!("{path}: {actual} != {expected}"));
            }
        }
    }
}

/// Compare and, if anything diverged, fail with the whole profile.
fn assert_contract(actual: &Value, expected: &Value, path: &str, exact: bool) {
    let mut divergence = Divergence::default();
    compare(actual, expected, path, exact, &mut divergence);
    assert!(divergence.is_clean(), "{}", divergence.report());
}

#[test]
fn complete_results_match_the_pre_extraction_contract() {
    let temp = std::env::temp_dir().join(format!("automix-contract-{}.wav", std::process::id()));
    let mut cases = Vec::new();
    for (duration, channels, phase, pattern) in [
        (4.0, 1, 0.0, fixtures::Pattern::Straight),
        (10.0, 2, 0.217, fixtures::Pattern::Straight),
        (13.0, 2, 0.005, fixtures::Pattern::Subdivision),
        (13.0, 1, 0.217, fixtures::Pattern::Silence),
        (13.0, 2, 0.217, fixtures::Pattern::Noise),
        (13.0, 2, 0.217, fixtures::Pattern::Ramp),
    ] {
        let mut fixture = fixtures::Fixture::constant(22_050, 127.3);
        fixture.duration_sec = duration;
        fixture.channels = channels;
        fixture.phase_sec = phase;
        fixture.pattern = pattern;
        fs::write(&temp, fixture.render().0).unwrap();
        for mode in [AutomixAnalysisMode::Head, AutomixAnalysisMode::Full] {
            let output = analyze_automix(
                MediaLocation::Local(temp.clone()),
                None,
                AutomixAnalysisOptions {
                    mode,
                    max_analyze_time_sec: 5.0,
                },
            )
            .unwrap();
            let companion = analyze_automix_with_descriptors(
                MediaLocation::Local(temp.clone()),
                None,
                AutomixAnalysisOptions {
                    mode,
                    max_analyze_time_sec: 5.0,
                },
                None,
            )
            .unwrap();
            assert_contract(
                &serde_json::to_value(&companion.analysis).unwrap(),
                &serde_json::to_value(&output).unwrap(),
                "descriptor opt-in parity",
                true,
            );
            assert_eq!(companion.descriptors.sample_rate_hz, 22_050);
            assert_eq!(companion.descriptors.source_channels, usize::from(channels));
            for segment in &companion.descriptors.segments {
                let count = segment.end_frame - segment.start_frame;
                assert_eq!(segment.mono_signal.sample_count, count);
                let frames = if count < 4096 {
                    0
                } else {
                    1 + (count - 4096) / 1024
                };
                assert_eq!(segment.spectral.frame_count, frames);
                assert_eq!(
                    segment.spectral.last_end_frame,
                    (frames > 0).then(|| segment.start_frame + 4096 + (frames - 1) * 1024)
                );
            }
            cases.push(json!({"fixture": fixture, "mode": mode, "output": output}));
        }
    }
    fs::remove_file(temp).unwrap();
    let actual =
        json!({"source_revision": "d2816f271d03f2464f01e5a308360f3d02b1ead2", "cases": cases});
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/automix-contract.json");
    if std::env::var_os("UPDATE_AUTOMIX_CONTRACT").is_some() {
        let mut text = serde_json::to_string_pretty(&actual).unwrap();
        text.push('\n');
        fs::write(&golden, text).unwrap();
        return;
    }
    let expected: Value = serde_json::from_slice(&fs::read(golden).unwrap()).unwrap();
    assert_contract(&actual, &expected, "contract", false);
}
