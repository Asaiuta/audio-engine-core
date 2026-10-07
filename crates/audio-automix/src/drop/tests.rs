use super::*;
use crate::tempo::BeatGrid;
use std::cell::Cell;

const RATE: f64 = 50.0;

fn segment(parts: &[(usize, f32, f32)]) -> AnalysisSegment {
    let mut head = AnalysisSegment::default();
    for &(count, full, low) in parts {
        head.envelope.extend(std::iter::repeat_n(full, count));
        head.low_envelope.extend(std::iter::repeat_n(low, count));
    }
    head
}

fn reentry() -> AnalysisSegment {
    // All full-band RMS bins are equal. Bass falls for exactly four seconds,
    // after an observed two-second reference, then returns for two seconds.
    segment(&[(100, 0.1, 0.08), (200, 0.1, 0.01), (100, 0.1, 0.08)])
}

fn detect(head: &AnalysisSegment) -> Option<f64> {
    detect_drop(head, RATE, TempoEstimate::default(), None).unwrap()
}

fn grid(confidence: f64, stability: f64) -> TempoEstimate {
    TempoEstimate {
        grid: Some(BeatGrid {
            period_sec: 0.5,
            first_beat_sec: 0.217,
            stability,
        }),
        confidence: Some(confidence),
    }
}

#[test]
fn equal_full_band_rms_bass_reentry_is_detected_at_the_transition() {
    let head = reentry();
    assert_eq!(detect(&head), Some(6.0));
    assert!(head.envelope.iter().all(|&value| value == 0.1));
}

#[test]
fn full_band_loudness_jump_without_low_rise_is_not_a_drop() {
    let head = segment(&[(100, 0.1, 0.08), (200, 0.1, 0.01), (100, 0.4, 0.01)]);
    assert_eq!(detect(&head), None);
}

#[test]
fn broadband_gain_dip_and_recovery_do_not_establish_bass_reentry() {
    // Both absolute envelopes dip and recover fourfold, but spectral share
    // is unchanged. A ratio-only low-band detector would wrongly accept this.
    let head = segment(&[(100, 0.4, 0.1), (200, 0.1, 0.025), (100, 0.4, 0.1)]);
    assert_eq!(detect(&head), None);
}

#[test]
fn bass_increase_without_an_observed_prior_breakdown_is_not_a_drop() {
    let head = segment(&[(300, 0.1, 0.01), (100, 0.1, 0.08)]);
    assert_eq!(detect(&head), None);
    // A track starting inside a four-second bass-light intro has no earlier
    // reference at all. No missing history is fabricated.
    let intro = segment(&[(200, 0.1, 0.01), (100, 0.1, 0.08)]);
    assert_eq!(detect(&intro), None);
}

#[test]
fn bass_reentry_without_full_band_retention_is_rejected() {
    let head = segment(&[(100, 0.1, 0.08), (200, 0.1, 0.01), (100, 0.05, 0.04)]);
    assert_eq!(detect(&head), None);
}

#[test]
fn a_break_in_the_last_second_cannot_pass_as_sustained_bass_dip() {
    let mut head = reentry();
    // A single bin at the oldest position still inside the one-second dip
    // interrupts it; moving it one bin earlier places it outside that window.
    head.low_envelope[250] = 0.05;
    assert_eq!(detect(&head), None);
    head.low_envelope[250] = 0.01;
    head.low_envelope[249] = 0.05;
    assert_eq!(detect(&head), Some(6.0));
}

#[test]
fn all_post_bins_are_required_including_the_last_complete_bin() {
    let mut head = reentry();
    head.envelope.pop();
    head.low_envelope.pop();
    assert_eq!(detect(&head), None);
    head.envelope.push(0.1);
    head.low_envelope.push(0.08);
    assert_eq!(detect(&head), Some(6.0));
    assert_eq!(detect(&segment(&[(250, 0.1, 0.08)])), None);
}

#[test]
fn fractional_cadence_rounds_each_support_duration_up() {
    let rate = 50.1;
    let mut head = segment(&[(101, 0.1, 0.08), (201, 0.1, 0.01), (101, 0.1, 0.08)]);
    let expected = 302.0 / rate;
    assert_eq!(
        detect_drop(&head, rate, TempoEstimate::default(), None).unwrap(),
        Some(expected)
    );
    head.envelope.pop();
    head.low_envelope.pop();
    assert_eq!(
        detect_drop(&head, rate, TempoEstimate::default(), None).unwrap(),
        None
    );
}

