//! Whole-result migration oracle captured from d2816f2 before package extraction.
//! Numeric tolerance admits platform floating-point noise, never missing fields or null changes.

#[path = "../benches/support/automix_accuracy/corpus.rs"]
pub mod corpus;
#[path = "../benches/support/automix_accuracy/fixtures.rs"]
pub mod fixtures;
#[path = "../benches/support/automix_accuracy/metrics.rs"]
pub mod metrics;

use audio_automix::{analyze_automix, AutomixAnalysisMode, AutomixAnalysisOptions};
use audio_engine_core::decoder::MediaLocation;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

fn compare(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            assert!(
                (a - b).abs() <= 1e-9 * b.abs().max(1.0),
                "{path}: {a} != {b}"
            );
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{path}");
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                compare(a, b, &format!("{path}[{i}]"));
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{path}"
            );
            for (key, value) in a {
                compare(value, &b[key], &format!("{path}.{key}"));
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
            cases.push(json!({"fixture": fixture, "mode": mode, "output": output}));
        }
    }
    fs::remove_file(temp).unwrap();
    let actual =
        json!({"source_revision": "d2816f271d03f2464f01e5a308360f3d02b1ead2", "cases": cases});
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/automix-contract.json");
    let expected: Value = serde_json::from_slice(&fs::read(golden).unwrap()).unwrap();
    compare(&actual, &expected, "contract");
}
