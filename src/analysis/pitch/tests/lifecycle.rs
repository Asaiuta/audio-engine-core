use super::*;

// O12/O13: zero/DC is undefined pitch but has defined raw-window RMS,
// including finite extremes and subnormals. No energy gate is applied.
#[test]
fn o12_o13_silence_dc_and_raw_rms() {
    let config = small_config();
    for level in [0.0, 1e-310, 1e-3, 0.3, 1.0, -123.456, 1e300, f64::MAX] {
        let frames = all_frames(&config, 8_000, &vec![level; 400]);
        for frame in frames {
            assert_undefined(&frame);
            assert_eq!(frame.rms, Some(level.abs()));
        }
    }
    let mut analyzer = PitchAnalyzer::new(&config, 8_000).unwrap();
    let mut samples = vec![1e300; 161];
    samples[81..].fill(1e-300);
    analyzer.push(&samples);
    assert_eq!(analyzer.frame().unwrap().rms, Some(1e-300));

    let config = o1_config();
    let mut samples = tiled_sine(64, 2049);
    let reference = all_frames(&config, 44_100, &samples).remove(0);
    for offset in [1.0, 1000.0, 1e6] {
        for (n, x) in samples.iter_mut().enumerate() {
            *x = (2.0 * PI * (n % 64) as f64 / 64.0).sin() + offset;
        }
        let frame = all_frames(&config, 44_100, &samples).remove(0);
        assert!(cents(frame.f0_hz.unwrap() / reference.f0_hz.unwrap()).abs() < 1e-6);
        // Exact tiled signals have zero aperiodicity; absolute roundoff is
        // the meaningful comparison at zero (relative error is undefined).
        assert!((frame.aperiodicity.unwrap() - reference.aperiodicity.unwrap()).abs() < 1e-12);
        assert!((frame.rms.unwrap() / (offset * offset + 0.5).sqrt() - 1.0).abs() < 1e-14);
    }
    // Also test nonzero aperiodicity, where O13's relative comparison is
    // defined. The independent deterministic noise prevents a zero trough.
    let mut state = 41;
    let samples: Vec<_> = (0..2049)
        .map(|n| (2.0 * PI * 233.13 * n as f64 / 44_100.0).sin() + 0.1 * lcg_uniform(&mut state))
        .collect();
    let reference = all_frames(&config, 44_100, &samples).remove(0);
    for offset in [1.0, 1000.0, 1e6] {
        let shifted: Vec<_> = samples.iter().map(|x| x + offset).collect();
        let actual = all_frames(&config, 44_100, &shifted).remove(0);
        assert!(cents(actual.f0_hz.unwrap() / reference.f0_hz.unwrap()).abs() < 1e-6);
        assert!(
            (actual.aperiodicity.unwrap() / reference.aperiodicity.unwrap() - 1.0).abs() < 1e-8
        );
    }
}

// O14: every and only buffer containing NaN/Inf is invalid, even when the
// bad sample is older than the RMS integration window; recovery is automatic.
#[test]
fn o14_contamination_and_reset_rearm_exactly() {
    let config = small_config();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut samples = sine(250.0, 8_000, 800);
        samples[320] = bad;
        for frame in all_frames(&config, 8_000, &samples) {
            let end = frame.end_sample as usize;
            if end > 320 && end - 161 <= 320 {
                assert_undefined(&frame);
                assert_eq!(frame.rms, None);
            } else {
                assert!(frame.voiced);
            }
        }
        let mut analyzer = PitchAnalyzer::new(&config, 8_000).unwrap();
        analyzer.push(&samples);
        analyzer.reset();
        assert!(analyzer.frame().is_none());
        assert!(analyzer.reference_sample().is_none());
        assert_eq!(analyzer.push(&samples[..160]), 0);
        assert_eq!(analyzer.push(&samples[160..161]), 1);
        let fresh = all_frames(&config, 8_000, &samples[..161]).remove(0);
        assert_eq!(analyzer.frame(), Some(&fresh));
    }
}

fn assert_pitch_bits(left: &PitchFrame, right: &PitchFrame) {
    assert_eq!(frame_bits(left), frame_bits(right));
    assert_eq!(
        left.voicing_probability.map(f64::to_bits),
        right.voicing_probability.map(f64::to_bits)
    );
    assert_eq!(left.candidates, right.candidates);
}

