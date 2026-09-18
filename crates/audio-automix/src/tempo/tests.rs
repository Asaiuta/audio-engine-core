use super::*;
use audio_engine_core::decoder::DecodeCancelToken;

#[test]
fn fft_correlations_match_direct_overlap_normalization_and_reuse() {
    for length in [73, 401, 6_013] {
        let max_lag = 659.min(length - 1);
        let mut workspace = Autocorrelation::new(length, max_lag);
        for shape in 0..4 {
            let values: Vec<_> = (0..length)
                .map(|i| match shape {
                    0 => 0.0,
                    1 => 1.0 + 0.7 * (i as f64 * 0.127).sin(),
                    2 => {
                        if i == 0 {
                            1e6
                        } else {
                            1e-4 * (1.0 + (i as f64).sin())
                        }
                    }
                    _ => {
                        let phase = (i as f64 % 91.37) - 45.0;
                        (-0.5 * (phase / 2.3).powi(2)).exp()
                            + 0.13 * (i as f64 * 1.713).sin().max(0.0)
                    }
                })
                .collect();
            let expected = direct_autocorrelation(&values, max_lag, None).unwrap();
            let actual = workspace.compute(&values, max_lag, None).unwrap();
            for (lag, (&expected, &actual)) in expected.iter().zip(&actual).enumerate() {
                assert!(
                    (actual - expected).abs() < 3e-10,
                    "length={length}, shape={shape}, lag={lag}: {actual} != {expected}"
                );
            }
        }
    }
}

#[test]
fn short_accented_pulses_use_the_level_with_enough_observed_beats() {
    let rate = 200.0;
    let period = 1.0 / 3.0;
    let mut onsets = vec![0.0; 600];
    for beat in 0..9 {
        let center = (0.217 + beat as f64 * period) * rate;
        let amplitude = if beat % 2 == 0 { 1.0 } else { 0.5 };
        for (index, onset) in onsets.iter_mut().enumerate() {
            *onset += (amplitude * (-0.5 * ((index as f64 - center) / 1.6).powi(2)).exp()) as f32;
        }
    }
    let result = estimate(&onsets, rate, 0.0, None).unwrap();
    let bpm = result.bpm().expect("nine pulses can support a fitted grid");
    assert!((bpm - 180.0).abs() <= 0.05, "{result:?}");
    let grid = result.grid.unwrap();
    assert!((grid.first_beat_sec - 0.217).abs() <= 0.010);
}

#[test]
fn strong_subdivisions_cannot_promote_a_weaker_unrelated_period() {
    let rate = 200.0;
    let bpm = 119.18;
    let period = 60.0 / bpm;
    // Strong sixteenth notes accompany the beat. A weaker independent
    // pulse train is a distractor, not an octave of the dominant rhythm.
    let onsets: Vec<f32> = (0..6_000)
        .map(|index| {
            let time = index as f64 / rate;
            let pulse = |period: f64, offset: f64| {
                let phase = (time - offset + period / 2.0).rem_euclid(period) - period / 2.0;
                (-0.5 * (phase / 0.008).powi(2)).exp()
            };
            (pulse(period, 0.217) + pulse(period / 4.0, 0.217) + 0.6 * pulse(period * 1.25, 0.297))
                as f32
        })
        .collect();
    let estimate = estimate(&onsets, rate, 0.0, None).unwrap();
    let detected = estimate
        .bpm()
        .expect("dominant periodic onsets support a grid");
    assert!(
        (detected - bpm).abs() <= 0.05,
        "expected {bpm}, got {detected}"
    );
    let grid = estimate.grid.unwrap();
    let phase_error =
        (grid.first_beat_sec - 0.217 + period / 2.0).rem_euclid(period) - period / 2.0;
    assert!(phase_error.abs() <= 0.010);
    let final_error = phase_error + (30.0 / period).floor() * (grid.period_sec - period);
    assert!(final_error.abs() <= 0.020);
}

#[test]
fn channel_fusion_keeps_identical_views_equivalent() {
    let rate = 200.0;
    let period = 60.0 / 120.0;
    let values: Vec<f32> = (0..6_000)
        .map(|index| {
            let time = index as f64 / rate;
            let phase = (time - 0.137 + period / 2.0).rem_euclid(period) - period / 2.0;
            (-0.5 * (phase / 0.008).powi(2)).exp() as f32
        })
        .collect();
    let channels = [values.clone(), values.clone(), values];
    let control = estimate(&channels[0], rate, 0.0, None).unwrap();
    let fused = estimate_with_channels(&channels[0], Some(&channels), rate, 0.0, None).unwrap();
    assert_eq!(fused.bpm(), control.bpm());
    assert_eq!(fused.confidence, control.confidence);
    assert_eq!(
        fused.grid.map(|grid| grid.period_sec),
        control.grid.map(|grid| grid.period_sec)
    );
}

#[test]
fn channel_fusion_requires_margin_advantage_and_period_agreement() {
    let estimate = TempoEstimate {
        grid: Some(BeatGrid {
            period_sec: 100.0,
            first_beat_sec: 0.0,
            stability: 0.9,
        }),
        confidence: Some(0.9),
    };
    let all_band = RankedEstimate {
        candidate: TempoCandidate {
            period: 100.0,
            score: 0.5,
            salience: 0.8,
        },
        estimate,
        margin: 0.10,
    };
    let compatible = RankedEstimate {
        candidate: TempoCandidate {
            period: 200.5,
            score: 0.5,
            salience: 0.8,
        },
        estimate,
        margin: 0.16,
    };
    assert!(should_use_channel_mean(all_band, compatible));

    let weak_margin = RankedEstimate {
        margin: 0.14,
        ..compatible
    };
    assert!(!should_use_channel_mean(all_band, weak_margin));

    let incompatible = RankedEstimate {
        candidate: TempoCandidate {
            period: 130.0,
            ..compatible.candidate
        },
        ..compatible
    };
    assert!(!should_use_channel_mean(all_band, incompatible));
}

#[test]
fn slow_grid_cannot_hide_absolute_onset_jitter() {
    let rate = 200.0;
    for bpm in [60.0, 70.0, 90.0, 127.3] {
        let period = 60.0 / bpm;
        let mut onsets = vec![0.0; 12_000];
        for beat in 0..(60.0 / period) as usize {
            let time = 0.217 + beat as f64 * period + 0.065 * (beat as f64 * 1.713).sin();
            let center = time * rate;
            let start = (center - 8.0).max(0.0) as usize;
            let end = ((center + 9.0) as usize).min(onsets.len());
            for (offset, onset) in onsets[start..end].iter_mut().enumerate() {
                *onset +=
                    (-0.5 * ((start as f64 + offset as f64 - center) / 1.6).powi(2)).exp() as f32;
            }
        }
        let result = estimate(&onsets, rate, 0.0, None).unwrap();
        assert!(result.usable_grid().is_none(), "{bpm}: {result:?}");
        assert!(
            result.grid.is_none_or(|grid| grid.stability < 0.80),
            "{bpm}: {result:?}"
        );
    }
}

#[test]
fn large_tempo_search_observes_cancellation() {
    let onsets = vec![1.0; 1_000_000];
    let token = DecodeCancelToken::new();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(5));
            token.cancel();
        });
        assert!(matches!(
            Autocorrelation::new(onsets.len(), 1_000).compute(
                &onsets,
                1_000,
                Some(&|| token.is_cancelled())
            ),
            Err(AutomixError::Canceled)
        ));
    });
}
