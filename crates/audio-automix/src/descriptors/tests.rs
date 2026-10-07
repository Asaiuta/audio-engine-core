use std::cell::Cell;

use audio_engine_core::audio_block::AudioBlockError;

use super::*;

fn varying_signal(len: usize) -> Vec<f64> {
    let mut state = 0x0123_4567_89ab_cdef_u64;
    (0..len)
        .map(|i| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let noise = ((state >> 32) as u32 as f64 / u32::MAX as f64 - 0.5) * 0.07;
            let phase = std::f64::consts::TAU * i as f64 / FFT_SIZE as f64;
            let (bin, gain) = match (i / HOP_SIZE) % 4 {
                0 => (19.0, 0.1),
                1 => (371.0, 1.2),
                2 => (87.0, 0.45),
                _ => (913.0, 0.7),
            };
            gain * (phase * bin).sin() + noise + 0.125
        })
        .collect()
}

fn analyze(samples: &[f64], channels: usize, rate: u32, chunk_frames: usize) -> DescriptorSegment {
    let mut accumulator = DescriptorSegmentAccumulator::new(rate, channels).unwrap();
    for block in samples.chunks(chunk_frames * channels) {
        accumulator.process(block, None).unwrap();
    }
    accumulator.finish(
        DescriptorSegmentRole::Head,
        0,
        (samples.len() / channels) as u64,
    )
}

// Each independent core instance sees exactly one complete source window.
// This catches missed latest-frame publications and incorrect hop geometry.
fn spectral_oracle(mono: &[f64], rate: u32) -> DescriptorSpectralSummary {
    let mut result = DescriptorSpectralSummary::new();
    for (index, window) in mono.windows(FFT_SIZE).step_by(HOP_SIZE).enumerate() {
        let mut core = DescriptorAnalyzer::new(&core_config(), rate).unwrap();
        assert_eq!(core.push(window), 1);
        let frame = core.spectral().unwrap();
        let polluted = window.iter().any(|sample| !sample.is_finite());
        result.frame_count += 1;
        result.nonfinite_frame_count += u64::from(polluted);
        result.last_end_frame = Some((FFT_SIZE + index * HOP_SIZE) as u64);
        for (summary, value) in [
            (&mut result.centroid_hz, frame.centroid_hz),
            (&mut result.flatness, frame.flatness),
        ] {
            if let Some(value) = value.filter(|value| value.is_finite()) {
                summary.valid_frame_count += 1;
                let previous = summary.mean.unwrap_or(0.0);
                summary.mean =
                    Some(previous + (value - previous) / summary.valid_frame_count as f64);
            }
        }
    }
    result
}

fn assert_close(actual: Option<f64>, expected: f64) {
    let actual = actual.unwrap();
    assert!(
        (actual - expected).abs() <= 1e-12 * expected.abs().max(1.0),
        "actual={actual:?}, expected={expected:?}"
    );
}

fn assert_signal_null(signal: &DescriptorSignal) {
    assert_eq!(signal.sample_peak, None);
    assert_eq!(signal.rms, None);
    assert_eq!(signal.crest_factor, None);
    assert_eq!(signal.dc_offset, None);
    assert_eq!(signal.clipping_count, None);
    assert_eq!(signal.clipping_ratio, None);
}

#[test]
fn actual_v1_geometry_has_no_padded_warmup_or_tail() {
    for len in [0, 1, 4095, 4096, 5119, 5120, 7281] {
        for rate in [44_100, 48_000, 96_000] {
            let segment = analyze(&vec![0.0; len], 1, rate, 317);
            let expected_frames = if len < 4096 {
                0
            } else {
                1 + (len - 4096) / 1024
            };
            assert_eq!(segment.spectral.frame_count, expected_frames as u64);
            assert_eq!(
                segment.spectral.last_end_frame,
                (expected_frames > 0).then(|| (4096 + (expected_frames - 1) * 1024) as u64)
            );
            assert_eq!(segment.mono_signal.sample_count, len as u64);
            assert_eq!(segment.end_frame, len as u64);
            assert_eq!(segment.coverage_status, DescriptorCoverageStatus::Complete);
            assert_eq!(segment.spectral.centroid_hz.valid_frame_count, 0);
            assert_eq!(segment.spectral.flatness.mean, None);
            if len == 0 {
                assert_signal_null(&segment.mono_signal);
            } else {
                assert_eq!(segment.mono_signal.sample_peak, Some(0.0));
                assert_eq!(segment.mono_signal.rms, Some(0.0));
                assert_eq!(segment.mono_signal.dc_offset, Some(0.0));
                assert_eq!(segment.mono_signal.crest_factor, None);
                assert_eq!(segment.mono_signal.clipping_count, Some(0));
                assert_eq!(segment.mono_signal.clipping_ratio, Some(0.0));
            }
        }
    }
}

