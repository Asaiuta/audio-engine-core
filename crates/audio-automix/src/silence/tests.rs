use super::*;

/// -20 dBFS: sound.
const SOUND: f32 = 0.1;
/// Digital silence.
const QUIET: f32 = 0.0;

/// -51 dBFS: between the silence and sound latch levels.
fn dead() -> f32 {
    10.0_f32.powf(-51.0 / 20.0)
}

fn level(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

fn bins(parts: &[(f32, usize)]) -> Vec<f32> {
    parts
        .iter()
        .flat_map(|&(value, count)| std::iter::repeat_n(value, count))
        .collect()
}

fn scan(envelope: &[f32], reaches_input_end: bool) -> (Option<usize>, Option<usize>) {
    let result = scan_window(envelope, None, reaches_input_end, 50.0, None).unwrap();
    (
        result.first_sound,
        result
            .sound_end
            .map(|end| end.complete_bins(envelope.len())),
    )
}

fn sound(first_sound: Option<usize>, sound_end: Option<usize>) -> (Option<usize>, Option<usize>) {
    (first_sound, sound_end)
}

#[test]
fn contract_constants_match_the_frozen_design() {
    assert_eq!(SOUND_LATCH_DB, -48.0);
    assert_eq!(SILENCE_LATCH_DB, -54.0);
    // 100 ms and 2 s at the 50 Hz envelope cadence.
    assert_eq!(SOUND_CONFIRM_SEC * 50.0, 5.0);
    assert_eq!(SILENCE_CONFIRM_SEC * 50.0, 100.0);
    assert_eq!(crate::ENVELOPE_RATE, 50.0);
    assert_eq!(crate::WINDOW_SIZE_MS, 20);
}

#[test]
fn sound_latches_on_the_fifth_consecutive_bin_and_reports_the_run_start() {
    for end in [false, true] {
        let four = bins(&[(QUIET, 10), (SOUND, 4), (QUIET, 10)]);
        assert_eq!(scan(&four, end), sound(None, None), "4 bins, end={end}");
    }
    let five = bins(&[(QUIET, 10), (SOUND, 5), (QUIET, 10)]);
    // Without end of input the 10 silent bins cannot end the latched sound.
    assert_eq!(scan(&five, false), sound(Some(10), Some(25)));
    // At end of input the trailing silent run is accepted at its start.
    assert_eq!(scan(&five, true), sound(Some(10), Some(15)));
}

#[test]
fn silence_latches_on_the_hundredth_consecutive_bin_and_reports_the_run_start() {
    for end in [false, true] {
        let ninety_nine = bins(&[(SOUND, 10), (QUIET, 99), (SOUND, 10)]);
        assert_eq!(scan(&ninety_nine, end), sound(Some(0), Some(119)));
        // Three bins cannot relatch sound, so the silence start stays the end.
        let hundred = bins(&[(SOUND, 10), (QUIET, 100), (SOUND, 3)]);
        assert_eq!(scan(&hundred, end), sound(Some(0), Some(10)));
    }
    // Sound that relatches after confirmed silence moves only the end.
    let relatched = bins(&[
        (QUIET, 3),
        (SOUND, 5),
        (QUIET, 100),
        (SOUND, 5),
        (QUIET, 100),
    ]);
    assert_eq!(scan(&relatched, false), sound(Some(3), Some(113)));
}

#[test]
fn dead_band_keeps_the_latched_state_and_breaks_unconfirmed_runs() {
    let dead = dead();
    assert_eq!(
        scan(&bins(&[(SOUND, 4), (dead, 1), (SOUND, 5)]), false),
        sound(Some(5), Some(10))
    );
    assert_eq!(
        scan(&bins(&[(SOUND, 4), (QUIET, 1), (SOUND, 4)]), true),
        sound(None, None)
    );
    // A dead-band bin restarts a pending silent run.
    let interrupted = bins(&[(SOUND, 10), (QUIET, 60), (dead, 1), (QUIET, 99), (SOUND, 1)]);
    assert_eq!(scan(&interrupted, false), sound(Some(0), Some(171)));
    let confirmed = bins(&[
        (SOUND, 10),
        (QUIET, 60),
        (dead, 1),
        (QUIET, 100),
        (SOUND, 1),
    ]);
    assert_eq!(scan(&confirmed, false), sound(Some(0), Some(71)));
    // Dead band holds latched sound even through end of input.
    assert_eq!(
        scan(&bins(&[(SOUND, 10), (dead, 500)]), true),
        sound(Some(0), Some(510))
    );
    // Dead band alone never latches sound.
    assert_eq!(scan(&bins(&[(dead, 500)]), true), sound(None, None));
}

#[test]
fn exact_latch_levels_belong_to_their_latch_class() {
    let sound_level = level(SOUND_LATCH_DB);
    let below_sound = f32::from_bits(sound_level.to_bits() - 1);
    assert_eq!(
        scan(&bins(&[(sound_level, 5)]), false),
        sound(Some(0), Some(5))
    );
    assert_eq!(scan(&bins(&[(below_sound, 5)]), false), sound(None, None));

    let silence_level = level(SILENCE_LATCH_DB);
    let above_silence = f32::from_bits(silence_level.to_bits() + 1);
    assert_eq!(
        scan(&bins(&[(SOUND, 5), (silence_level, 100)]), false),
        sound(Some(0), Some(5))
    );
    assert_eq!(
        scan(&bins(&[(SOUND, 5), (above_silence, 100)]), false),
        sound(Some(0), Some(105))
    );
}

#[test]
fn end_of_input_confirms_only_a_silent_run_that_reaches_the_last_bin() {
    let trailing = bins(&[(SOUND, 10), (QUIET, 20)]);
    assert_eq!(scan(&trailing, true), sound(Some(0), Some(10)));
    assert_eq!(scan(&trailing, false), sound(Some(0), Some(30)));
    // A final dead-band bin means the silent run does not reach the end.
    let not_trailing = bins(&[(SOUND, 10), (QUIET, 20), (dead(), 1)]);
    assert_eq!(scan(&not_trailing, true), sound(Some(0), Some(31)));
    // An unconfirmed final sound run is discarded like a click.
    assert_eq!(
        scan(&bins(&[(QUIET, 10), (SOUND, 4)]), true),
        sound(None, None)
    );
}

#[test]
fn short_windows_latch_only_complete_runs_or_the_end_of_input_rule() {
    for end in [false, true] {
        assert_eq!(scan(&[], end), sound(None, None));
        assert_eq!(scan(&bins(&[(SOUND, 4)]), end), sound(None, None));
        assert_eq!(scan(&bins(&[(SOUND, 5)]), end), sound(Some(0), Some(5)));
    }
    // A 55-bin window is shorter than one silence confirmation run.
    let short = bins(&[(SOUND, 5), (QUIET, 50)]);
    assert_eq!(scan(&short, false), sound(Some(0), Some(55)));
    assert_eq!(scan(&short, true), sound(Some(0), Some(5)));
}

#[test]
fn non_finite_bins_act_as_dead_band() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let interrupted = bins(&[(SOUND, 4), (value, 1), (SOUND, 5)]);
        assert_eq!(scan(&interrupted, false).0, Some(5));
        assert_eq!(scan(&bins(&[(value, 10)]), true), sound(None, None));
        assert_eq!(
            scan(&bins(&[(SOUND, 5), (value, 200)]), true),
            sound(Some(0), Some(205))
        );
    }
}

