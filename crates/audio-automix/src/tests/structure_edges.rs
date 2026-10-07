use super::*;

#[test]
fn continuous_pcm_sound_keeps_the_partial_final_bin_in_placement() {
    for mode in [AutomixAnalysisMode::Head, AutomixAnalysisMode::Full] {
        let analysis = analyze_spans(4.005, &[(0.0, 4.005, dbfs(-20.0))], mode);
        assert_at("fade_out", analysis.fade_out_pos, 4.005);
        if mode.includes_tail() {
            assert_at("cut_out", analysis.cut_out_pos.unwrap(), 4.005);
        } else {
            assert_eq!(analysis.cut_out_pos, None);
        }
        assert_at("mix_center", analysis.mix_center_pos, 4.005);
    }
}

#[test]
fn partial_pcm_sound_or_dead_band_prevents_false_eof_silence() {
    for partial_level in [dbfs(-20.0), dbfs(-51.0)] {
        let analysis = analyze_spans(
            4.005,
            &[(0.0, 3.0, dbfs(-20.0)), (4.0, 4.005, partial_level)],
            AutomixAnalysisMode::Full,
        );
        assert_at("fade_out", analysis.fade_out_pos, 4.005);
        assert_at("cut_out", analysis.cut_out_pos.unwrap(), 4.005);
        assert_at("mix_center", analysis.mix_center_pos, 4.005);
    }
}

#[test]
fn quiet_partial_pcm_ends_sound_at_its_own_origin() {
    let analysis = analyze_spans(4.005, &[(0.0, 4.0, dbfs(-20.0))], AutomixAnalysisMode::Full);
    assert_at("fade_out", analysis.fade_out_pos, 4.0);
    assert_at("cut_out", analysis.cut_out_pos.unwrap(), 4.0);
    assert_at("mix_center", analysis.mix_center_pos, 4.0);
}

#[test]
fn every_partial_envelope_length_preserves_rms_count_and_physical_end() {
    const FULL_FRAMES: usize = 5 * 160;
    for residual in 1..160 {
        // Binary fractions make the f64 oracle independent of f32 rounding
        // while the nonconstant tail detects use of a full-bin denominator.
        let mut samples = vec![0.125_f64; FULL_FRAMES];
        samples.extend((0..residual).map(|index| (index % 11 + 1) as f64 / 256.0));
        let expected_rms = (samples[FULL_FRAMES..]
            .iter()
            .map(|sample| sample * sample)
            .sum::<f64>()
            / residual as f64)
            .sqrt();

        for chunk_size in [samples.len(), 113] {
            let mut analyzer = SegmentAnalyzer::new(SPAN_RATE, 1);
            let mut meter = LoudnessMeter::new(1, SPAN_RATE).unwrap();
            let mut segment = AnalysisSegment::at(12.25);
            for chunk in samples.chunks(chunk_size) {
                analyzer
                    .process(chunk, &mut meter, &mut segment, None)
                    .unwrap();
            }
            assert_eq!(segment.sample_rate, SPAN_RATE);
            assert_eq!(segment.frames_analyzed, (FULL_FRAMES + residual) as u64);
            assert_eq!(meter.frames_processed(), segment.frames_analyzed);
            assert_eq!(segment.envelope, vec![0.125_f32; 5]);
            assert_eq!(segment.low_envelope.len(), 5);
            assert!(
                (f64::from(segment.partial_envelope.unwrap()) - expected_rms).abs() < 1e-8,
                "residual={residual}, chunk_size={chunk_size}"
            );
            assert_at(
                "selected end",
                segment.end_time(),
                12.25 + (FULL_FRAMES + residual) as f64 / f64::from(SPAN_RATE),
            );

            // Completing this same pending bin must clear its partial value.
            let padding = vec![0.0; 160 - residual];
            analyzer
                .process(&padding, &mut meter, &mut segment, None)
                .unwrap();
            assert_eq!(segment.frames_analyzed, (FULL_FRAMES + 160) as u64);
            assert_eq!(segment.envelope.len(), 6);
            assert_eq!(segment.partial_envelope, None);
            let expected_complete = expected_rms * (residual as f64 / 160.0).sqrt();
            assert!((f64::from(segment.envelope[5]) - expected_complete).abs() < 1e-8);
        }
    }
}