#[test]
fn every_publication_matches_per_window_core_and_independent_signal_statistics() {
    let mono = varying_signal(FFT_SIZE + 11 * HOP_SIZE + 713);
    let expected = spectral_oracle(&mono, 48_000);
    assert_eq!(expected.frame_count, 12);
    assert_eq!(expected.centroid_hz.valid_frame_count, 12);
    let segment = analyze(&mono, 1, 48_000, mono.len());
    assert_eq!(segment.spectral, expected);
    let count = mono.len() as f64;
    let peak = mono.iter().copied().map(f64::abs).fold(0.0, f64::max);
    let rms = (mono.iter().map(|sample| sample * sample).sum::<f64>() / count).sqrt();
    let clipped = mono.iter().filter(|sample| sample.abs() >= 1.0).count() as u64;
    assert!(clipped > 0);
    assert_close(segment.mono_signal.sample_peak, peak);
    assert_close(segment.mono_signal.rms, rms);
    assert_close(segment.mono_signal.crest_factor, peak / rms);
    assert_close(
        segment.mono_signal.dc_offset,
        mono.iter().sum::<f64>() / count,
    );
    assert_eq!(segment.mono_signal.clipping_count, Some(clipped));
    assert_close(segment.mono_signal.clipping_ratio, clipped as f64 / count);

    let mut all_at_once = DescriptorAnalyzer::new(&core_config(), 48_000).unwrap();
    assert_eq!(all_at_once.push(&mono), 12);
    assert_eq!(
        segment.mono_signal,
        DescriptorSignal::from_core(all_at_once.signal(), false)
    );
    // A varying spectrum ensures reading only the final core frame is wrong.
    assert_ne!(
        segment.spectral.centroid_hz.mean,
        all_at_once.spectral().unwrap().centroid_hz
    );
}

#[test]
fn one_fft_hop_large_and_irregular_chunks_are_bit_identical() {
    let mono = varying_signal(FFT_SIZE + 7 * HOP_SIZE + 911);
    let stereo: Vec<_> = mono
        .iter()
        .flat_map(|sample| [*sample, *sample * 0.5])
        .collect();
    let reference = analyze(&stereo, 2, 44_100, mono.len());
    let reference_bytes = serde_json::to_vec(&reference).unwrap();
    for chunk in [1, 7, HOP_SIZE, FFT_SIZE, FFT_SIZE + HOP_SIZE + 3] {
        assert_eq!(
            serde_json::to_vec(&analyze(&stereo, 2, 44_100, chunk)).unwrap(),
            reference_bytes
        );
    }
    let mut accumulator = DescriptorSegmentAccumulator::new(44_100, 2).unwrap();
    let mut cursor = 0;
    let mut state = 7_u64;
    while cursor < mono.len() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let end = (cursor + 1 + (state >> 32) as usize % 6000).min(mono.len());
        accumulator
            .process(&stereo[cursor * 2..end * 2], None)
            .unwrap();
        accumulator.process(&[], None).unwrap();
        cursor = end;
    }
    let irregular = accumulator.finish(DescriptorSegmentRole::Head, 0, mono.len() as u64);
    assert_eq!(serde_json::to_vec(&irregular).unwrap(), reference_bytes);
}

