use super::{analyze_decoder, AnalysisDecoder};
use crate::descriptors::{DescriptorCoverageStatus, DescriptorReport, DescriptorSegmentRole};
use crate::{AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions, AutomixError};
use audio_engine_core::channel_layout::ChannelLayout;
use audio_engine_core::decoder::{AudioInfo, DecoderError, TrackMetadata};
use audio_engine_core::processor::ProcessError;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::error::Error;
use std::rc::Rc;

const RATE: u32 = 8_000;
const WINDOW: u64 = 40_000;

#[derive(Clone, Debug)]
enum Step {
    Packet(Vec<f64>),
    Eof,
    DecodeError(&'static str),
    DecoderCanceled,
    Seek { target: u64, realized: u64 },
    SeekError { target: u64, message: &'static str },
}

#[derive(Debug, PartialEq)]
enum Call {
    Decode(usize),
    Seek(u64),
    Position(u64),
}

struct ScriptedDecoder {
    info: AudioInfo,
    steps: VecDeque<Step>,
    step_index: usize,
    current_frame: u64,
    calls: RefCell<Vec<Call>>,
    canceled: Rc<Cell<bool>>,
    cancel_after_step: Option<usize>,
}

impl ScriptedDecoder {
    fn new(info: AudioInfo, steps: Vec<Step>) -> Self {
        Self {
            info,
            steps: steps.into(),
            step_index: 0,
            current_frame: 0,
            calls: RefCell::new(Vec::new()),
            canceled: Rc::new(Cell::new(false)),
            cancel_after_step: None,
        }
    }

    fn next(&mut self) -> Step {
        if self.cancel_after_step == Some(self.step_index) {
            self.canceled.set(true);
        }
        self.step_index += 1;
        self.steps
            .pop_front()
            .expect("unexpected extra decoder call")
    }
}

impl AnalysisDecoder for ScriptedDecoder {
    fn info(&self) -> &AudioInfo {
        &self.info
    }

    fn decode_next_into(&mut self, buffer: &mut Vec<f64>) -> Result<Option<usize>, DecoderError> {
        self.calls.borrow_mut().push(Call::Decode(self.step_index));
        match self.next() {
            Step::Packet(samples) => {
                assert!(buffer.is_empty(), "driver must clear each packet buffer");
                self.current_frame += (samples.len() / self.info.channels.max(1)) as u64;
                buffer.extend_from_slice(&samples);
                Ok(Some(samples.len()))
            }
            Step::Eof => Ok(None),
            Step::DecodeError(message) => Err(DecoderError::Decoder(message.to_owned())),
            Step::DecoderCanceled => Err(DecoderError::Canceled),
            step => panic!("expected seek, got decode at {step:?}"),
        }
    }

    fn seek(&mut self, seconds: f64) -> Result<(), DecoderError> {
        self.calls.borrow_mut().push(Call::Seek(seconds.to_bits()));
        match self.next() {
            Step::Seek { target, realized } => {
                assert_eq!(seconds.to_bits(), (target as f64 / RATE as f64).to_bits());
                self.current_frame = realized;
                Ok(())
            }
            Step::SeekError { target, message } => {
                assert_eq!(seconds.to_bits(), (target as f64 / RATE as f64).to_bits());
                Err(DecoderError::Decoder(message.to_owned()))
            }
            step => panic!("expected decode, got seek at {step:?}"),
        }
    }

    fn current_frame(&self) -> u64 {
        self.calls
            .borrow_mut()
            .push(Call::Position(self.current_frame));
        self.current_frame
    }
}

fn info(total_frames: Option<u64>, channels: usize) -> AudioInfo {
    AudioInfo {
        sample_rate: RATE,
        channels,
        channel_layout: ChannelLayout::from_count(channels),
        bits_per_sample: Some(64),
        total_frames,
        duration_secs: None,
        encoder_delay: 0,
        end_padding: 0,
        metadata: TrackMetadata::default(),
    }
}

fn options(mode: AutomixAnalysisMode) -> AutomixAnalysisOptions {
    AutomixAnalysisOptions {
        mode,
        max_analyze_time_sec: 5.0,
    }
}

fn seek_call(target: u64) -> Call {
    Call::Seek((target as f64 / RATE as f64).to_bits())
}

// Walk the whole DTO, including arrays and nulls. Comparing Number directly
// would regard -0.0 and +0.0 as equal, so compare floating-point bits explicitly.
fn assert_value_bits_eq(left: &Value, right: &Value, path: &str) {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) if left.is_f64() || right.is_f64() => {
            assert_eq!(left.is_f64(), right.is_f64(), "{path}");
            assert_eq!(
                left.as_f64().unwrap().to_bits(),
                right.as_f64().unwrap().to_bits(),
                "{path}"
            );
        }
        (Value::Array(left), Value::Array(right)) => {
            assert_eq!(left.len(), right.len(), "{path}");
            for (index, (left, right)) in left.iter().zip(right).enumerate() {
                assert_value_bits_eq(left, right, &format!("{path}[{index}]"));
            }
        }
        (Value::Object(left), Value::Object(right)) => {
            assert_eq!(left.len(), right.len(), "{path}");
            for (key, left) in left {
                assert_value_bits_eq(
                    left,
                    right.get(key).expect("missing analysis field"),
                    &format!("{path}.{key}"),
                );
            }
        }
        _ => assert_eq!(left, right, "{path}"),
    }
}