#[test]
fn physical_pcm_clock_defines_positions_and_pending_cannot_complete_post() {
    // 12,345 Hz uses 246-frame bins, not exactly 20 ms. Each temporal window
    // must independently round up to enough complete bins: 101/201/101.
    let mut head = segment(&[(101, 0.1, 0.08), (201, 0.1, 0.01), (101, 0.1, 0.08)]);
    head.sample_rate = 12_345;
    head.start_time = 17.0;
    head.frames_analyzed = 403 * 246;
    let rate = head.envelope_rate();
    let actual = detect_drop(&head, rate, TempoEstimate::default(), None)
        .unwrap()
        .unwrap();
    let expected = 17.0 + 302.0 * 246.0 / 12_345.0;
    assert!((actual - expected).abs() < 1e-12);
    assert!((actual - (17.0 + 302.0 / 50.0)).abs() > 0.01);
    head.envelope.pop();
    head.low_envelope.pop();
    head.partial_envelope = Some(0.1);
    head.frames_analyzed = 402 * 246 + 245;
    assert_eq!(
        detect_drop(&head, rate, TempoEstimate::default(), None).unwrap(),
        None
    );
}

#[test]
fn source_origin_is_preserved_in_unsnapped_and_grid_positions() {
    let mut head = reentry();
    head.start_time = 37.125;
    assert_eq!(detect(&head), Some(43.125));
    let mut tempo = grid(0.9, 0.95);
    tempo.grid.as_mut().unwrap().first_beat_sec += head.start_time;
    let actual = detect_drop(&head, RATE, tempo, None).unwrap().unwrap();
    assert!((actual - 43.342).abs() < 1e-12);
}

#[test]
fn only_usable_grids_snap_to_individual_beats_never_four_beat_bars() {
    let head = segment(&[(125, 0.1, 0.08), (200, 0.1, 0.01), (100, 0.1, 0.08)]);
    assert_eq!(detect(&head), Some(6.5));
    let snapped = detect_drop(&head, RATE, grid(0.35, 0.80), None)
        .unwrap()
        .unwrap();
    assert!((snapped - 6.717).abs() < 1e-12);
    // The nearest four-beat multiple from the same phase would be 6.217 s.
    assert!((snapped - 6.217).abs() > 0.49);
    for tempo in [grid(0.349, 1.0), grid(1.0, 0.799), TempoEstimate::default()] {
        assert_eq!(detect_drop(&head, RATE, tempo, None).unwrap(), Some(6.5));
    }
}

#[test]
fn snapping_outside_observed_head_or_invalid_grid_keeps_raw_evidence() {
    let head = reentry();
    let mut outside = grid(1.0, 1.0);
    let outside_grid = outside.grid.as_mut().unwrap();
    outside_grid.period_sec = 100.0;
    outside_grid.first_beat_sec = 9.0;
    assert_eq!(detect_drop(&head, RATE, outside, None).unwrap(), Some(6.0));
    for period in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut invalid = grid(1.0, 1.0);
        invalid.grid.as_mut().unwrap().period_sec = period;
        assert_eq!(detect_drop(&head, RATE, invalid, None).unwrap(), Some(6.0));
    }
}

#[test]
fn exact_ties_keep_the_earliest_corroborated_transition() {
    let head = segment(&[
        (100, 0.1, 0.08),
        (200, 0.1, 0.01),
        (200, 0.1, 0.08),
        (200, 0.1, 0.01),
        (100, 0.1, 0.08),
    ]);
    assert_eq!(detect(&head), Some(6.0));
}

#[test]
fn malformed_envelopes_geometry_and_subfloor_signals_abstain() {
    for value in [f32::NAN, f32::INFINITY, -0.01] {
        let mut head = reentry();
        head.envelope[150] = value;
        assert_eq!(detect(&head), None);
        head = reentry();
        head.low_envelope[150] = value;
        assert_eq!(detect(&head), None);
    }
    let mut mismatched = reentry();
    mismatched.low_envelope.pop();
    assert_eq!(detect(&mismatched), None);
    for rate in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
        assert_eq!(
            detect_drop(&reentry(), rate, TempoEstimate::default(), None).unwrap(),
            None
        );
    }
    for start in [-1.0, f64::NAN, f64::INFINITY] {
        let mut head = reentry();
        head.start_time = start;
        assert_eq!(detect(&head), None);
    }
    let mut quiet = reentry();
    for value in quiet.envelope.iter_mut().chain(&mut quiet.low_envelope) {
        *value *= 0.001;
    }
    assert_eq!(detect(&quiet), None);
}

#[test]
fn cancellation_is_typed_at_entry_and_during_both_linear_scans() {
    assert!(matches!(
        detect_drop(&reentry(), RATE, TempoEstimate::default(), Some(&|| true)),
        Err(AutomixError::Canceled)
    ));
    let head = segment(&[(10_000, 0.1, 0.08)]);
    // Entry + five prefix polls precede the first candidate-scan poll. These
    // cases distinguish cancellation inside each scan from entry-only checks.
    for stop_at in [4, 7] {
        let polls = Cell::new(0);
        let canceled = || {
            polls.set(polls.get() + 1);
            polls.get() >= stop_at
        };
        assert!(matches!(
            detect_drop(&head, RATE, TempoEstimate::default(), Some(&canceled)),
            Err(AutomixError::Canceled)
        ));
        assert_eq!(polls.get(), stop_at);
    }
}