#[test]
fn silence_dc_and_transition_windows_keep_separate_valid_denominators() {
    let mut mono = vec![0.0; FFT_SIZE];
    mono.extend(vec![0.25; FFT_SIZE]);
    mono.extend(varying_signal(FFT_SIZE));
    let segment = analyze(&mono, 1, 48_000, mono.len());
    assert_eq!(segment.spectral, spectral_oracle(&mono, 48_000));
    assert_eq!(segment.spectral.frame_count, 9);
    assert_eq!(segment.spectral.nonfinite_frame_count, 0);
    assert_eq!(segment.spectral.centroid_hz.valid_frame_count, 7);
    assert_eq!(segment.spectral.flatness.valid_frame_count, 7);
    let dc = analyze(&vec![-0.25; FFT_SIZE * 2], 1, 48_000, 173);
    assert_eq!(dc.spectral.frame_count, 5);
    assert_eq!(dc.spectral.centroid_hz.mean, None);
    assert_eq!(dc.spectral.nonfinite_frame_count, 0);
    assert_eq!(dc.mono_signal.dc_offset, Some(-0.25));
    assert_eq!(dc.mono_signal.rms, Some(0.25));
    assert_eq!(dc.mono_signal.crest_factor, Some(1.0));
}

#[test]
fn nonfinite_window_recovery_uses_frame_positions_and_signal_stays_invalid() {
    for bad_value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut mono = varying_signal(FFT_SIZE + 7 * HOP_SIZE + 73);
        mono[HOP_SIZE + 3] = bad_value;
        let segment = analyze(&mono, 1, 48_000, mono.len());
        assert_eq!(segment.nonfinite_input_frame_count, 1);
        assert_eq!(segment.spectral.frame_count, 8);
        assert_eq!(segment.spectral.nonfinite_frame_count, 2);
        assert_eq!(segment.spectral.centroid_hz.valid_frame_count, 6);
        assert_eq!(segment.spectral, spectral_oracle(&mono, 48_000));
        assert_signal_null(&segment.mono_signal);
        assert_eq!(segment.mono_signal.sample_count, mono.len() as u64);
        let clean = analyze(&varying_signal(FFT_SIZE), 1, 48_000, FFT_SIZE);
        assert_eq!(clean.nonfinite_input_frame_count, 0);
        assert!(clean.mono_signal.rms.is_some());
        assert_eq!(clean.spectral.centroid_hz.valid_frame_count, 1);
    }
}

#[test]
fn pollution_on_publication_edges_and_unpublished_tail_is_counted_exactly() {
    let length = FFT_SIZE + 5 * HOP_SIZE + 73;
    for bad_index in [
        0,
        HOP_SIZE,
        FFT_SIZE - 1,
        FFT_SIZE,
        FFT_SIZE + HOP_SIZE - 1,
        length - 1,
    ] {
        let mut mono = varying_signal(length);
        mono[bad_index] = f64::NAN;
        let segment = analyze(&mono, 1, 96_000, mono.len());
        let expected_count = (0..=5)
            .filter(|index| (index * HOP_SIZE..index * HOP_SIZE + FFT_SIZE).contains(&bad_index))
            .count() as u64;
        assert_eq!(segment.nonfinite_input_frame_count, 1);
        assert_eq!(segment.spectral.nonfinite_frame_count, expected_count);
        assert_eq!(segment.spectral, spectral_oracle(&mono, 96_000));
        assert_signal_null(&segment.mono_signal);
        if bad_index == length - 1 {
            assert_eq!(segment.spectral.nonfinite_frame_count, 0);
            assert_eq!(segment.spectral.centroid_hz.valid_frame_count, 6);
        }
    }
}

#[test]
fn input_pollution_is_per_source_frame_including_downmix_overflow() {
    let stereo = [1.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX, f64::MAX];
    let segment = analyze(&stereo, 2, 48_000, 8);
    assert_eq!(segment.mono_signal.sample_count, 3);
    assert_eq!(segment.nonfinite_input_frame_count, 2);
    assert_signal_null(&segment.mono_signal);
    assert_eq!(segment.spectral.frame_count, 0);

    // Source-channel pollution invalidates all signal fields even if core's
    // supplied mono measurement happens to remain finite.
    let mut core = DescriptorAnalyzer::new(&core_config(), 48_000).unwrap();
    core.push(&[0.5]);
    assert_signal_null(&DescriptorSignal::from_core(core.signal(), true));
}