fn paired_success(
    info: AudioInfo,
    steps: Vec<Step>,
    options: AutomixAnalysisOptions,
    expected_calls: Vec<Call>,
) -> (AutomixAnalysis, DescriptorReport) {
    let mut off = ScriptedDecoder::new(info.clone(), steps.clone());
    let mut on = ScriptedDecoder::new(info.clone(), steps);
    let mut report = DescriptorReport::new(info.sample_rate, info.channels.max(1));
    let original = analyze_decoder(&mut off, options.clone(), None, None).unwrap();
    let enhanced = analyze_decoder(&mut on, options, None, Some(&mut report)).unwrap();
    assert_value_bits_eq(
        &serde_json::to_value(&original).unwrap(),
        &serde_json::to_value(&enhanced).unwrap(),
        "analysis",
    );
    assert_eq!(
        *off.calls.borrow(),
        expected_calls,
        "unexpected original decoder path"
    );
    assert_eq!(
        *on.calls.borrow(),
        *off.calls.borrow(),
        "descriptor opt-in changed decoder calls"
    );
    assert_eq!(on.steps.len(), off.steps.len());
    (enhanced, report)
}

#[test]
fn unknown_length_uses_normalized_head_only_and_reports_actual_eof() {
    for (cap, planned_end) in [
        (-1.0, WINDOW),
        (f64::NAN, 480_000),
        (f64::INFINITY, 480_000),
    ] {
        let (analysis, report) = paired_success(
            info(None, 1),
            vec![Step::Packet(vec![0.25; 5_001]), Step::Eof],
            AutomixAnalysisOptions {
                mode: AutomixAnalysisMode::Full,
                max_analyze_time_sec: cap,
            },
            vec![Call::Decode(0), Call::Decode(1)],
        );
        assert_eq!(analysis.analyze_window, planned_end as f64 / RATE as f64);
        assert_eq!(
            report.segments.len(),
            1,
            "unknown duration must not invent a tail"
        );
        let head = &report.segments[0];
        assert_eq!(head.role, DescriptorSegmentRole::Head);
        assert_eq!(
            (head.start_frame, head.planned_end_frame, head.end_frame),
            (0, planned_end, 5_001)
        );
        assert_eq!(head.coverage_status, DescriptorCoverageStatus::ShortEof);
        assert_eq!(head.mono_signal.sample_count, 5_001);
        assert_eq!(head.spectral.frame_count, 1);
        assert_eq!(head.spectral.last_end_frame, Some(4_096));
    }
}

