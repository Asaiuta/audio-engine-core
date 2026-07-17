#[path = "../benches/support/mod.rs"]
mod support;

use support::{
    compare_case_medians, environment_json, generated_unix_ms, read_json, regression_gate_error,
    summarize_trials, validate_performance_baseline, write_json, BenchEnvironment, BenchMode,
    PerfArgs, PerformanceReportIdentity, RegressionComparison, TrialDistribution,
    DEFAULT_MAX_MEDIAN_REGRESSION_PCT, REPORT_SCHEMA_VERSION,
};

fn environment(revision: &str) -> BenchEnvironment {
    BenchEnvironment {
        revision: revision.to_string(),
        dirty: Some(false),
        rustc: "rustc 1.test".to_string(),
        target: "x86_64-test".to_string(),
        os: "test-os".to_string(),
        arch: "x86_64".to_string(),
        cpu: "test-cpu".to_string(),
        profile: "release".to_string(),
        features: vec!["http".to_string(), "loudness-db".to_string()],
    }
}

#[test]
fn trial_distribution_reports_median_and_nearest_rank_p95() {
    let report = summarize_trials(vec![9.0, 1.0, 5.0, 3.0, 7.0]).unwrap();
    assert_eq!(report.min, 1.0);
    assert_eq!(report.median, 5.0);
    assert_eq!(report.p95, 9.0);
    assert_eq!(report.max, 9.0);
    assert_eq!(report.samples, vec![9.0, 1.0, 5.0, 3.0, 7.0]);

    let even = summarize_trials(vec![4.0, 1.0, 3.0, 2.0]).unwrap();
    assert_eq!(even.median, 2.5);
}

#[test]
fn trial_distribution_rejects_empty_nonfinite_and_nonpositive_samples() {
    assert!(summarize_trials(Vec::new()).is_err());
    assert!(summarize_trials(vec![1.0, f64::NAN]).is_err());
    assert!(summarize_trials(vec![1.0, f64::INFINITY]).is_err());
    assert!(summarize_trials(vec![1.0, 0.0]).is_err());
}

#[test]
fn performance_args_parse_modes_paths_thresholds_and_errors() {
    let args = PerfArgs::parse(vec![
        "--quick".to_string(),
        "--enforce".to_string(),
        "--out=result.json".to_string(),
        "--baseline".to_string(),
        "base.json".to_string(),
        "--max-median-regression-pct=12.5".to_string(),
    ])
    .unwrap();
    assert_eq!(args.mode, BenchMode::Quick);
    assert!(args.enforce);
    assert_eq!(args.out.unwrap().to_string_lossy(), "result.json");
    assert_eq!(args.baseline.unwrap().to_string_lossy(), "base.json");
    assert_eq!(args.max_median_regression_pct, 12.5);

    let defaults = PerfArgs::parse(Vec::new()).unwrap();
    assert_eq!(defaults.mode, BenchMode::Full);
    assert_eq!(
        defaults.max_median_regression_pct,
        DEFAULT_MAX_MEDIAN_REGRESSION_PCT
    );

    let heavy = PerfArgs::parse(vec!["--heavy".to_string()]).unwrap();
    assert_eq!(heavy.mode, BenchMode::Heavy);

    assert!(PerfArgs::parse(vec!["--quick".into(), "--heavy".into()]).is_err());
    assert!(PerfArgs::parse(vec!["--out".into()]).is_err());
    assert!(PerfArgs::parse(vec!["--max-median-regression-pct=-1".into()]).is_err());
    assert!(PerfArgs::parse(vec!["--unknown".into()]).is_err());
}