#[test]
fn mono_cancellation_and_ordered_f64_downmix_are_not_channel_peak_measurements() {
    let stereo: Vec<_> = (0..FFT_SIZE).flat_map(|_| [2.0, -2.0]).collect();
    let segment = analyze(&stereo, 2, 48_000, 73);
    assert_eq!(segment.mono_signal.sample_peak, Some(0.0));
    assert_eq!(segment.mono_signal.clipping_count, Some(0));
    assert_eq!(segment.spectral.flatness.mean, None);
    assert_eq!(segment.spectral.nonfinite_frame_count, 0);

    let left_to_right = analyze(&[1e16, 1.0, -1e16], 3, 48_000, 1);
    assert_eq!(left_to_right.mono_signal.dc_offset, Some(0.0));
    let different_order = analyze(&[1e16, -1e16, 1.0], 3, 48_000, 1);
    assert_eq!(different_order.mono_signal.dc_offset, Some(1.0 / 3.0));
    // An f32 intermediate would erase this difference from 1.
    let full_precision = analyze(&[1.0 + 2_f64.powi(-30), 1.0], 2, 48_000, 1);
    assert_eq!(
        full_precision.mono_signal.sample_peak,
        Some(1.0 + 2_f64.powi(-31))
    );
}

#[test]
fn finite_extremes_use_core_scaled_signal_and_are_not_nonfinite_inputs() {
    for amplitude in [f64::MAX / 2.0, 1e-300] {
        let mono: Vec<_> = (0..FFT_SIZE + HOP_SIZE)
            .map(|index| {
                if index % 2 == 0 {
                    amplitude
                } else {
                    -amplitude
                }
            })
            .collect();
        let segment = analyze(&mono, 1, 48_000, 79);
        assert_eq!(segment.nonfinite_input_frame_count, 0);
        assert_eq!(segment.spectral.nonfinite_frame_count, 0);
        assert_eq!(segment.mono_signal.sample_peak, Some(amplitude));
        assert_eq!(segment.mono_signal.rms, Some(amplitude));
        assert_eq!(segment.mono_signal.crest_factor, Some(1.0));
        assert_eq!(segment.mono_signal.dc_offset, Some(0.0));
        assert_eq!(segment.spectral, spectral_oracle(&mono, 48_000));
        assert!(segment
            .spectral
            .centroid_hz
            .mean
            .is_some_and(f64::is_finite));
        let json = serde_json::to_string(&segment).unwrap();
        assert!(!json.contains("NaN") && !json.contains("Infinity"));
    }
}

#[test]
fn nonzero_origin_short_eof_and_empty_segment_preserve_actual_coverage() {
    let mut accumulator = DescriptorSegmentAccumulator::new(48_000, 1).unwrap();
    accumulator
        .process(&varying_signal(FFT_SIZE + 71), None)
        .unwrap();
    let segment = accumulator.finish(DescriptorSegmentRole::Tail, 90_000, 100_000);
    assert_eq!(segment.end_frame, 94_167);
    assert_eq!(segment.planned_end_frame, 100_000);
    assert_eq!(segment.spectral.last_end_frame, Some(94_096));
    assert_eq!(segment.coverage_status, DescriptorCoverageStatus::ShortEof);
    let empty = DescriptorSegmentAccumulator::new(48_000, 1)
        .unwrap()
        .finish(DescriptorSegmentRole::Tail, 90_000, 100_000);
    assert_eq!(empty.end_frame, 90_000);
    assert_eq!(empty.coverage_status, DescriptorCoverageStatus::ShortEof);
    assert_eq!(empty.spectral.last_end_frame, None);
    assert_signal_null(&empty.mono_signal);
}

#[test]
fn independent_adjacent_segments_do_not_share_fft_history() {
    let input = varying_signal(FFT_SIZE * 2);
    let head = analyze(&input[..FFT_SIZE - 1], 1, 48_000, 123);
    let mut tail = DescriptorSegmentAccumulator::new(48_000, 1).unwrap();
    tail.process(&input[FFT_SIZE - 1..FFT_SIZE * 2 - 2], None)
        .unwrap();
    let tail = tail.finish(
        DescriptorSegmentRole::Tail,
        (FFT_SIZE - 1) as u64,
        (FFT_SIZE * 2 - 2) as u64,
    );
    assert_eq!(head.spectral.frame_count, 0);
    assert_eq!(tail.spectral.frame_count, 0);
    assert_eq!(head.mono_signal.sample_count, 4095);
    assert_eq!(tail.mono_signal.sample_count, 4095);
}