#[test]
fn unknown_input_distinguishes_empty_eof_from_filling_the_cap() {
    for actual in [0, WINDOW] {
        let steps = if actual == 0 {
            vec![Step::Eof]
        } else {
            vec![
                Step::Packet(vec![0.125; WINDOW as usize]),
                Step::DecodeError("cap must stop without EOF probe"),
            ]
        };
        let (_, report) = paired_success(
            info(None, 1),
            steps,
            options(AutomixAnalysisMode::Full),
            vec![Call::Decode(0)],
        );
        assert_eq!(report.segments.len(), 1);
        let head = &report.segments[0];
        assert_eq!(
            (head.start_frame, head.planned_end_frame, head.end_frame),
            (0, WINDOW, actual)
        );
        assert_eq!(head.mono_signal.sample_count, actual);
        if actual == 0 {
            assert_eq!(head.coverage_status, DescriptorCoverageStatus::ShortEof);
            assert_eq!(head.mono_signal.sample_peak, None);
            assert_eq!(head.mono_signal.rms, None);
            assert_eq!(head.mono_signal.dc_offset, None);
            assert_eq!(head.mono_signal.clipping_count, None);
            assert_eq!(head.spectral.frame_count, 0);
            assert_eq!(head.spectral.last_end_frame, None);
        } else {
            assert_eq!(head.coverage_status, DescriptorCoverageStatus::Complete);
            assert_eq!(head.mono_signal.rms, Some(0.125));
            assert_eq!(head.spectral.frame_count, 36);
            assert_eq!(head.spectral.last_end_frame, Some(39_936));
        }
    }
}

#[test]
fn known_short_track_has_no_tail_and_excludes_excess_packet_samples() {
    let mut samples = [0.25, 0.75].repeat(5_119);
    samples.extend([f64::NAN; 34]);
    let (_, report) = paired_success(
        info(Some(5_119), 2),
        vec![
            Step::Packet(Vec::new()),
            Step::Packet(samples),
            Step::DecodeError("must not read past cap"),
        ],
        options(AutomixAnalysisMode::Full),
        vec![Call::Decode(0), Call::Decode(1)],
    );
    assert_eq!(report.source_channels, 2);
    assert_eq!(report.segments.len(), 1);
    let head = &report.segments[0];
    assert_eq!(
        (head.start_frame, head.planned_end_frame, head.end_frame),
        (0, 5_119, 5_119)
    );
    assert_eq!(head.coverage_status, DescriptorCoverageStatus::Complete);
    assert_eq!(head.nonfinite_input_frame_count, 0);
    assert_eq!(head.mono_signal.sample_count, 5_119);
    assert_eq!(head.mono_signal.sample_peak, Some(0.5));
    assert_eq!(head.mono_signal.rms, Some(0.5));
    assert_eq!(head.mono_signal.dc_offset, Some(0.5));
    assert_eq!(head.spectral.frame_count, 1);
    assert_eq!(head.spectral.last_end_frame, Some(4_096));
}