#[test]
fn environment_compatibility_ignores_revision_and_dirty_but_checks_conditions() {
    let candidate = environment("candidate");
    let mut baseline = environment("baseline");
    baseline.dirty = Some(true);
    assert!(candidate.compatibility_issues(&baseline).is_empty());

    macro_rules! assert_mismatch {
        ($field:ident, $value:expr, $message:literal) => {{
            let mut mismatched = environment("baseline");
            mismatched.$field = $value;
            let issues = candidate.compatibility_issues(&mismatched);
            assert!(issues.iter().any(|issue| issue.contains($message)));
        }};
    }
    assert_mismatch!(rustc, "other-rustc".to_string(), "rustc differs");
    assert_mismatch!(target, "other-target".to_string(), "target differs");
    assert_mismatch!(os, "other-os".to_string(), "os differs");
    assert_mismatch!(arch, "other-arch".to_string(), "arch differs");
    assert_mismatch!(cpu, "other-cpu".to_string(), "cpu differs");
    assert_mismatch!(profile, "debug".to_string(), "profile differs");
    assert_mismatch!(features, vec!["http".to_string()], "features differ");

    let mut unknown = environment("unknown-environment");
    unknown.cpu = "unknown".to_string();
    let issues = unknown.compatibility_issues(&unknown);
    assert!(issues
        .iter()
        .any(|issue| issue.contains("cpu is unavailable")));
}

#[test]
fn performance_baseline_identity_rejects_every_incompatible_dimension() {
    let candidate_environment = environment("candidate");
    let baseline_environment = environment("baseline");
    let candidate_conditions = "conditions".to_string();
    let baseline_conditions = "conditions".to_string();
    let other_conditions = "other-conditions".to_string();

    macro_rules! identity {
        ($schema:expr, $probe:expr, $mode:expr, $environment:expr, $conditions:expr) => {
            PerformanceReportIdentity {
                schema_version: $schema,
                probe: $probe,
                mode: $mode,
                environment: $environment,
                conditions: $conditions,
            }
        };
    }

    assert!(validate_performance_baseline(
        "test",
        identity!(
            1,
            "probe",
            BenchMode::Quick,
            &candidate_environment,
            &candidate_conditions
        ),
        identity!(
            1,
            "probe",
            BenchMode::Quick,
            &baseline_environment,
            &baseline_conditions
        ),
    )
    .is_ok());

    let schema_error = validate_performance_baseline(
        "test",
        identity!(
            1,
            "probe",
            BenchMode::Quick,
            &candidate_environment,
            &candidate_conditions
        ),
        identity!(
            2,
            "probe",
            BenchMode::Quick,
            &baseline_environment,
            &baseline_conditions
        ),
    )
    .unwrap_err();
    assert!(schema_error.contains("schema differs"));

    let probe_error = validate_performance_baseline(
        "test",
        identity!(
            1,
            "candidate-probe",
            BenchMode::Quick,
            &candidate_environment,
            &candidate_conditions
        ),
        identity!(
            1,
            "baseline-probe",
            BenchMode::Quick,
            &baseline_environment,
            &baseline_conditions
        ),
    )
    .unwrap_err();
    assert!(probe_error.contains("probe differs"));

    let mode_error = validate_performance_baseline(
        "test",
        identity!(
            1,
            "probe",
            BenchMode::Quick,
            &candidate_environment,
            &candidate_conditions
        ),
        identity!(
            1,
            "probe",
            BenchMode::Full,
            &baseline_environment,
            &baseline_conditions
        ),
    )
    .unwrap_err();
    assert!(mode_error.contains("mode differs"));

    let conditions_error = validate_performance_baseline(
        "test",
        identity!(
            1,
            "probe",
            BenchMode::Quick,
            &candidate_environment,
            &candidate_conditions
        ),
        identity!(
            1,
            "probe",
            BenchMode::Quick,
            &baseline_environment,
            &other_conditions
        ),
    )
    .unwrap_err();
    assert!(conditions_error.contains("conditions differ"));

    let mut incompatible_environment = baseline_environment.clone();
    incompatible_environment.profile = "debug".to_string();
    let environment_error = validate_performance_baseline(
        "test",
        identity!(
            1,
            "probe",
            BenchMode::Quick,
            &candidate_environment,
            &candidate_conditions
        ),
        identity!(
            1,
            "probe",
            BenchMode::Quick,
            &incompatible_environment,
            &baseline_conditions
        ),
    )
    .unwrap_err();
    assert!(environment_error.contains("profile differs"));
}

