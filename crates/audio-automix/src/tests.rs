use super::*;
use crate::decode::{
    decode_segment, is_plausible_duration, select_packet_frames, AnalysisWindowPlan, FrameWindow,
};
use crate::features::{spectral_hop_size, AnalysisSegment, SegmentAnalyzer};
use crate::placement::{
    build_energy_profile, calculate_smart_cut_in, calculate_smart_cut_out, finalize_analysis,
    reported_first_beat, snap_to_beat,
};
use crate::tempo::{self, BeatGrid, TempoEstimate};
use audio_engine_core::analysis::LoudnessMeter;
use audio_engine_core::decoder::{DecodeCancelToken, MediaLocation, StreamingDecoder};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

mod structure_drop;

static TEMP_AUDIO_COUNTER: AtomicU32 = AtomicU32::new(0);

struct TempAudio {
    path: PathBuf,
}

impl TempAudio {
    fn wav(bytes: &[u8]) -> Self {
        let id = TEMP_AUDIO_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "aec_automix_test_{}_{}.wav",
            std::process::id(),
            id
        ));
        let mut file = std::fs::File::create(&path).expect("create AutoMix fixture");
        file.write_all(bytes).expect("write AutoMix fixture");
        file.flush().expect("flush AutoMix fixture");
        Self { path }
    }

    fn path_string(&self) -> String {
        self.path
            .to_str()
            .expect("UTF-8 AutoMix fixture path")
            .to_owned()
    }
}

impl Drop for TempAudio {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn synth_wav<F: Fn(u64) -> f64>(sample_rate: u32, frames: u64, sample: F) -> Vec<u8> {
    let channels = 1_u16;
    let bits_per_sample = 16_u16;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = frames as usize * usize::from(block_align);
    let mut bytes = Vec::with_capacity(44 + data_len);

    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for frame in 0..frames {
        let value = sample(frame).clamp(-1.0, 1.0);
        bytes.extend_from_slice(&((value * i16::MAX as f64).round() as i16).to_le_bytes());
    }