#[test]
fn eof_keeps_planned_empty_tail_and_partial_tail_coverage() {
    let target = 60_000;
    for actual_tail in [0, 5_000] {
        let mut steps = vec![
            Step::Packet(vec![0.25; 1_600]),
            Step::Eof,
            Step::Seek {
                target,
                realized: target,
            },
        ];
        let mut calls = vec![
            Call::Decode(0),
            Call::Decode(1),
            seek_call(target),
            Call::Position(target),
        ];
        if actual_tail != 0 {
            calls.push(Call::Decode(steps.len()));
            steps.push(Step::Packet(vec![-0.5; actual_tail]));
        }
        calls.push(Call::Decode(steps.len()));
        steps.push(Step::Eof);
        let (_, report) = paired_success(
            info(Some(100_000), 1),
            steps,
            options(AutomixAnalysisMode::Full),
            calls,
        );
        assert_eq!(report.segments.len(), 2);
        assert_eq!(report.segments[0].end_frame, 1_600);
        assert_eq!(
            report.segments[0].coverage_status,
            DescriptorCoverageStatus::ShortEof
        );
        let tail = &report.segments[1];
        assert_eq!(tail.role, DescriptorSegmentRole::Tail);
        assert_eq!(
            (tail.start_frame, tail.planned_end_frame, tail.end_frame),
            (target, 100_000, target + actual_tail as u64)
        );
        assert_eq!(tail.coverage_status, DescriptorCoverageStatus::ShortEof);
        assert_eq!(tail.mono_signal.sample_count, actual_tail as u64);
        if actual_tail == 0 {
            let signal = serde_json::to_value(&tail.mono_signal).unwrap();
            for (field, value) in signal.as_object().unwrap() {
                if field != "sample_count" {
                    assert!(value.is_null(), "empty tail {field} must be null");
                }
            }
            assert_eq!(tail.spectral.frame_count, 0);
            assert_eq!(tail.spectral.last_end_frame, None);
        } else {
            assert_eq!(tail.mono_signal.rms, Some(0.5));
            assert_eq!(tail.mono_signal.dc_offset, Some(-0.5));
            assert_eq!(tail.spectral.frame_count, 1);
            assert_eq!(tail.spectral.last_end_frame, Some(target + 4_096));
        }
    }
}

#[test]
fn adjacent_and_gapped_full_windows_reset_signal_and_spectrum_state() {
    let head: Vec<_> = (0..WINDOW)
        .map(|frame| 0.5 * (std::f64::consts::TAU * frame as f64 / 32.0).sin())
        .collect();
    for (total, target, tail_len, tail_spectra, last_end) in [
        (43_071, 40_000, 3_071, 0, None),
        (103_071, 63_071, 40_000, 36, Some(103_007)),
    ] {
        let steps = vec![
            Step::Packet(head.clone()),
            Step::Seek {
                target,
                realized: target,
            },
            Step::Packet(vec![0.0; tail_len]),
        ];
        for mode in [AutomixAnalysisMode::Head, AutomixAnalysisMode::Full] {
            let mut calls = vec![Call::Decode(0)];
            if mode == AutomixAnalysisMode::Full {
                calls.extend([seek_call(target), Call::Position(target), Call::Decode(2)]);
            }
            let (_, report) =
                paired_success(info(Some(total), 1), steps.clone(), options(mode), calls);
            let head = &report.segments[0];
            assert_eq!(head.mono_signal.sample_count, WINDOW);
            assert_eq!(head.spectral.frame_count, 36);
            assert_eq!(head.spectral.last_end_frame, Some(39_936));
            assert_eq!(head.spectral.centroid_hz.valid_frame_count, 36);
            if mode == AutomixAnalysisMode::Head {
                assert_eq!(report.segments.len(), 1);
                continue;
            }
            assert_eq!(report.segments.len(), 2);
            let tail = &report.segments[1];
            assert_eq!(tail.role, DescriptorSegmentRole::Tail);
            assert_eq!(
                (tail.start_frame, tail.planned_end_frame, tail.end_frame),
                (target, total, total)
            );
            assert_eq!(tail.coverage_status, DescriptorCoverageStatus::Complete);
            assert_eq!(tail.mono_signal.sample_count, tail_len as u64);
            assert_eq!(tail.mono_signal.sample_peak, Some(0.0));
            assert_eq!(tail.mono_signal.rms, Some(0.0));
            assert_eq!(tail.mono_signal.crest_factor, None);
            assert_eq!(tail.mono_signal.dc_offset, Some(0.0));
            assert_eq!(tail.spectral.frame_count, tail_spectra);
            assert_eq!(tail.spectral.last_end_frame, last_end);
            assert_eq!(tail.spectral.centroid_hz.valid_frame_count, 0);
            assert_eq!(tail.spectral.centroid_hz.mean, None);
            assert_eq!(tail.spectral.flatness.valid_frame_count, 0);
            assert_eq!(tail.spectral.flatness.mean, None);
        }
    }
}