// O15a: exact covariance for powers of two; bounded arithmetic error for
// arbitrary gains. RMS alone scales rather than staying invariant.
#[test]
fn o15a_gain_covariance() {
    let config = small_config();
    let samples = sine(237.0, 8_000, 600);
    let reference = all_frames(&config, 8_000, &samples);
    for exponent in [-500, -20, 20, 500] {
        let gain = 2.0_f64.powi(exponent);
        let frames = all_frames(
            &config,
            8_000,
            &samples.iter().map(|x| x * gain).collect::<Vec<_>>(),
        );
        for (actual, expected) in frames.iter().zip(&reference) {
            assert_pitch_bits(actual, expected);
            assert_eq!(
                actual.rms.unwrap().to_bits(),
                (expected.rms.unwrap() * gain).to_bits()
            );
        }
    }
    for gain in [0.3, 1e5, 1e-200] {
        let frames = all_frames(
            &config,
            8_000,
            &samples.iter().map(|x| x * gain).collect::<Vec<_>>(),
        );
        for (actual, expected) in frames.iter().zip(&reference) {
            assert!(
                (actual.period_samples.unwrap() / expected.period_samples.unwrap() - 1.0).abs()
                    < 1e-15
            );
            assert!(
                actual.aperiodicity == expected.aperiodicity
                    || (actual.aperiodicity.unwrap() / expected.aperiodicity.unwrap() - 1.0).abs()
                        < 1e-9,
                "gain={gain}, actual={:?}, expected={:?}",
                actual.aperiodicity,
                expected.aperiodicity
            );
        }
    }
}

// O15b: changing frame scales must not contaminate later uniform-gain
// windows. Transition frames are measured but are allowed to lose voicing.
#[test]
fn o15b_gain_steps_recover_identical_pitch() {
    let config = small_config();
    let samples = sine(250.0, 8_000, 3200);
    let reference = all_frames(&config, 8_000, &samples);
    let changed: Vec<_> = samples
        .iter()
        .enumerate()
        .map(|(i, x)| {
            x * if (i / 800) % 2 == 0 {
                1.0
            } else {
                2.0_f64.powi(-20)
            }
        })
        .collect();
    let frames = all_frames(&config, 8_000, &changed);
    let mut compared = 0;
    for (actual, expected) in frames.iter().zip(&reference) {
        let end = actual.end_sample as usize;
        if (end - 161) / 800 == (end - 1) / 800 {
            assert_pitch_bits(actual, expected);
            compared += 1;
        }
    }
    assert!(compared > 100);
}

// O16: sample-rate relabeling changes only Hz, never lag, depth, probability,
// RMS or the reference sample clock.
#[test]
fn o16_sample_rate_relabeling() {
    let config = small_config();
    let samples = sine(237.0, 8_000, 800);
    let reference = all_frames(&config, 8_000, &samples);
    for factor in [0.5, 2.0] {
        let mut relabeled = config.clone();
        relabeled.fmin_hz *= factor;
        relabeled.fmax_hz *= factor;
        let frames = all_frames(&relabeled, (8_000.0 * factor) as u32, &samples);
        for (actual, expected) in frames.iter().zip(&reference) {
            let mut scaled = expected.clone();
            scaled.f0_hz = scaled.f0_hz.map(|f| f * factor);
            for candidate in &mut scaled.candidates {
                candidate.f0_hz *= factor;
            }
            assert_eq!(actual, &scaled);
        }
    }
}

// O17: whole-block and irregular pushes agree bitwise at every observable
// frame and publish the exact unpadded sample clock; partial tails stay stale.
#[test]
fn o17_chunk_invariance_and_clock() {
    let config = small_config();
    let samples = sine(237.0, 8_000, 2003);
    let reference = all_frames(&config, 8_000, &samples);
    for chunk in [1, 7, 64, 1000, samples.len()] {
        let mut analyzer = PitchAnalyzer::new(&config, 8_000).unwrap();
        let mut count = 0;
        for (i, block) in samples.chunks(chunk).enumerate() {
            count += analyzer.push(block);
            let consumed = ((i + 1) * chunk).min(samples.len());
            let expected = reference
                .iter()
                .rev()
                .find(|frame| frame.end_sample <= consumed as u64);
            assert_eq!(analyzer.frame(), expected);
            if let Some(frame) = expected {
                assert_pitch_bits(analyzer.frame().unwrap(), frame);
                assert_eq!(
                    analyzer.reference_sample(),
                    Some(frame.end_sample as f64 - (80.0 + frame.period_samples.unwrap()) / 2.0)
                );
            }
        }
        assert_eq!(count, 1 + (samples.len() - 161) / 16);
        assert_eq!(analyzer.push(&[]), 0);
        assert_eq!(analyzer.integration_samples(), 80);
    }
}