    bytes
}

fn analyze_tail_fixture(duration_secs: u64) -> AutomixAnalysis {
    let sample_rate = 8_000_u32;
    let frames = duration_secs * u64::from(sample_rate);
    let active_end = frames - u64::from(sample_rate) * 2 / 5;
    let wav = synth_wav(sample_rate, frames, |frame| {
        if frame < active_end {
            0.25
        } else {
            0.0
        }
    });
    let fixture = TempAudio::wav(&wav);

    analyze_automix(
        MediaLocation::local(fixture.path_string()),
        None,
        AutomixAnalysisOptions {
            mode: AutomixAnalysisMode::Full,
            max_analyze_time_sec: MIN_ANALYZE_TIME_SEC,
        },
    )
    .expect("analyze tail fixture")
}

fn pulse_train(rate: f64, bpm: f64, duration_seconds: f64) -> Vec<f32> {
    let len = (rate * duration_seconds).ceil() as usize;
    let period = rate * 60.0 / bpm;
    let mut values = vec![0.0; len];
    let mut position = 0.0_f64;
    while (position.round() as usize) < values.len() {
        values[position.round() as usize] = 1.0;
        position += period;
    }
    values
}

fn empty_analysis_with_flux(sample_rate: u32, flux: Vec<f32>) -> AutomixAnalysis {
    let meter = LoudnessMeter::new(2, sample_rate).unwrap();
    let head = AnalysisSegment {
        spectral_flux: flux,
        ..AnalysisSegment::default()
    };
    finalize_analysis(
        &AutomixAnalysisOptions {
            mode: AutomixAnalysisMode::Head,
            ..AutomixAnalysisOptions::default()
        },
        12.0,
        sample_rate,
        &meter,
        &head,
        &AnalysisSegment::default(),
        None,
    )
    .unwrap()
}

#[test]
fn window_plan_keeps_head_and_tail_disjoint_at_boundaries() {
    let window = 60;
    let cases = [
        (60, None),
        (61, Some(FrameWindow { start: 60, end: 61 })),
        (
            120,
            Some(FrameWindow {
                start: 60,
                end: 120,
            }),
        ),
        (
            121,
            Some(FrameWindow {
                start: 61,
                end: 121,
            }),
        ),
    ];

    for (track_frames, expected_tail) in cases {
        let plan = AnalysisWindowPlan::new(AutomixAnalysisMode::Full, Some(track_frames), window);
        assert_eq!(plan.head, FrameWindow { start: 0, end: 60 });
        assert_eq!(plan.tail, expected_tail);
        if let Some(tail) = plan.tail {
            assert!(plan.head.end <= tail.start);
        }
    }

    assert_eq!(
        AnalysisWindowPlan::new(AutomixAnalysisMode::Head, Some(121), window).tail,
        None
    );
    assert_eq!(
        AnalysisWindowPlan::new(AutomixAnalysisMode::Full, None, window),
        AnalysisWindowPlan {
            head: FrameWindow {
                start: 0,
                end: window,
            },
            tail: None,
        }
    );
}

#[test]
fn packet_selection_slices_once_before_all_metric_consumers() {
    let sample_rate = 8_000;
    let channels = 2;
    let packet_frames = 1_400;
    let mut packet = Vec::with_capacity(packet_frames * channels);
    for frame in 0..packet_frames {
        let value = frame as f64 / packet_frames as f64 * 0.5;
        packet.extend_from_slice(&[value, value]);
    }
    let mut skip_remaining = 128;
    let frame_range = select_packet_frames(packet_frames, &mut skip_remaining, 1_024)
        .expect("packet should contain selected frames");

    assert_eq!(frame_range, 128..1_152);
    assert_eq!(skip_remaining, 0);
    let sample_range = frame_range.start * channels..frame_range.end * channels;
    let selected = &packet[sample_range];
    assert_eq!(selected.len(), 1_024 * channels);
    assert_eq!(selected[0], packet[128 * channels]);
    assert_eq!(selected[selected.len() - 1], packet[1_152 * channels - 1]);

    let mut meter = LoudnessMeter::new(channels, sample_rate).unwrap();
    let mut segment = AnalysisSegment::default();
    SegmentAnalyzer::new(sample_rate, channels)
        .process(selected, &mut meter, &mut segment, None)
        .unwrap();

    assert_eq!(meter.frames_processed(), 1_024);
    assert_eq!(segment.frames_analyzed, 1_024);
    assert_eq!(segment.envelope.len(), 6);
    assert_eq!(segment.low_envelope.len(), 6);
    assert_eq!(segment.sample_rate, 8_000);
    assert!(segment.partial_envelope.is_some());
    assert_eq!(segment.spectral_flux.len(), 1);
}

#[test]
fn segment_start_time_is_the_single_tail_timeline_origin() {
    let sample_rate = 8_000;
    let meter = LoudnessMeter::new(1, sample_rate).unwrap();
    let head = AnalysisSegment {
        frames_analyzed: 5 * u64::from(sample_rate),
        envelope: vec![0.25; 250],
        ..AnalysisSegment::default()
    };
    let tail = AnalysisSegment {
        start_time: 12.0,
        frames_analyzed: 2 * u64::from(sample_rate),
        envelope: vec![0.25; 100],
        ..AnalysisSegment::default()
    };

    let analysis = finalize_analysis(
        &AutomixAnalysisOptions {
            mode: AutomixAnalysisMode::Full,
            max_analyze_time_sec: 5.0,
        },
        20.0,
        sample_rate,
        &meter,
        &head,
        &tail,
        None,
    )
    .unwrap();

    assert!((analysis.fade_out_pos - 14.0).abs() < 0.001);
    assert!(analysis.energy_profile[120] > 0.0);
    assert_eq!(analysis.energy_profile[180], 0.0);
}

#[test]
fn an_absurd_declared_duration_cannot_size_the_energy_profile() {
    // A container may declare any duration. Before the ceiling, this asked
    // for `1e12 * ENERGY_PROFILE_RATE` slots and aborted the process.
    let sample_rate = 8_000_u32;
    let head = AnalysisSegment {
        frames_analyzed: 5 * u64::from(sample_rate),
        envelope: vec![0.25; 250],
        ..AnalysisSegment::default()
    };

    let profile = build_energy_profile(&head, None, 1.0e12);

    assert_eq!(
        profile.len(),
        (MAX_DECLARED_DURATION_SEC * ENERGY_PROFILE_RATE) as usize
    );
    assert!(profile[0] > 0.0, "head evidence still lands at its origin");
}

#[test]
fn an_implausible_declared_duration_falls_back_to_measured_head_evidence() {
    // Discarded rather than clamped: the analysis reports the five seconds
    // it actually decoded, not a confident 24-hour timeline it never saw.
    assert!(!is_plausible_duration(&(MAX_DECLARED_DURATION_SEC + 1.0)));
    assert!(!is_plausible_duration(&f64::INFINITY));
    assert!(!is_plausible_duration(&0.0));
    assert!(is_plausible_duration(&MAX_DECLARED_DURATION_SEC));

    let sample_rate = 8_000;
    let meter = LoudnessMeter::new(1, sample_rate).unwrap();
    let head = AnalysisSegment {
        frames_analyzed: 5 * u64::from(sample_rate),
        envelope: vec![0.25; 250],
        ..AnalysisSegment::default()
    };

    // `duration = 0.0` is what the caller passes once it rejects the
    // declared value, so `finalize_analysis` derives the timeline itself.
    let analysis = finalize_analysis(
        &AutomixAnalysisOptions {
            mode: AutomixAnalysisMode::Head,
            max_analyze_time_sec: 5.0,
        },
        0.0,
        sample_rate,
        &meter,
        &head,
        &AnalysisSegment::default(),
        None,
    )
    .unwrap();

    assert!((analysis.duration - 5.0).abs() < 0.001);
    assert_eq!(
        analysis.energy_profile.len(),
        (5.0 * ENERGY_PROFILE_RATE) as usize
    );
}

#[test]
fn full_analysis_uses_absolute_tail_positions_at_window_boundaries() {
    for duration_secs in [6_u64, 10, 11] {
        let analysis = analyze_tail_fixture(duration_secs);
        let expected_end = duration_secs as f64 - 0.4;
        assert_eq!(
            analysis.bpm, None,
            "fixture must not exercise beat snapping"
        );
        for (name, actual) in [
            ("fade_out", analysis.fade_out_pos),
            ("cut_out", analysis.cut_out_pos.expect("Full mode cut-out")),
            ("mix_center", analysis.mix_center_pos),
        ] {
            assert!(
                (actual - expected_end).abs() <= 0.05,
                "{duration_secs}s {name}: expected {expected_end:.3}s, got {actual:.3}s"
            );
        }
    }
}

#[test]
fn silence_detection_uses_head_and_tail_windows() {
    let sample_rate = 8_000;
    let meter = LoudnessMeter::new(1, sample_rate).unwrap();
    let mut head_envelope = vec![0.0; 50];
    head_envelope.extend(vec![0.02; 100]);
    let mut tail_envelope = vec![0.02; 100];
    tail_envelope.extend(vec![0.0; 50]);
    let head = AnalysisSegment {
        frames_analyzed: 3 * u64::from(sample_rate),
        envelope: head_envelope,
        ..AnalysisSegment::default()
    };
    // The tail origin comes from its segment, never from duration minus length.
    let tail = AnalysisSegment {
        start_time: 17.0,
        frames_analyzed: 3 * u64::from(sample_rate),
        reaches_input_end: true,
        envelope: tail_envelope,
        ..AnalysisSegment::default()
    };

    let analysis = finalize_analysis(
        &AutomixAnalysisOptions {
            mode: AutomixAnalysisMode::Full,
            max_analyze_time_sec: 5.0,
        },
        20.0,
        sample_rate,
        &meter,
        &head,
        &tail,
        None,
    )
    .unwrap();

    assert!((analysis.fade_in_pos - 1.0).abs() < 1e-9);
    assert!((analysis.fade_out_pos - 19.0).abs() < 1e-9);
    let outro = analysis.outro_energy_level.expect("tail outro evidence");
    assert!((outro - 20.0 * 0.02_f64.log10()).abs() < 1e-4, "{outro}");
}

/// Fixture rate: 160 PCM frames per 20 ms envelope bin.
const SPAN_RATE: u32 = 8_000;

fn dbfs(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

/// Mono PCM16 made of constant `(start_sec, end_sec, amplitude)` spans over
/// digital silence. Later spans override earlier ones; bounds are exact frames.
fn span_wav(duration_sec: f64, spans: &[(f64, f64, f64)]) -> Vec<u8> {
    let to_frame = |seconds: f64| (seconds * f64::from(SPAN_RATE)).round() as u64;
    let spans: Vec<_> = spans
        .iter()
        .map(|&(start, end, amplitude)| (to_frame(start), to_frame(end), amplitude))
        .collect();
    synth_wav(SPAN_RATE, to_frame(duration_sec), |frame| {
        spans
            .iter()
            .rev()
            .find(|(start, end, _)| (*start..*end).contains(&frame))
            .map_or(0.0, |span| span.2)
    })
}

fn analyze_wav_bytes(wav: &[u8], mode: AutomixAnalysisMode) -> AutomixAnalysis {
    let fixture = TempAudio::wav(wav);
    analyze_automix(
        MediaLocation::local(fixture.path_string()),
        None,
        AutomixAnalysisOptions {
            mode,
            max_analyze_time_sec: MIN_ANALYZE_TIME_SEC,
        },
    )
    .expect("analyze span fixture")
}

fn analyze_spans(
    duration_sec: f64,
    spans: &[(f64, f64, f64)],
    mode: AutomixAnalysisMode,
) -> AutomixAnalysis {
    let analysis = analyze_wav_bytes(&span_wav(duration_sec, spans), mode);
    assert_eq!(
        analysis.bpm, None,
        "span fixtures must not exercise snapping"
    );
    analysis
}

fn assert_at(name: &str, actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{name}: expected {expected}, got {actual}"
    );
}

#[test]
fn a_click_three_seconds_before_the_music_does_not_move_fade_in() {
    let music = dbfs(-20.0);
    // 10 ms at -20 dBFS starting exactly 3 s before the music.
    let analysis = analyze_spans(
        12.0,
        &[(1.0, 1.01, music), (4.0, 10.0, music)],
        AutomixAnalysisMode::Full,
    );
    assert_at("fade_in", analysis.fade_in_pos, 4.0);
    assert_at("cut_in", analysis.cut_in_pos.unwrap(), 4.0);
    assert_at("fade_out", analysis.fade_out_pos, 10.0);
    assert_at("cut_out", analysis.cut_out_pos.unwrap(), 10.0);
    assert_at("mix_center", analysis.mix_center_pos, 10.0);
    // The outro ends at the same boundary: 3 s of -20 dBFS tail evidence.
    let outro = analysis.outro_energy_level.unwrap();
    assert!((outro + 20.0).abs() < 0.01, "{outro}");
}

#[test]
fn count_in_ticks_do_not_create_a_sound_boundary() {
    let music = dbfs(-20.0);
    // 70 ms ticks start 10 ms into a bin, so each touches exactly four bins.
    let mut spans: Vec<_> = [1.01, 1.51, 2.01, 2.51]
        .into_iter()
        .map(|start| (start, start + 0.07, music))
        .collect();
    spans.push((3.0, 10.0, music));
    let analysis = analyze_spans(12.0, &spans, AutomixAnalysisMode::Full);
    assert_at("fade_in", analysis.fade_in_pos, 3.0);
    assert_at("cut_in", analysis.cut_in_pos.unwrap(), 3.0);
}

#[test]
fn sound_confirmation_counts_complete_pcm_bins() {
    let music = dbfs(-20.0);
    // Aligned 80 ms = 4 bins: not sound. Aligned 100 ms = 5 bins: sound.
    for (burst, expected) in [(0.08, 3.0), (0.10, 1.0)] {
        let analysis = analyze_spans(
            12.0,
            &[(1.0, 1.0 + burst, music), (3.0, 10.0, music)],
            AutomixAnalysisMode::Full,
        );
        assert_at("fade_in", analysis.fade_in_pos, expected);
    }
}

#[test]
fn a_short_mid_track_dip_does_not_trigger_fade_out() {
    let music = dbfs(-20.0);
    // 60 ms inside the Full-mode tail, into the dead band and to silence.
    for dip in [dbfs(-50.0), 0.0] {
        let analysis = analyze_spans(
            12.0,
            &[(0.5, 11.0, music), (8.5, 8.56, dip)],
            AutomixAnalysisMode::Full,
        );
        assert_at("fade_out", analysis.fade_out_pos, 11.0);
        assert_at("cut_out", analysis.cut_out_pos.unwrap(), 11.0);
        assert_at("mix_center", analysis.mix_center_pos, 11.0);
    }
}

#[test]
fn silence_confirmation_counts_complete_pcm_bins() {
    let music = dbfs(-20.0);
    let hum = dbfs(-51.0);
    // 99 silent bins, then dead band to the end: the sound stays latched.
    let ninety_nine = analyze_spans(
        12.0,
        &[(0.5, 9.0, music), (10.98, 12.0, hum)],
        AutomixAnalysisMode::Full,
    );
    assert_at("99-bin fade_out", ninety_nine.fade_out_pos, 12.0);
    // 100 silent bins latch silence at the start of the run.
    let hundred = analyze_spans(
        12.0,
        &[(0.5, 9.0, music), (11.0, 12.0, hum)],
        AutomixAnalysisMode::Full,
    );
    assert_at("100-bin fade_out", hundred.fade_out_pos, 9.0);
    assert_at("100-bin cut_out", hundred.cut_out_pos.unwrap(), 9.0);
    let outro = hundred.outro_energy_level.unwrap();
    assert!((outro + 20.0).abs() < 0.01, "{outro}");
}

#[test]
fn a_tail_shorter_than_a_silence_run_uses_the_end_of_input_rule() {
    let music = dbfs(-20.0);
    // 5.5 s track, 5 s windows: the tail is [5.0, 5.5) = 25 bins.
    let latched = analyze_spans(5.5, &[(0.5, 5.2, music)], AutomixAnalysisMode::Full);
    assert_at("fade_out", latched.fade_out_pos, 5.2);
    assert_at("cut_out", latched.cut_out_pos.unwrap(), 5.2);
    // Three tail bins cannot latch sound, and the head is not joined to it.
    let unlatched = analyze_spans(5.5, &[(0.5, 5.06, music)], AutomixAnalysisMode::Full);
    assert_at("unlatched fade_out", unlatched.fade_out_pos, 5.5);
    assert!(unlatched.outro_energy_level.is_some());
}

#[test]
fn a_head_window_at_the_analysis_cap_is_not_end_of_input() {
    let music = dbfs(-20.0);
    let spans = [(0.5, 4.0, music)];
    // Head [0, 5) ends in 1 s of unconfirmed silence before the track end.
    let head = analyze_spans(12.0, &spans, AutomixAnalysisMode::Head);
    assert_at("head fade_in", head.fade_in_pos, 0.5);
    assert_at("head fade_out", head.fade_out_pos, 12.0);
    assert_at("head mix_center", head.mix_center_pos, 5.0);
    // The same content ending the input confirms the trailing silence.
    let whole = analyze_spans(4.5, &spans, AutomixAnalysisMode::Full);
    assert_at("whole fade_out", whole.fade_out_pos, 4.0);
    assert_at("whole mix_center", whole.mix_center_pos, 4.0);
    assert_eq!(whole.outro_energy_level, None, "no tail window");
    // An all-silent tail has no sound end; nothing is bridged from the head.
    let full = analyze_spans(12.0, &spans, AutomixAnalysisMode::Full);
    assert_at("full fade_out", full.fade_out_pos, 12.0);
    assert_eq!(full.outro_energy_level, Some(-70.0));
}

#[test]
fn decoder_end_of_stream_marks_the_window_as_input_end() {
    // 8,000 frames. Only a take that the stream cannot fill observes end of
    // stream; an exactly filled take relies on the planner's track-end check.
    let fixture = TempAudio::wav(&span_wav(1.0, &[(0.0, 1.0, dbfs(-20.0))]));
    for (take, expected) in [(4_000, false), (8_000, false), (12_000, true)] {
        let mut decoder = StreamingDecoder::open_with_credentials_and_cancel(
            MediaLocation::local(fixture.path_string()),
            None,
            None,
        )
        .unwrap();
        let mut meter = LoudnessMeter::new(1, SPAN_RATE).unwrap();
        let mut segment = AnalysisSegment::default();
        decode_segment(&mut decoder, &mut meter, &mut segment, 0, take, None, None).unwrap();
        assert_eq!(segment.reaches_input_end, expected, "take {take}");
        assert_eq!(segment.frames_analyzed, take.min(8_000), "take {take}");
    }
    assert!(!FrameWindow { start: 0, end: 40 }.ends_at_track_end(None));
    assert!(!FrameWindow { start: 0, end: 40 }.ends_at_track_end(Some(41)));
    assert!(FrameWindow { start: 0, end: 40 }.ends_at_track_end(Some(40)));
}

#[test]
fn all_silent_input_keeps_the_default_boundaries() {
    let analysis = analyze_spans(12.0, &[], AutomixAnalysisMode::Full);
    assert_at("fade_in", analysis.fade_in_pos, 0.0);
    assert_at("fade_out", analysis.fade_out_pos, 12.0);
    assert_at("cut_in", analysis.cut_in_pos.unwrap(), 0.0);
    assert_at("cut_out", analysis.cut_out_pos.unwrap(), 12.0);
    assert_at("mix_center", analysis.mix_center_pos, 12.0);
    assert_eq!(analysis.outro_energy_level, Some(-70.0));
}

#[test]
fn bpm_detection_returns_structured_low_confidence_for_flat_signal() {
    let values = vec![0.01; 160];
    let estimate = tempo::estimate(&values, 50.0, 0.0, None).unwrap();

    assert!(estimate.grid.is_none());
    assert!(estimate.confidence.is_none());
}

#[test]
fn bpm_detection_rejects_invalid_rate_and_short_input() {
    let values = pulse_train(50.0, 120.0, 12.0);
    for rate in [0.0, -50.0, f64::NAN, f64::INFINITY] {
        assert!(tempo::estimate(&values, rate, 0.0, None)
            .unwrap()
            .grid
            .is_none());
    }
    assert!(tempo::estimate(&values[..99], 50.0, 0.0, None)
        .unwrap()
        .grid
        .is_none());
    assert!(tempo::estimate(&vec![f32::NAN; 500], 50.0, 0.0, None)
        .unwrap()
        .grid
        .is_none());
}

#[test]
fn bpm_detection_finds_regular_pulse_train() {
    let mut values = vec![0.0; 300];
    for idx in (0..values.len()).step_by(25) {
        values[idx] = 1.0;
    }

    let estimate = tempo::estimate(&values, 50.0, 0.0, None).unwrap();

    assert!(estimate
        .bpm()
        .is_some_and(|value| (value - 120.0).abs() <= 0.05));
    assert!(estimate.usable_grid().is_some());
}

#[test]
fn sub_frame_grid_regression_preserves_precision_and_unrounded_drift() {
    // This is an ODF-only test. The integration suite separately drives
    // the complete native-rate PCM -> decode -> FFT -> public DTO path.
    for rate in [50.0, 44_100.0 / 220.0, 200.0] {
        for bpm in [60.0, 127.3, 174.6, 200.0] {
            let values = pulse_train(rate, bpm, 60.0);
            let estimate = tempo::estimate(&values, rate, 0.0, None).unwrap();
            let detected = estimate.bpm().unwrap_or_else(|| {
                panic!("expected {bpm} BPM to be detected at observation rate {rate}")
            });
            assert!(
                (detected - bpm).abs() <= 0.05,
                "expected {bpm} BPM at {rate} Hz, got {detected}"
            );
            let grid = estimate.grid.unwrap();
            let true_period = 60.0 / bpm;
            let phase_error = (grid.first_beat_sec + true_period / 2.0).rem_euclid(true_period)
                - true_period / 2.0;
            assert!(
                phase_error.abs() <= 0.010,
                "{rate} Hz, {bpm} BPM phase: {phase_error}"
            );
            let last = (60.0 / true_period).floor();
            for period in [grid.period_sec, 60.0 / detected] {
                assert!(
                    (phase_error + last * (period - true_period)).abs() <= 0.020,
                    "{rate} Hz, {bpm} BPM drift"
                );
            }
        }
    }
}

#[test]
fn finalize_analysis_uses_spectral_flux_cadence() {
    let sample_rate = 44_100;
    let flux_rate = sample_rate as f64 / spectral_hop_size(sample_rate) as f64;
    let analysis = empty_analysis_with_flux(sample_rate, pulse_train(flux_rate, 120.0, 12.0));

    assert!(
        analysis
            .bpm
            .is_some_and(|value| (value - 120.0).abs() <= 0.05),
        "spectral-flux BPM used the wrong cadence: {:?}",
        analysis.bpm
    );
}

#[test]
fn cuts_snap_to_individual_beats_only_with_usable_evidence() {
    let grid = BeatGrid {
        period_sec: 0.5,
        first_beat_sec: 0.217,
        stability: 0.95,
    };
    let tempo = TempoEstimate {
        grid: Some(grid),
        confidence: Some(0.9),
    };
    assert!((snap_to_beat(1.3, grid) - 1.217).abs() < 1e-12);
    assert!((calculate_smart_cut_in(tempo, Some(70.1), 0.1) - 6.217).abs() < 1e-12);
    assert!((calculate_smart_cut_out(tempo, 10.1, 11.0) - 10.217).abs() < 1e-12);
    assert!(calculate_smart_cut_out(tempo, 11.0, 11.0) <= 11.0);
    for (confidence, stability) in [(0.34, 0.95), (0.9, 0.79)] {
        let uncertain = TempoEstimate {
            grid: Some(BeatGrid { stability, ..grid }),
            confidence: Some(confidence),
        };
        assert_eq!(calculate_smart_cut_in(uncertain, Some(70.1), 0.1), 0.1);
        assert_eq!(calculate_smart_cut_out(uncertain, 10.1, 11.0), 10.1);
    }
}

#[test]
fn boundary_beat_reporting_requires_isolated_pcm_energy_and_keeps_the_grid() {
    let grid = BeatGrid {
        period_sec: 0.5,
        first_beat_sec: 0.495,
        stability: 0.95,
    };
    let head = AnalysisSegment {
        envelope: vec![0.1, 0.0],
        ..AnalysisSegment::at(0.0)
    };
    assert_eq!(reported_first_beat(grid, &head), 0.0);
    let sustained = AnalysisSegment {
        envelope: vec![0.1, 0.1],
        ..AnalysisSegment::at(0.0)
    };
    assert_eq!(reported_first_beat(grid, &sustained), grid.first_beat_sec);
    assert_eq!(snap_to_beat(0.27, grid), 0.495);
}

#[test]
fn boundary_beat_reporting_preserves_origin_and_requires_both_energy_windows() {
    let silence = 10.0_f32.powf(SILENCE_THRESHOLD_DB / 20.0);
    for origin in [0.0, 17.25] {
        let grid = BeatGrid {
            period_sec: 0.5,
            first_beat_sec: origin + 0.495,
            stability: 0.95,
        };
        for envelope in [
            vec![],
            vec![0.1],
            vec![0.0, 0.0],
            vec![silence, 0.0],
            vec![0.1, 0.1],
        ] {
            let head = AnalysisSegment {
                envelope,
                ..AnalysisSegment::at(origin)
            };
            assert_eq!(reported_first_beat(grid, &head), grid.first_beat_sec);
        }
        let head = AnalysisSegment {
            envelope: vec![0.1, silence],
            ..AnalysisSegment::at(origin)
        };
        assert_eq!(reported_first_beat(grid, &head), origin);
        let outside_boundary = BeatGrid {
            first_beat_sec: origin + 0.48,
            ..grid
        };
        assert_eq!(
            reported_first_beat(outside_boundary, &head),
            outside_boundary.first_beat_sec
        );
    }
}

#[test]
fn serialized_v4_analysis_pins_null_grid_and_omits_key_placeholders() {
    let analysis = empty_analysis_with_flux(48_000, Vec::new());
    let json = serde_json::to_value(&analysis).expect("analysis should serialize");

    assert_eq!(json["version"], 4);
    assert_eq!(json["mode"], "head");
    assert!(json["beat_grid_stability"].is_null());
    for field in ["vocal_in_pos", "vocal_out_pos", "vocal_last_in_pos"] {
        assert!(json.get(field).is_some_and(serde_json::Value::is_null));
    }
    let expected_keys: std::collections::BTreeSet<_> = [
        "version",
        "mode",
        "duration",
        "analyze_window",
        "bpm",
        "bpm_confidence",
        "first_beat_pos",
        "beat_grid_stability",
        "loudness",
        "true_peak_dbtp",
        "fade_in_pos",
        "fade_out_pos",
        "cut_in_pos",
        "cut_out_pos",
        "mix_center_pos",
        "mix_start_pos",
        "mix_end_pos",
        "energy_profile",
        "drop_pos",
        "vocal_in_pos",
        "vocal_out_pos",
        "vocal_last_in_pos",
        "outro_energy_level",
    ]
    .into_iter()
    .collect();
    let actual_keys: std::collections::BTreeSet<_> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(actual_keys, expected_keys);
    for field in [
        "key_status",
        "key_root",
        "key_pitch_class",
        "key_mode",
        "key_confidence",
        "camelot_key",
    ] {
        assert!(json.get(field).is_none(), "{field} must not be reserved");
    }
}

#[test]
fn analysis_reports_cancellation_as_a_typed_variant() {
    let token = DecodeCancelToken::new();
    token.cancel();

    let error = analyze_automix_with_cancel(
        MediaLocation::local("unused.wav"),
        None,
        AutomixAnalysisOptions::default(),
        Some(token),
    )
    .expect_err("pre-canceled analysis must stop before opening the source");

    assert!(matches!(error, AutomixError::Canceled));
}

#[test]
fn analysis_preserves_decoder_error_source() {
    let id = TEMP_AUDIO_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aec_automix_missing_{}_{}.wav",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_file(&path);
    let error = analyze_automix(
        MediaLocation::local(path),
        None,
        AutomixAnalysisOptions::default(),
    )
    .expect_err("missing source must fail");

    assert!(matches!(
        &error,
        AutomixError::Decoder(DecoderError::FileOpen(_))
    ));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn analysis_preserves_loudness_process_error_source() {
    let mut meter = LoudnessMeter::new(2, 48_000).unwrap();
    let mut segment = AnalysisSegment::default();
    let error = SegmentAnalyzer::new(48_000, 2)
        .process(&[0.25, -0.25, 0.5], &mut meter, &mut segment, None)
        .expect_err("incomplete interleaved frame must fail");

    assert!(matches!(
        error,
        AutomixError::Loudness(ProcessError::InvalidBlock(
            audio_engine_core::audio_block::AudioBlockError::IncompleteFrame {
                samples: 3,
                channels: 2,
            }
        ))
    ));
    assert_eq!(meter.frames_processed(), 0);
    assert_eq!(segment.frames_analyzed, 0);
}

mod structure_edges;