#[test]
fn early_seek_skips_multiple_packets_and_slices_both_packet_edges() {
    let target = 60_003;
    let mut first_selected = vec![f64::NAN; 4];
    first_selected.extend([-0.25; 8]);
    let mut last_selected = vec![-0.25; 39_992];
    last_selected.extend([f64::NAN; 19]);
    let (_, report) = paired_success(
        info(Some(100_003), 1),
        vec![
            Step::Packet(vec![0.5; WINDOW as usize]),
            Step::Seek {
                target,
                realized: target - 13,
            },
            Step::Packet(Vec::new()),
            Step::Packet(vec![f64::NAN; 5]),
            Step::Packet(vec![f64::NAN; 4]),
            Step::Packet(first_selected),
            Step::Packet(last_selected),
            Step::DecodeError("must not read after complete tail"),
        ],
        options(AutomixAnalysisMode::Full),
        vec![
            Call::Decode(0),
            seek_call(target),
            Call::Position(target - 13),
            Call::Decode(2),
            Call::Decode(3),
            Call::Decode(4),
            Call::Decode(5),
            Call::Decode(6),
        ],
    );
    let tail = &report.segments[1];
    assert_eq!((tail.start_frame, tail.end_frame), (target, 100_003));
    assert_eq!(
        tail.nonfinite_input_frame_count, 0,
        "skipped and excess samples are not inputs"
    );
    assert_eq!(tail.mono_signal.sample_count, WINDOW);
    assert_eq!(tail.mono_signal.sample_peak, Some(0.25));
    assert_eq!(tail.mono_signal.rms, Some(0.25));
    assert_eq!(tail.mono_signal.dc_offset, Some(-0.25));
    assert_eq!(tail.spectral.frame_count, 36);
    assert_eq!(tail.spectral.last_end_frame, Some(99_939));
}

#[test]
fn late_seek_is_the_same_typed_error_without_reading_a_tail_packet() {
    let target = 60_003;
    for enabled in [false, true] {
        let mut decoder = ScriptedDecoder::new(
            info(Some(100_003), 1),
            vec![
                Step::Packet(vec![0.25; WINDOW as usize]),
                Step::Seek {
                    target,
                    realized: target + 1,
                },
            ],
        );
        let mut report = DescriptorReport::new(RATE, 1);
        let result = analyze_decoder(
            &mut decoder,
            options(AutomixAnalysisMode::Full),
            None,
            enabled.then_some(&mut report),
        );
        assert!(matches!(
            result,
            Err(AutomixError::TailSeekPastStart {
                planned_frame: 60_003,
                realized_frame: 60_004
            })
        ));
        assert_eq!(
            *decoder.calls.borrow(),
            [
                Call::Decode(0),
                seek_call(target),
                Call::Position(target + 1)
            ]
        );
    }
}