#[test]
fn median_regression_boundary_and_case_set_validation_are_explicit() {
    let at_limit = compare_case_medians(
        [("case".to_string(), 110.0)],
        [("case".to_string(), 100.0)],
        10.0,
    )
    .unwrap();
    assert!(at_limit[0].passed);
    assert!(regression_gate_error(&at_limit, "median gate failed", "ns/sample").is_none());

    let over_limit = compare_case_medians(
        [("case".to_string(), 110.01)],
        [("case".to_string(), 100.0)],
        10.0,
    )
    .unwrap();
    assert!(!over_limit[0].passed);
    assert!(over_limit[0].regression_pct > 10.0);
    let diagnostic = regression_gate_error(&over_limit, "median gate failed", "ns/sample").unwrap();
    assert!(diagnostic.contains("median gate failed"));
    assert!(diagnostic.contains("case"));
    assert!(diagnostic.contains("baseline 100.000 ns/sample"));
    assert!(diagnostic.contains("candidate 110.010 ns/sample"));
    assert!(diagnostic.contains("regression 10.010%"));
    assert!(diagnostic.contains("threshold 10.000%"));

    let improvement = compare_case_medians(
        [("case".to_string(), 90.0)],
        [("case".to_string(), 100.0)],
        10.0,
    )
    .unwrap();
    assert!(improvement[0].passed);

    assert!(compare_case_medians(
        [("candidate".to_string(), 1.0)],
        [("baseline".to_string(), 1.0)],
        10.0,
    )
    .unwrap_err()
    .contains("case sets differ"));
    assert!(compare_case_medians(
        [
            ("duplicate".to_string(), 1.0),
            ("duplicate".to_string(), 2.0)
        ],
        [("duplicate".to_string(), 1.0)],
        10.0,
    )
    .unwrap_err()
    .contains("duplicate case key"));
}

#[test]
fn shared_report_types_round_trip_as_json() {
    let distribution = summarize_trials(vec![1.0, 2.0, 3.0]).unwrap();
    let encoded = serde_json::to_string(&distribution).unwrap();
    let decoded: TrialDistribution = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, distribution);

    let comparison = compare_case_medians(
        [("case".to_string(), 101.0)],
        [("case".to_string(), 100.0)],
        10.0,
    )
    .unwrap()
    .remove(0);
    let encoded = serde_json::to_string(&comparison).unwrap();
    let decoded: RegressionComparison = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, comparison);

    let env = environment("revision");
    let encoded = serde_json::to_string(&env).unwrap();
    let decoded: BenchEnvironment = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, env);
}

#[test]
fn environment_capture_and_json_file_helpers_produce_traceable_evidence() {
    assert_eq!(REPORT_SCHEMA_VERSION, 1);
    assert_eq!(BenchMode::Quick.as_str(), "quick");
    assert!(generated_unix_ms() > 0);

    let captured = BenchEnvironment::capture();
    assert!(!captured.revision.is_empty());
    assert!(!captured.rustc.is_empty());
    assert!(!captured.target.is_empty());
    assert!(environment_json(&captured).unwrap().starts_with('{'));

    let report = summarize_trials(vec![1.0, 2.0, 3.0]).unwrap();
    let path = std::env::temp_dir().join(format!(
        "audio-engine-core-benchmark-support-{}-{}.json",
        std::process::id(),
        generated_unix_ms()
    ));
    write_json(&path, &report, "test report").unwrap();
    let decoded: TrialDistribution = read_json(&path, "test report").unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(decoded, report);
}