#[test]
fn scanning_observes_cancellation() {
    let canceled = || true;
    assert!(matches!(
        scan_window(&bins(&[(SOUND, 10)]), None, true, 50.0, Some(&canceled)),
        Err(AutomixError::Canceled)
    ));
}

fn segment(start_time: f64, envelope: Vec<f32>, reaches_input_end: bool) -> AnalysisSegment {
    AnalysisSegment {
        start_time,
        reaches_input_end,
        envelope,
        ..AnalysisSegment::default()
    }
}

#[test]
fn boundaries_use_absolute_origins_and_never_join_the_windows() {
    let head = segment(0.0, bins(&[(QUIET, 50), (SOUND, 200)]), false);
    let tail = segment(12.0, bins(&[(SOUND, 100), (QUIET, 50)]), true);
    let boundaries = sound_boundaries(&head, Some(&tail), 20.0, None).unwrap();
    assert_eq!(boundaries.fade_in, 1.0);
    assert_eq!(boundaries.fade_out, 14.0);
    assert_eq!(boundaries.outro_end_bin, Some(100));

    // Three tail bins cannot latch sound, and the head's latched sound is not
    // carried across the unanalyzed interval: fade-out keeps the timeline end.
    let unconfirmed = segment(12.0, bins(&[(SOUND, 3), (QUIET, 200)]), true);
    let boundaries = sound_boundaries(&head, Some(&unconfirmed), 20.0, None).unwrap();
    assert_eq!(boundaries.fade_out, 20.0);
    assert_eq!(boundaries.outro_end_bin, Some(203));
}