#[test]
fn decode_and_seek_errors_preserve_the_decoder_source_and_stop_immediately() {
    let target = 60_000;
    let scenarios = [
        vec![Step::DecodeError("first packet")],
        vec![
            Step::Packet(vec![0.25; 64]),
            Step::DecodeError("later head packet"),
        ],
        vec![
            Step::Packet(vec![0.25; WINDOW as usize]),
            Step::SeekError {
                target,
                message: "tail seek",
            },
        ],
        vec![
            Step::Packet(vec![0.25; WINDOW as usize]),
            Step::Seek {
                target,
                realized: target - 20,
            },
            Step::Packet(vec![0.0; 10]),
            Step::DecodeError("skip packet"),
        ],
        vec![
            Step::Packet(vec![0.25; WINDOW as usize]),
            Step::Seek {
                target,
                realized: target,
            },
            Step::Packet(vec![0.5; 64]),
            Step::DecodeError("tail packet"),
        ],
    ];
    for steps in scenarios {
        let message = match steps.last().unwrap() {
            Step::DecodeError(message) | Step::SeekError { message, .. } => *message,
            _ => unreachable!(),
        };
        let mut traces = Vec::new();
        for enabled in [false, true] {
            let mut decoder = ScriptedDecoder::new(info(Some(100_000), 1), steps.clone());
            let mut report = DescriptorReport::new(RATE, 1);
            let error = analyze_decoder(
                &mut decoder,
                options(AutomixAnalysisMode::Full),
                None,
                enabled.then_some(&mut report),
            )
            .unwrap_err();
            assert!(
                matches!(&error, AutomixError::Decoder(DecoderError::Decoder(actual)) if actual == message)
            );
            assert!(
                matches!(error.source().unwrap().downcast_ref::<DecoderError>(), Some(DecoderError::Decoder(actual)) if actual == message)
            );
            assert!(
                decoder.steps.is_empty(),
                "script must reach the intended failure"
            );
            traces.push(decoder.calls.into_inner());
        }
        assert_eq!(traces[0], traces[1]);
    }
}

#[test]
fn decoder_cancellation_stays_distinct_from_cooperative_cancellation() {
    for enabled in [false, true] {
        let mut decoder = ScriptedDecoder::new(info(None, 1), vec![Step::DecoderCanceled]);
        let mut report = DescriptorReport::new(RATE, 1);
        let error = analyze_decoder(
            &mut decoder,
            options(AutomixAnalysisMode::Head),
            None,
            enabled.then_some(&mut report),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            AutomixError::Decoder(DecoderError::Canceled)
        ));
        assert!(matches!(
            error.source().unwrap().downcast_ref::<DecoderError>(),
            Some(DecoderError::Canceled)
        ));
        assert!(report.segments.is_empty());
        assert_eq!(*decoder.calls.borrow(), [Call::Decode(0)]);
    }
}

#[test]
fn cooperative_cancellation_stops_before_reading_or_processing_more_pcm() {
    for enabled in [false, true] {
        for packet in [None, Some(Vec::new()), Some(vec![0.25; 16_384])] {
            let before_read = packet.is_none();
            let steps = packet.into_iter().map(Step::Packet).collect();
            let mut decoder = ScriptedDecoder::new(info(None, 1), steps);
            decoder.cancel_after_step = Some(0);
            decoder.canceled.set(before_read);
            let token = Rc::clone(&decoder.canceled);
            let canceled = || token.get();
            let mut report = DescriptorReport::new(RATE, 1);
            let error = analyze_decoder(
                &mut decoder,
                options(AutomixAnalysisMode::Head),
                Some(&canceled),
                enabled.then_some(&mut report),
            )
            .unwrap_err();
            assert!(matches!(error, AutomixError::Canceled));
            assert!(error.source().is_none());
            assert!(
                report.segments.is_empty(),
                "cancellation must not publish a partial head"
            );
            assert_eq!(decoder.calls.borrow().len(), usize::from(!before_read));
        }
    }
}

#[test]
fn existing_loudness_setup_error_has_precedence_over_descriptor_setup() {
    for enabled in [false, true] {
        let mut source = info(None, 1);
        source.sample_rate = 0;
        let mut decoder = ScriptedDecoder::new(source, Vec::new());
        let mut report = DescriptorReport::new(0, 1);
        let error = analyze_decoder(
            &mut decoder,
            options(AutomixAnalysisMode::Head),
            None,
            enabled.then_some(&mut report),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            AutomixError::Loudness(ProcessError::InvalidSampleRate {
                processor: "LoudnessMeter",
                sample_rate_hz: 0
            })
        ));
        assert!(matches!(
            error.source().unwrap().downcast_ref::<ProcessError>(),
            Some(ProcessError::InvalidSampleRate {
                processor: "LoudnessMeter",
                sample_rate_hz: 0
            })
        ));
        assert!(decoder.calls.borrow().is_empty());
        assert!(report.segments.is_empty());
    }
}
