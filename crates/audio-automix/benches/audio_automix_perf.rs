//! Package-local, off-callback Head/Full cost measurements.
#[path = "support/automix_accuracy/mod.rs"]
pub mod accuracy;
pub mod support;

use audio_automix::{analyze_automix, AutomixAnalysisMode, AutomixAnalysisOptions};
use audio_engine_core::decoder::MediaLocation;
use serde_json::json;
use std::{fs, hint::black_box, path::PathBuf, time::Instant};

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut trials = 21;
    let mut out = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quick" => trials = 7,
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a path")?)),
            "--help" => {
                println!("audio_automix_perf [--quick] [--out PATH]\nLocal Head/Full cost report; no historical performance gate.");
                return Ok(());
            }
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    let mut fixture = accuracy::fixtures::Fixture::constant(48_000, 127.3);
    fixture.duration_sec = 12.0;
    let bytes = fixture.render().0;
    let path = std::env::temp_dir().join(format!("automix-perf-{}.wav", std::process::id()));
    fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let result = measure(&path, trials);
    let cleanup = fs::remove_file(&path).map_err(|e| e.to_string());
    let cases = result?;
    cleanup?;
    let report = json!({
        "schema_version": 1, "probe": "audio_automix_perf",
        "environment": support::BenchEnvironment::capture(),
        "generated_unix_ms": support::generated_unix_ms(),
        "conditions": {
            "fixture": fixture, "fixture_sha256": accuracy::corpus::sha256_bytes(&bytes),
            "trials": trials, "warmup_runs": 2, "window_seconds": 5.0,
            "timer_scope": "source open, bounded decode, analysis, and result allocation; excludes fixture generation",
            "scheduling": "serial, unpinned", "historical_baseline_compatible": false
        }, "cases": cases
    });
    if let Some(out) = out {
        support::write_json(&out, &report, "AutoMix performance report")?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report["cases"]).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn measure(path: &std::path::Path, trials: usize) -> Result<Vec<serde_json::Value>, String> {
    let mut cases = Vec::new();
    for mode in [AutomixAnalysisMode::Head, AutomixAnalysisMode::Full] {
        let mut samples_ms = Vec::with_capacity(trials);
        for run in 0..trials + 2 {
            let start = Instant::now();
            let analysis = analyze_automix(
                MediaLocation::Local(path.to_path_buf()),
                None,
                AutomixAnalysisOptions {
                    mode,
                    max_analyze_time_sec: 5.0,
                },
            )
            .map_err(|e| e.to_string())?;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;
            if analysis.version != 4
                || analysis.mode != mode
                || analysis.duration != 12.0
                || analysis.energy_profile.is_empty()
                || !analysis.mix_center_pos.is_finite()
            {
                return Err(format!("invalid analysis in {mode:?}"));
            }
            black_box(analysis);
            if run >= 2 {
                samples_ms.push(elapsed_ms);
            }
        }
        let mut sorted = samples_ms.clone();
        sorted.sort_by(f64::total_cmp);
        cases
            .push(json!({"mode": mode, "samples_ms": samples_ms, "median_ms": sorted[trials / 2]}));
    }
    Ok(cases)
}