#[test]
fn head_only_boundaries_follow_the_head_input_end() {
    let envelope = bins(&[(QUIET, 25), (SOUND, 100), (QUIET, 50)]);
    let capped = segment(0.0, envelope.clone(), false);
    let at_end = segment(0.0, envelope, true);
    let capped = sound_boundaries(&capped, None, 30.0, None).unwrap();
    let at_end = sound_boundaries(&at_end, None, 30.0, None).unwrap();
    assert_eq!((capped.fade_in, capped.fade_out), (0.5, 3.5));
    assert_eq!((at_end.fade_in, at_end.fade_out), (0.5, 2.5));
    assert_eq!(capped.outro_end_bin, None);

    let silent = segment(0.0, bins(&[(QUIET, 500)]), true);
    let silent = sound_boundaries(&silent, None, 10.0, None).unwrap();
    assert_eq!((silent.fade_in, silent.fade_out), (0.0, 10.0));

    // The fade-out never exceeds the placement timeline.
    let long = segment(0.0, bins(&[(SOUND, 100)]), false);
    let long = sound_boundaries(&long, None, 1.5, None).unwrap();
    assert_eq!(long.fade_out, 1.5);
}

#[test]
fn sound_confirmation_uses_the_actual_pcm_bin_duration() {
    let rate = 11_025.0 / 220.0;
    for reaches_input_end in [false, true] {
        let five = scan_window(&[SOUND; 5], None, reaches_input_end, rate, None).unwrap();
        assert_eq!(five, WindowSound::default());
        let six = scan_window(&[SOUND; 6], None, reaches_input_end, rate, None).unwrap();
        assert_eq!(six.first_sound, Some(0));
        assert_eq!(six.sound_end, Some(SoundEnd::WindowEnd));
    }
}

#[test]
fn silence_confirmation_uses_the_actual_pcm_bin_duration() {
    let rate = 11_025.0 / 220.0;
    for reaches_input_end in [false, true] {
        // The final dead-band bin prevents EOF from confirming a short run.
        let short = bins(&[(SOUND, 6), (QUIET, 100), (dead(), 1)]);
        let short = scan_window(&short, None, reaches_input_end, rate, None).unwrap();
        assert_eq!(short.sound_end, Some(SoundEnd::WindowEnd));

        let complete = bins(&[(SOUND, 6), (QUIET, 101), (dead(), 1)]);
        let complete = scan_window(&complete, None, reaches_input_end, rate, None).unwrap();
        assert_eq!(complete.sound_end, Some(SoundEnd::Silence(6)));
    }
}

#[test]
fn partial_bins_cannot_complete_normal_confirmation_runs() {
    for reaches_input_end in [false, true] {
        let short_sound =
            scan_window(&[SOUND; 4], Some(SOUND), reaches_input_end, 50.0, None).unwrap();
        assert_eq!(short_sound, WindowSound::default());
    }
    let short_silence = bins(&[(SOUND, 5), (QUIET, 99)]);
    let capped = scan_window(&short_silence, Some(QUIET), false, 50.0, None).unwrap();
    assert_eq!(capped.first_sound, Some(0));
    assert_eq!(capped.sound_end, Some(SoundEnd::WindowEnd));
}

#[test]
fn non_silent_partial_bins_interrupt_pending_eof_silence() {
    let envelope = bins(&[(SOUND, 5), (QUIET, 20)]);
    for partial in [SOUND, dead(), f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let result = scan_window(&envelope, Some(partial), true, 50.0, None).unwrap();
        assert_eq!(result.first_sound, Some(0));
        assert_eq!(
            result.sound_end,
            Some(SoundEnd::WindowEnd),
            "partial={partial}"
        );
    }
    let quiet = scan_window(&envelope, Some(QUIET), true, 50.0, None).unwrap();
    assert_eq!(quiet.sound_end, Some(SoundEnd::Silence(5)));
}

