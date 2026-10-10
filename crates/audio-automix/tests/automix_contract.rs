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
/// The golden was captured on Windows/MSVC. The same source reproduces these
/// values only approximately on other toolchains and libm implementations: the
/// 2026-10 CI runs reported `bpm_confidence` differing by 1.3e-9 (macOS) and
/// `beat_grid_stability` by 4.5e-9 (Linux), which is visible platform noise —
/// the values agree to seven significant digits — yet both exceeded the
/// previous 1e-9 budget. 1e-6 relative leaves about 200x headroom over the
/// largest observed spread, while any semantic change moves these fields by
/// 1e-3 or more. Values that are exactly zero keep only the absolute floor, so
/// a stray non-zero spill is still caught.
const RELATIVE_TOLERANCE: f64 = 1e-6;
const ABSOLUTE_FLOOR: f64 = 1e-9;

fn compare(actual: &Value, expected: &Value, path: &str, exact: bool) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            if exact {
                assert_eq!(a.to_bits(), b.to_bits(), "{path}: {a} != {b}");
            } else {
                let tolerance = ABSOLUTE_FLOOR.max(RELATIVE_TOLERANCE * b.abs());
                assert!((a - b).abs() <= tolerance, "{path}: {a} != {b}");
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{path}");
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                compare(a, b, &format!("{path}[{i}]"), exact);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{path}"
            );
            for (key, value) in a {
                compare(value, &b[key], &format!("{path}.{key}"), exact);
            }
        }
        _ => assert_eq!(actual, expected, "{path}"),
    }
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
            compare(
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
    compare(&actual, &expected, "contract", false);
}
