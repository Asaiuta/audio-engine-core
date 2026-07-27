#[path = "../benches/support/mod.rs"]
mod support;

use support::allocation::AllocationScope;
use support::audio_fixture::{
    deterministic_pcm_wav_bytes, ensure_deterministic_pcm_fixture, fixture_path_display,
    FIXTURE_CHANNELS, FIXTURE_FRAMES, FIXTURE_SAMPLE_RATE_HZ,
};
use support::callback_fixture::{
    callback_case_key, synthetic_callback_buffer, validate_callback_work, CallbackChainFixture,
    CallbackScenario, CALLBACK_BUFFER_FRAMES, CALLBACK_CHANNELS,
};
use support::signals::resampler_test_buffer;
use support::{
    compare_case_medians, enforce_pinned_burst_limits, environment_json, generated_unix_ms,
    index_cases_by_key, parse_callback_tail_args, parse_pinned_probe_args, pin_current_thread,
    read_json, regression_gate_error, summarize_callback_samples, summarize_trials,
    validate_case_key_set, validate_performance_baseline, validate_pinned_core,
    validate_unique_case_keys, write_json_round_trip, BenchEnvironment, BenchMode,
    CallbackTailDistribution, PerfArgs, PerformanceReportIdentity, PinnedSchedulingState,
    RegressionComparison, TrialDistribution, DEFAULT_MAX_MEDIAN_REGRESSION_PCT,
    DEFAULT_MAX_P999_REGRESSION_PCT, DEFAULT_MAX_P99_REGRESSION_PCT, DEFAULT_PINNED_PROBE_CORE,
    REPORT_SCHEMA_VERSION,
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
fn deterministic_decoder_fixture_has_stable_pcm_contract() {
    let bytes = deterministic_pcm_wav_bytes();
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(
        bytes.len(),
        44 + FIXTURE_FRAMES as usize * FIXTURE_CHANNELS * 2
    );

    let first = ensure_deterministic_pcm_fixture().unwrap();
    let second = ensure_deterministic_pcm_fixture().unwrap();
    assert_eq!(first.path, second.path);
    assert_eq!(first.metadata, second.metadata);
    assert_eq!(first.metadata.sample_rate_hz, FIXTURE_SAMPLE_RATE_HZ);
    assert_eq!(first.metadata.channels, FIXTURE_CHANNELS);
    assert_eq!(first.metadata.frames, FIXTURE_FRAMES);
    assert_eq!(first.metadata.content_fnv1a64, "fab9a2c095db8642");
    assert_eq!(
        fixture_path_display(&first.path),
        "target/audio-benchmark-fixtures/deterministic_pcm16_stereo_48k_12s.wav"
    );
    assert_eq!(std::fs::read(first.path).unwrap(), bytes);
}

#[test]
fn standalone_case_key_validation_rejects_empty_and_duplicate_reports() {
    validate_unique_case_keys(["a".to_string(), "b".to_string()], "test").unwrap();
    assert!(validate_unique_case_keys(Vec::new(), "test")
        .unwrap_err()
        .contains("no cases"));
    assert!(
        validate_unique_case_keys(["a".to_string(), "a".to_string()], "test")
            .unwrap_err()
            .contains("duplicate case key 'a'")
    );

    validate_case_key_set(
        ["b".to_string(), "a".to_string()],
        ["a".to_string(), "b".to_string()],
        "test",
    )
    .unwrap();
    let mismatch = validate_case_key_set(
        ["a".to_string(), "c".to_string()],
        ["a".to_string(), "b".to_string()],
        "test",
    )
    .unwrap_err();
    assert!(mismatch.contains("missing [\"b\"]"));
    assert!(mismatch.contains("unexpected [\"c\"]"));
}

#[test]
fn shared_case_indexing_rejects_empty_and_duplicate_case_structs() {
    #[derive(Debug)]
    struct Case {
        key: String,
    }
    let cases = [Case { key: "a".into() }, Case { key: "b".into() }];
    let indexed = index_cases_by_key(&cases, |case| case.key.as_str(), "test").unwrap();
    assert_eq!(indexed.len(), 2);
    assert_eq!(indexed["a"].key, "a");

    let empty: [Case; 0] = [];
    assert!(index_cases_by_key(&empty, |case| case.key.as_str(), "test")
        .unwrap_err()
        .contains("no cases"));

    let duplicated = [Case { key: "a".into() }, Case { key: "a".into() }];
    assert!(
        index_cases_by_key(&duplicated, |case| case.key.as_str(), "test")
            .unwrap_err()
            .contains("duplicate case key 'a'")
    );
}

#[test]
fn shared_resampler_test_buffer_is_deterministic_and_bounded() {
    let stereo = resampler_test_buffer(256, 2, 48_000);
    assert_eq!(stereo.len(), 512);
    assert_eq!(stereo, resampler_test_buffer(256, 2, 48_000));
    assert!(stereo.iter().all(|sample| sample.abs() <= 0.95));
    assert!(stereo.iter().any(|sample| *sample != 0.0));

    let quad = resampler_test_buffer(64, 4, 44_100);
    assert_eq!(quad.len(), 256);
    // Channels beyond stereo are scaled copies of the left channel.
    assert!((quad[2] - quad[0] * 0.90).abs() < 1e-12);
}

#[test]
fn allocation_scope_reports_rust_allocator_activity_and_peak_bytes() {
    let scope = AllocationScope::start();
    let allocation = vec![0x5a_u8; 32 * 1024];
    std::hint::black_box(&allocation);
    let snapshot = scope.finish();

    assert!(snapshot.allocations >= 1);
    assert!(snapshot.peak_live_bytes >= allocation.len());
    drop(allocation);
}

#[test]
fn trial_distribution_rejects_empty_nonfinite_and_nonpositive_samples() {
    assert!(summarize_trials(Vec::new()).is_err());
    assert!(summarize_trials(vec![1.0, f64::NAN]).is_err());
    assert!(summarize_trials(vec![1.0, f64::INFINITY]).is_err());
    assert!(summarize_trials(vec![1.0, 0.0]).is_err());
}

#[test]
fn callback_tail_distribution_retains_raw_samples_and_reports_nearest_rank_tails() {
    let samples = (1..=1_000).rev().map(f64::from).collect::<Vec<_>>();
    let distribution = summarize_callback_samples(samples.clone()).unwrap();
    assert_eq!(distribution.samples, samples);
    assert_eq!(distribution.min, 1.0);
    assert_eq!(distribution.median, 500.5);
    assert_eq!(distribution.p95, 950.0);
    assert_eq!(distribution.p99, 990.0);
    assert_eq!(distribution.p99_9, 999.0);
    assert_eq!(distribution.max, 1_000.0);

    assert!(summarize_callback_samples(Vec::new()).is_err());
    assert!(summarize_callback_samples(vec![1.0, f64::NAN]).is_err());
    assert!(summarize_callback_samples(vec![1.0, 0.0]).is_err());
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
fn pinned_probe_args_are_removed_before_shared_argument_parsing() {
    let args = parse_pinned_probe_args(vec![
        "--quick".into(),
        "--pinned".into(),
        "--pin-core=5".into(),
        "--enforce".into(),
    ])
    .unwrap();
    assert!(args.enabled);
    assert_eq!(args.core, 5);
    assert_eq!(args.remaining, ["--quick", "--enforce"]);
    assert!(PerfArgs::parse(args.remaining).unwrap().enforce);

    let defaults = parse_pinned_probe_args(vec!["--pinned".into()]).unwrap();
    assert_eq!(defaults.core, DEFAULT_PINNED_PROBE_CORE);

    assert!(parse_pinned_probe_args(vec!["--pin-core".into()]).is_err());
    assert!(parse_pinned_probe_args(vec!["--pin-core".into(), "x".into()]).is_err());
    assert!(parse_pinned_probe_args(vec!["--pin-core=3".into()]).is_err());
}

#[test]
fn callback_tail_args_are_removed_before_shared_argument_parsing() {
    let args = parse_callback_tail_args(vec![
        "--quick".into(),
        "--max-p99-regression-pct=12.5".into(),
        "--max-p999-regression-pct".into(),
        "18".into(),
        "--enforce".into(),
    ])
    .unwrap();
    assert_eq!(args.max_p99_regression_pct, 12.5);
    assert_eq!(args.max_p999_regression_pct, 18.0);
    assert_eq!(args.remaining, ["--quick", "--enforce"]);
    assert!(PerfArgs::parse(args.remaining).unwrap().enforce);

    let defaults = parse_callback_tail_args(Vec::new()).unwrap();
    assert_eq!(
        defaults.max_p99_regression_pct,
        DEFAULT_MAX_P99_REGRESSION_PCT
    );
    assert_eq!(
        defaults.max_p999_regression_pct,
        DEFAULT_MAX_P999_REGRESSION_PCT
    );
    assert!(parse_callback_tail_args(vec!["--max-p99-regression-pct".into()]).is_err());
    assert!(parse_callback_tail_args(vec!["--max-p999-regression-pct=-1".into()]).is_err());
    assert!(parse_callback_tail_args(vec!["--max-p999-regression-pct=NaN".into()]).is_err());
}

#[test]
fn pinned_core_validation_rejects_affinity_shift_overflow() {
    let _pin_entrypoint: fn(usize) -> Result<PinnedSchedulingState, String> = pin_current_thread;
    assert!(validate_pinned_core(0).is_ok());
    assert!(validate_pinned_core(usize::BITS as usize - 1).is_ok());
    let error = validate_pinned_core(usize::BITS as usize).unwrap_err();
    assert!(error.contains("affinity mask width"));
}

#[test]
fn callback_fixture_preserves_canonical_scenarios_and_case_keys() {
    assert_eq!(CALLBACK_CHANNELS, 2);
    assert_eq!(CALLBACK_BUFFER_FRAMES, [64, 128, 256, 512]);
    assert_eq!(
        CallbackScenario::ALL.map(|scenario| scenario.name()),
        [
            "bypass_default",
            "active_dsp_no_convolver",
            "active_dsp_with_convolver"
        ]
    );
    assert!(CallbackScenario::ActiveDspWithConvolver
        .config_description()
        .contains("256-tap synthetic convolver"));
    assert_eq!(
        callback_case_key(CallbackScenario::BypassDefault, 512),
        "scenario=bypass_default;frames=512;config=bypass_defaults"
    );
    assert_eq!(
        callback_case_key(CallbackScenario::ActiveDspNoConvolver, 512),
        "scenario=active_dsp_no_convolver;frames=512;config=active_oversampled4x_no_convolver"
    );
    assert_eq!(
        callback_case_key(CallbackScenario::ActiveDspWithConvolver, 512),
        "scenario=active_dsp_with_convolver;frames=512;config=active_oversampled4x_ir256"
    );
}

#[test]
fn callback_fixture_executes_bypass_and_active_work() {
    let corpus = synthetic_callback_buffer(64);
    let mut fixture = CallbackChainFixture::build(CallbackScenario::BypassDefault).unwrap();
    let _chain = fixture.chain_mut();
    for scenario in CallbackScenario::ALL {
        let validation = validate_callback_work(scenario, 64, &corpus).unwrap();
        assert!(validation.valid, "scenario {scenario:?}");
        assert_eq!(
            validation.output_changed,
            scenario != CallbackScenario::BypassDefault
        );
        assert_eq!(validation.consumed_frames, 64);
        assert_eq!(validation.produced_frames, 64);
    }
}

#[test]
fn pinned_burst_limits_enforce_both_task_critical_thresholds() {
    assert!(enforce_pinned_burst_limits("case", 40.0, 50.0, 40.0, 50.0).is_ok());

    let p99_error = enforce_pinned_burst_limits("case", 40.001, 10.0, 40.0, 50.0).unwrap_err();
    assert!(p99_error.contains("p99 gate failed"));

    let max_error = enforce_pinned_burst_limits("case", 10.0, 50.001, 40.0, 50.0).unwrap_err();
    assert!(max_error.contains("max gate failed"));
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

    let callback_distribution = summarize_callback_samples(vec![1.0, 2.0, 3.0]).unwrap();
    let encoded = serde_json::to_string(&callback_distribution).unwrap();
    let decoded: CallbackTailDistribution = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, callback_distribution);

    let scheduling = PinnedSchedulingState {
        requested_core: 2,
        effective_group: 0,
        effective_affinity_mask: 4,
        effective_process_priority_class: 0x80,
        effective_thread_priority: 2,
    };
    let encoded = serde_json::to_string(&scheduling).unwrap();
    let decoded: PinnedSchedulingState = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, scheduling);

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
    // The compiled resampler backend must be recorded so cross-backend
    // baseline comparisons fail the feature compatibility check.
    assert!(captured.features.contains(&format!(
        "resampler-{}",
        audio_engine_core::processor::RESAMPLER_BACKEND_NAME
    )));
    assert!(environment_json(&captured).unwrap().starts_with('{'));

    let report = summarize_trials(vec![1.0, 2.0, 3.0]).unwrap();
    let path = std::env::temp_dir().join(format!(
        "audio-engine-core-benchmark-support-{}-{}.json",
        std::process::id(),
        generated_unix_ms()
    ));
    write_json_round_trip(&path, &report, "test report").unwrap();
    let decoded: TrialDistribution = read_json(&path, "test report").unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(decoded, report);
}