#[test]
fn quiet_partial_alone_ends_latched_sound_only_at_input_end() {
    let eof = scan_window(&[SOUND; 5], Some(QUIET), true, 50.0, None).unwrap();
    assert_eq!(eof.sound_end, Some(SoundEnd::Silence(5)));
    let capped = scan_window(&[SOUND; 5], Some(QUIET), false, 50.0, None).unwrap();
    assert_eq!(capped.sound_end, Some(SoundEnd::WindowEnd));
}

#[test]
fn physical_boundaries_map_bin_indices_from_each_window_origin() {
    let head = AnalysisSegment {
        start_time: 1.5,
        sample_rate: 8_000,
        frames_analyzed: 10 * 160,
        envelope: bins(&[(QUIET, 5), (SOUND, 5)]),
        ..AnalysisSegment::default()
    };
    let tail = AnalysisSegment {
        start_time: 12.25,
        sample_rate: 11_025,
        frames_analyzed: 111 * 220 + 17,
        envelope: bins(&[(QUIET, 3), (SOUND, 6), (QUIET, 101), (dead(), 1)]),
        partial_envelope: Some(dead()),
        ..AnalysisSegment::default()
    };
    let boundaries = sound_boundaries(&head, Some(&tail), 30.0, None).unwrap();
    assert!((boundaries.fade_in - (1.5 + 5.0 * 160.0 / 8_000.0)).abs() < 1e-12);
    assert!((boundaries.fade_out - (12.25 + 9.0 * 220.0 / 11_025.0)).abs() < 1e-12);
    assert_eq!(boundaries.outro_end_bin, Some(9));

    let head_only = sound_boundaries(&tail, None, 30.0, None).unwrap();
    assert!((head_only.fade_in - (12.25 + 3.0 * 220.0 / 11_025.0)).abs() < 1e-12);
}

#[test]
fn latched_sound_uses_the_actual_selected_pcm_end_including_partial_frames() {
    for reaches_input_end in [false, true] {
        let window = AnalysisSegment {
            start_time: 12.25,
            sample_rate: 11_025,
            frames_analyzed: 6 * 220 + 17,
            reaches_input_end,
            envelope: vec![SOUND; 6],
            partial_envelope: Some(SOUND),
            ..AnalysisSegment::default()
        };
        let boundaries = sound_boundaries(&window, None, 30.0, None).unwrap();
        let expected = 12.25 + (6.0 * 220.0 + 17.0) / 11_025.0;
        assert!((boundaries.fade_out - expected).abs() < 1e-12);

        let quiet_partial = AnalysisSegment {
            partial_envelope: Some(QUIET),
            ..window
        };
        let boundaries = sound_boundaries(&quiet_partial, None, 30.0, None).unwrap();
        let expected = if reaches_input_end {
            12.25 + 6.0 * 220.0 / 11_025.0
        } else {
            expected
        };
        assert!((boundaries.fade_out - expected).abs() < 1e-12);
    }
}

#[test]
fn empty_windows_have_no_sound_and_still_observe_cancellation() {
    for partial in [None, Some(SOUND), Some(QUIET), Some(f32::NAN)] {
        let empty = scan_window(&[], partial, true, 50.0, None).unwrap();
        assert_eq!(empty, WindowSound::default());
    }
    assert!(matches!(
        scan_window(&[], None, true, 50.0, Some(&|| true)),
        Err(AutomixError::Canceled)
    ));
}

#[test]
fn long_scans_observe_cancellation_at_the_mid_scan_poll() {
    let polls = std::cell::Cell::new(0);
    let cancel = || {
        let count = polls.get() + 1;
        polls.set(count);
        count == 3
    };
    // Entry and bin zero pass; bin 4096 must poll again before completing.
    let envelope = vec![SOUND; 4_097];
    assert!(matches!(
        scan_window(&envelope, None, true, 50.0, Some(&cancel)),
        Err(AutomixError::Canceled)
    ));
    assert_eq!(polls.get(), 3);
}