// O18: construction owns allocation, including candidate capacity; even the
// first frame, invalid-frame recovery, accessors and reset cannot allocate.
#[test]
fn o18_no_allocation_first_steady_invalid_and_reset() {
    let mut analyzer = PitchAnalyzer::new(&small_config(), 8_000).unwrap();
    let samples = sine(237.0, 8_000, 800);
    assert_no_alloc::assert_no_alloc(|| {
        assert_eq!(analyzer.push(&samples[..161]), 1);
        analyzer.push(&samples);
        std::hint::black_box(analyzer.frame());
        std::hint::black_box(analyzer.buffer_samples());
        std::hint::black_box(analyzer.sample_rate_hz());
        std::hint::black_box(analyzer.integration_samples());
        std::hint::black_box(analyzer.reference_sample());
        analyzer.push(&[f64::NAN; 200]);
        analyzer.push(&samples);
        analyzer.reset();
        analyzer.push(&samples);
    });
}

// O18: each parameter/geometry failure is returned before any allocation.
#[test]
fn o18_errors_before_allocation() {
    let base = small_config();
    let check = |config: &PitchConfig, rate, parameter: Option<&str>| {
        assert_no_alloc::assert_no_alloc(|| {
            let result = PitchAnalyzer::new(config, rate);
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("expected invalid config"),
            };
            match (parameter, error) {
                (Some(expected), ProcessError::InvalidParameter { parameter, .. }) => {
                    assert_eq!(parameter, expected)
                }
                (
                    None,
                    ProcessError::InvalidGeometry { .. } | ProcessError::InvalidSampleRate { .. },
                ) => {}
                (_, error) => panic!("unexpected {error:?}"),
            }
        });
    };
    check(&base, 0, None);
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        check(
            &PitchConfig {
                fmin_hz: bad,
                ..base.clone()
            },
            8_000,
            Some("fmin_hz"),
        );
        check(
            &PitchConfig {
                prior_alpha: bad,
                ..base.clone()
            },
            8_000,
            Some("prior_alpha"),
        );
        check(
            &PitchConfig {
                prior_beta: bad,
                ..base.clone()
            },
            8_000,
            Some("prior_beta"),
        );
        check(
            &PitchConfig {
                threshold: bad,
                ..base.clone()
            },
            8_000,
            Some("threshold"),
        );
    }
    for bad in [99.0, 100.0, 4000.0, 8000.0, f64::NAN, f64::INFINITY] {
        check(
            &PitchConfig {
                fmax_hz: bad,
                ..base.clone()
            },
            8_000,
            Some("fmax_hz"),
        );
    }
    check(
        &PitchConfig {
            fmin_hz: 1000.0,
            fmax_hz: 1001.0,
            ..base.clone()
        },
        8_000,
        Some("fmax_hz"),
    );
    for hop in [0, 162, usize::MAX] {
        check(
            &PitchConfig {
                hop_size: hop,
                ..base.clone()
            },
            8_000,
            None,
        );
    }
    for bad in [1.01, f64::MAX] {
        check(
            &PitchConfig {
                threshold: bad,
                ..base.clone()
            },
            8_000,
            Some("threshold"),
        );
    }
    for bad in [-0.01, 1.01, f64::NAN, f64::INFINITY] {
        check(
            &PitchConfig {
                absolute_min_weight: bad,
                ..base.clone()
            },
            8_000,
            Some("absolute_min_weight"),
        );
    }
    check(
        &PitchConfig {
            fmin_hz: f64::MIN_POSITIVE,
            ..base.clone()
        },
        8_000,
        None,
    );
    check(
        &PitchConfig {
            prior_alpha: f64::MAX,
            prior_beta: f64::MAX,
            ..base
        },
        8_000,
        Some("prior"),
    );
}