#[test]
fn cancellation_is_typed_and_added_loop_polls_within_one_window() {
    let mut accumulator = DescriptorSegmentAccumulator::new(48_000, 1).unwrap();
    assert!(matches!(
        accumulator.process(&[0.0], Some(&|| true)),
        Err(AutomixError::Canceled)
    ));
    assert_eq!(accumulator.analyzer.signal().sample_count, 0);
    let calls = Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        calls.get() == 2
    };
    assert!(matches!(
        accumulator.process(&vec![0.5; FFT_SIZE * 10], Some(&cancel)),
        Err(AutomixError::Canceled)
    ));
    assert_eq!(calls.get(), 2);
    assert_eq!(accumulator.analyzer.signal().sample_count, 4096);
    assert_eq!(accumulator.spectral.frame_count, 1);

    let mut partial = DescriptorSegmentAccumulator::new(48_000, 1).unwrap();
    partial.process(&[0.0; 4090], None).unwrap();
    calls.set(0);
    assert!(matches!(
        partial.process(&[0.0; 30], Some(&cancel)),
        Err(AutomixError::Canceled)
    ));
    assert_eq!(partial.analyzer.signal().sample_count, 4096);
}

#[test]
fn shared_geometry_errors_are_typed_and_do_not_advance_state() {
    assert!(matches!(
        DescriptorSegmentAccumulator::new(48_000, 0),
        Err(AutomixError::Descriptor(ProcessError::InvalidBlock(
            AudioBlockError::ZeroChannels
        )))
    ));
    assert!(matches!(
        DescriptorSegmentAccumulator::new(0, 1),
        Err(AutomixError::Descriptor(
            ProcessError::InvalidSampleRate { .. }
        ))
    ));
    let mut accumulator = DescriptorSegmentAccumulator::new(48_000, 2).unwrap();
    assert!(matches!(
        accumulator.process(&[0.5], None),
        Err(AutomixError::Descriptor(ProcessError::InvalidBlock(
            AudioBlockError::IncompleteFrame {
                samples: 1,
                channels: 2
            }
        )))
    ));
    assert_eq!(accumulator.analyzer.signal().sample_count, 0);
}

#[test]
fn schema_v1_has_exact_keys_fixed_identity_and_explicit_nulls() {
    let mut report = DescriptorReport::new(48_000, 2);
    report.segments.push(
        DescriptorSegmentAccumulator::new(48_000, 2)
            .unwrap()
            .finish(DescriptorSegmentRole::Tail, 100, 200),
    );
    let value = serde_json::to_value(report).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "schema_version": 1,
            "extractor_profile": "native-mono-l1-v1",
            "sample_rate_hz": 48000,
            "source_channels": 2,
            "config": {
                "channel_policy": "arithmetic_mean_f64",
                "fft_size": 4096,
                "hop_size": 1024,
                "window": "hann_periodic",
                "flatness_power_floor": 1e-20,
                "clipping_threshold": 1.0
            },
            "segments": [{
                "role": "tail",
                "start_frame": 100,
                "planned_end_frame": 200,
                "end_frame": 100,
                "coverage_status": "short_eof",
                "nonfinite_input_frame_count": 0,
                "mono_signal": {
                    "sample_count": 0,
                    "sample_peak": null,
                    "rms": null,
                    "crest_factor": null,
                    "dc_offset": null,
                    "clipping_count": null,
                    "clipping_ratio": null
                },
                "spectral": {
                    "frame_count": 0,
                    "nonfinite_frame_count": 0,
                    "last_end_frame": null,
                    "centroid_hz": { "mean": null, "valid_frame_count": 0 },
                    "flatness": { "mean": null, "valid_frame_count": 0 }
                }
            }]
        })
    );
    assert_eq!(
        serde_json::to_value(DescriptorSegmentRole::Head).unwrap(),
        "head"
    );
    assert_eq!(
        serde_json::to_value(DescriptorCoverageStatus::Complete).unwrap(),
        "complete"
    );
    assert_ne!(
        DescriptorReport::new(44_100, 2),
        DescriptorReport::new(48_000, 2)
    );
    assert_ne!(
        DescriptorReport::new(48_000, 1),
        DescriptorReport::new(48_000, 2)
    );
}
