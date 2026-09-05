use super::*;

fn example_case(samples: Vec<f64>) -> ComponentCase {
    component_case(ComponentCaseInput {
        case_key: "test".to_string(),
        component: "test",
        operation: "test",
        primary_unit: "ns/item",
        work_items_per_iteration: 4,
        iterations_per_trial: 3,
        expected_operations: samples.len() * 3,
        expected_work_items: samples.len() * 12,
        checksum: samples.len() as f64,
        samples,
        all_output_finite: true,
        output_nontrivial: true,
    })
    .unwrap()
}

#[test]
fn complete_warmups_do_not_contribute_samples_or_work() {
    let mut calls = Vec::new();
    let case = measure_component(3, |trials| {
        calls.push(trials);
        Ok(example_case(vec![calls.len() as f64; trials]))
    })
    .unwrap();
    assert!(calls.len() > WARMUP_MIN_PASSES);
    assert!(calls[..calls.len() - 1].iter().all(|trials| *trials == 1));
    assert_eq!(calls.last(), Some(&3));
    assert_eq!(case.distribution.samples, vec![calls.len() as f64; 3]);
    assert_eq!(case.work_validation.observed_operations, 9);
    assert_eq!(case.work_validation.observed_work_items, 36);
    assert_eq!(case.work_validation.checksum, 3.0);
}

#[test]
fn invalid_warmup_stops_before_measured_trials() {
    let mut calls = 0;
    let error = measure_component(3, |trials| {
        calls += 1;
        let mut case = example_case(vec![1.0; trials]);
        case.work_validation.valid = false;
        Ok(case)
    })
    .unwrap_err();
    assert!(error.contains("warmup failed for test"));
    assert_eq!(calls, 1);
}

#[test]
fn dispersion_retains_outliers_and_scales_the_actual_timing_window() {
    let raw = vec![2.0, 3.0, 4.0, 5.0, 100.0];
    let distribution = summarize_trials(raw.clone()).unwrap();
    let diagnostics = sampling_diagnostics(&distribution, 1_000_000);
    assert_eq!(distribution.samples, raw);
    assert_eq!(diagnostics.min_timed_ms, 2.0);
    assert_eq!(diagnostics.median_timed_ms, 4.0);
    assert_eq!(diagnostics.relative_mad_pct, 25.0);

    let even = summarize_trials(vec![2.0, 4.0, 6.0, 8.0]).unwrap();
    assert_eq!(sampling_diagnostics(&even, 1).relative_mad_pct, 40.0);
    let identical = summarize_trials(vec![5.0; 3]).unwrap();
    assert_eq!(sampling_diagnostics(&identical, 1).relative_mad_pct, 0.0);
}

fn example_report() -> ComponentReport {
    let fixture = ensure_deterministic_pcm_fixture().unwrap();
    ComponentReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: PROBE.to_string(),
        generated_unix_ms: 1,
        mode: BenchMode::Quick,
        environment: BenchEnvironment {
            revision: "test".to_string(),
            dirty: Some(false),
            rustc: "test".to_string(),
            target: "test".to_string(),
            os: "test".to_string(),
            arch: "test".to_string(),
            cpu: "test".to_string(),
            profile: "release".to_string(),
            features: vec!["test".to_string()],
        },
        conditions: component_conditions(workload(BenchMode::Quick), &fixture, None),
        cases: vec![example_case(vec![4.0; 3])],
        baseline: None,
        comparisons: Vec::new(),
    }
}

#[test]
fn legacy_protocol_and_different_scheduling_are_incompatible() {
    let candidate = example_report();
    let validate = |baseline: &ComponentReport| {
        validate_component_baseline(
            candidate.mode,
            &candidate.environment,
            &candidate.conditions,
            baseline,
        )
    };
    assert!(validate(&candidate).is_ok());

    let mut encoded = serde_json::to_value(&candidate).unwrap();
    let conditions = encoded["conditions"].as_object_mut().unwrap();
    conditions.remove("sampling");
    conditions.remove("pinned_scheduling");
    encoded["cases"][0]
        .as_object_mut()
        .unwrap()
        .remove("sampling_diagnostics");
    let legacy: ComponentReport = serde_json::from_value(encoded).unwrap();
    assert!(legacy.conditions.sampling.is_none());
    assert!(validate(&legacy).unwrap_err().contains("conditions differ"));

    let mut baseline = example_report();
    baseline.conditions.sampling.as_mut().unwrap().warmup_min_ms += 1;
    assert!(validate(&baseline)
        .unwrap_err()
        .contains("conditions differ"));
    baseline.conditions = candidate.conditions;
    baseline.conditions.pinned_scheduling = Some(PinnedSchedulingState {
        requested_core: 2,
        effective_group: 0,
        effective_affinity_mask: 4,
        effective_process_priority_class: 128,
        effective_thread_priority: 2,
    });
    let unpinned = example_report();
    assert!(validate_component_baseline(
        unpinned.mode,
        &unpinned.environment,
        &unpinned.conditions,
        &baseline
    )
    .unwrap_err()
    .contains("conditions differ"));

    let mut pinned = example_report();
    pinned.conditions.pinned_scheduling = baseline.conditions.pinned_scheduling.clone();
    assert!(validate_component_baseline(
        pinned.mode,
        &pinned.environment,
        &pinned.conditions,
        &baseline
    )
    .is_ok());
    baseline
        .conditions
        .pinned_scheduling
        .as_mut()
        .unwrap()
        .effective_group = 1;
    assert!(validate_component_baseline(
        pinned.mode,
        &pinned.environment,
        &pinned.conditions,
        &baseline
    )
    .unwrap_err()
    .contains("conditions differ"));
}

#[cfg(feature = "loudness-db")]
#[test]
fn database_repetitions_preserve_fresh_state_and_count_all_work() {
    let records = database_records(4);
    let databases = empty_databases(2).unwrap();
    databases[0].batch_upsert(&records).unwrap();
    assert_eq!(databases[1].stats().unwrap().total_tracks, 0);

    let batch = benchmark_database_batch(&records, 3, 2).unwrap();
    assert_eq!(batch.distribution.samples.len(), 2);
    assert_eq!(batch.work_validation.observed_operations, 6);
    assert_eq!(batch.work_validation.observed_work_items, 24);
    assert_eq!(batch.work_validation.checksum, 24.0);
    assert!(batch.work_validation.valid);

    for (iterations, expected_tracks) in [(2, 2), (8, 4)] {
        let upsert = benchmark_database_upsert(&records, iterations, 3, 2).unwrap();
        assert_eq!(upsert.work_validation.observed_operations, iterations * 6);
        assert_eq!(
            upsert.work_validation.checksum,
            (expected_tracks * 6) as f64
        );
        assert!(upsert.work_validation.valid);
    }
    let open = benchmark_database_open(3, 2).unwrap();
    assert_eq!(open.work_validation.observed_operations, 6);
    assert_eq!(open.distribution.samples.len(), 2);
    assert!(open.work_validation.valid);
}
