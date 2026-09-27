use super::*;
use std::f64::consts::{PI, SQRT_2};

fn config(n: usize) -> DescriptorConfig {
    DescriptorConfig {
        fft_size: n,
        hop_size: n / 4,
        window: WindowFunction::Kaiser { beta: 0.0 },
        ..DescriptorConfig::default()
    }
}

fn tone(n: usize, bin: f64) -> Vec<f64> {
    (0..n)
        .map(|i| (2.0 * PI * bin * i as f64 / n as f64 + 0.123).sin())
        .collect()
}

fn analyze(config: &DescriptorConfig, samples: &[f64]) -> DescriptorAnalyzer {
    let mut analyzer = DescriptorAnalyzer::new(config, 48_000).unwrap();
    analyzer.push(samples);
    analyzer
}

fn close(actual: Option<f64>, expected: f64, tolerance: f64) {
    let actual = actual.unwrap();
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected} +/- {tolerance}"
    );
}

// Build a known power spectrum using an independent inverse complex FFT.
fn shaped_noise(n: usize, last_bin: usize, exponent: f64) -> Vec<f64> {
    let mut spectrum = vec![Complex::default(); n];
    let mut state = 0x1234_5678_u64;
    for k in 1..=last_bin {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let phase = 2.0 * PI * (state >> 11) as f64 / ((1_u64 << 53) as f64);
        spectrum[k] = Complex::from_polar((k as f64).powf(-exponent / 2.0), phase);
        spectrum[n - k] = spectrum[k].conj();
    }
    rustfft::FftPlanner::new()
        .plan_fft_inverse(n)
        .process(&mut spectrum);
    spectrum.iter().map(|x| x.re / n as f64).collect()
}

#[test]
fn tone_centroid_bandwidth_rolloff_and_flatness_match_oracles() {
    let n = 4096;
    for window in [
        WindowFunction::Hann,
        WindowFunction::BlackmanHarris4,
        WindowFunction::Kaiser { beta: 0.0 },
    ] {
        let settings = DescriptorConfig {
            window,
            ..config(n)
        };
        let samples = tone(n, 128.0);
        let analyzer = analyze(&settings, &samples);
        let result = analyzer.spectral().unwrap();
        let bin_hz = 48_000.0 / n as f64;
        close(result.centroid_hz, 1500.0, bin_hz);
        close(result.rolloff_hz, 1500.0, bin_hz);
        assert!(result.bandwidth_hz.unwrap() < 2.0 * bin_hz);
        assert!(result.flatness.unwrap() < 1e-8);
        if window == (WindowFunction::Kaiser { beta: 0.0 }) {
            close(result.bandwidth_hz, 0.0, 1e-8);
        }
    }
}

#[test]
fn brickwall_rolloff_is_a_power_quantile_and_tilt_orders_centroid() {
    let n = 4096;
    let cutoff = 512;
    let samples = shaped_noise(n, cutoff, 0.0);
    let analyzer = analyze(&config(n), &samples);
    let expected = (0.85 * cutoff as f64).ceil() * 48_000.0 / n as f64;
    close(
        analyzer.spectral().unwrap().rolloff_hz,
        expected,
        48_000.0 / n as f64,
    );
    let mut previous = 0.0;
    for exponent in [2.0, 1.0, 0.0] {
        let analyzer = analyze(&config(n), &shaped_noise(n, n / 2 - 1, exponent));
        let centroid = analyzer.spectral().unwrap().centroid_hz.unwrap();
        assert!(centroid > previous);
        previous = centroid;
    }
}

#[test]
fn equal_power_impulse_has_unit_flatness_and_zero_contrast() {
    let n = 1024;
    let mut samples = vec![0.0; n];
    samples[0] = 1.0;
    let analyzer = analyze(&config(n), &samples);
    let result = analyzer.spectral().unwrap();
    close(result.flatness, 1.0, 1e-12);
    for contrast in &result.contrast_db {
        close(*contrast, 0.0, 1e-12);
    }
    close(
        result.centroid_hz,
        (1.0 + (n / 2) as f64) / 2.0 * 48_000.0 / n as f64,
        1e-9,
    );
}

#[test]
fn white_noise_flatness_has_periodogram_bias() {
    let n = 4096;
    let mut state = 42_u64;
    let mut mean = 0.0;
    let mut analyzer = DescriptorAnalyzer::new(&config(n), 48_000).unwrap();
    for _ in 0..32 {
        let samples: Vec<_> = (0..n)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (state >> 11) as f64 / ((1_u64 << 53) as f64) * 2.0 - 1.0
            })
            .collect();
        analyzer.reset();
        analyzer.push(&samples);
        mean += analyzer.spectral().unwrap().flatness.unwrap();
    }
    close(Some(mean / 32.0), 0.56146, 0.025);
}

#[test]
fn contrast_uses_top_and_bottom_twenty_percent_power_means() {
    let n = 64;
    let mut spectrum = vec![Complex::default(); n];
    for k in 1..=10 {
        spectrum[k] = Complex::new((k as f64).sqrt(), 0.0);
        spectrum[n - k] = spectrum[k];
    }
    rustfft::FftPlanner::new()
        .plan_fft_inverse(n)
        .process(&mut spectrum);
    let samples: Vec<_> = spectrum.iter().map(|x| x.re / n as f64).collect();
    let settings = DescriptorConfig {
        contrast_band_edges_hz: vec![750.0, 7500.0],
        ..config(n)
    };
    let analyzer = analyze(&settings, &samples);
    close(
        analyzer.spectral().unwrap().contrast_db[0],
        10.0 * (19.0_f64 / 3.0).log10(),
        1e-10,
    );
    let narrow = DescriptorConfig {
        contrast_band_edges_hz: vec![751.0, 752.0],
        ..config(n)
    };
    assert_eq!(
        analyze(&narrow, &samples).spectral().unwrap().contrast_db,
        [None]
    );
}

#[test]
fn time_measurements_match_sine_square_impulse_and_dc() {
    let n = 4096;
    let samples: Vec<_> = (0..n)
        .map(|i| (2.0 * PI * 16.0 * i as f64 / n as f64).sin())
        .collect();
    let analyzer = analyze(&config(n), &samples);
    let result = analyzer.signal();
    close(result.sample_peak, 1.0, 1e-15);
    close(result.rms, 1.0 / SQRT_2, 1e-12);
    close(result.crest_factor, SQRT_2, 1e-12);
    close(result.dc_offset, 0.0, 1e-14);
    close(result.zero_crossing_rate, 32.0 / n as f64, 2.0 / n as f64);
    let square: Vec<_> = (0..n)
        .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
        .collect();
    let analyzer = analyze(&config(n), &square);
    close(analyzer.signal().crest_factor, 1.0, 0.0);
    close(analyzer.signal().zero_crossing_rate, 1.0, 0.0);
    assert_eq!(analyzer.signal().clipping_count, Some(n as u64));
    let mut impulse = vec![0.0; n];
    impulse[17] = -1.0;
    let analyzer = analyze(&config(n), &impulse);
    close(analyzer.signal().rms, 1.0 / (n as f64).sqrt(), 0.0);
    close(analyzer.signal().crest_factor, (n as f64).sqrt(), 0.0);
    assert_eq!(analyzer.signal().clipping_count, Some(1));
    close(analyzer.signal().clipping_ratio, 1.0 / n as f64, 0.0);
    close(analyzer.signal().dc_offset, -1.0 / n as f64, 0.0);
    let analyzer = analyze(&config(n), &vec![0.25; n]);
    close(analyzer.signal().dc_offset, 0.25, 0.0);
    close(analyzer.signal().zero_crossing_rate, 0.0, 0.0);
    assert!(analyzer.spectral().unwrap().centroid_hz.is_none());
}

#[test]
fn zero_boundaries_clipping_threshold_and_empty_input_are_explicit() {
    let settings = DescriptorConfig {
        clipping_threshold: 0.5,
        ..config(16)
    };
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    assert_eq!(analyzer.signal(), &SignalMeasurements::default());
    analyzer.push(&[-0.5]);
    assert_eq!(analyzer.signal().zero_crossing_rate, None);
    analyzer.push(&[0.0, 0.5, -0.5, -0.0, 0.5]);
    assert_eq!(analyzer.signal().clipping_count, Some(4));
    close(analyzer.signal().zero_crossing_rate, 1.0 / 5.0, 0.0);
    let before = *analyzer.signal();
    assert_eq!(analyzer.push(&[]), 0);
    assert_eq!(analyzer.signal(), &before);
}

#[test]
fn silence_omits_only_undefined_measurements() {
    let analyzer = analyze(&config(16), &[0.0; 16]);
    let result = analyzer.spectral().unwrap();
    assert_eq!(result.centroid_hz, None);
    assert_eq!(result.bandwidth_hz, None);
    assert_eq!(result.rolloff_hz, None);
    assert_eq!(result.flatness, None);
    assert!(result.contrast_db.iter().all(Option::is_none));
    assert_eq!(analyzer.signal().crest_factor, None);
    close(analyzer.signal().rms, 0.0, 0.0);
    close(analyzer.signal().sample_peak, 0.0, 0.0);
    close(analyzer.signal().dc_offset, 0.0, 0.0);
    assert_eq!(analyzer.signal().clipping_count, Some(0));
}

#[test]
fn sample_rate_units_nonoverlap_and_replacement_of_stale_frames() {
    let settings = DescriptorConfig {
        hop_size: 64,
        ..config(64)
    };
    let samples = tone(64, 8.0);
    for rate in [8_000, 48_000, 96_000] {
        let mut analyzer = DescriptorAnalyzer::new(&settings, rate).unwrap();
        assert_eq!(analyzer.push(&samples[..63]), 0);
        assert!(analyzer.spectral().is_none());
        assert_eq!(analyzer.push(&samples[63..]), 1);
        close(
            analyzer.spectral().unwrap().centroid_hz,
            rate as f64 / 8.0,
            1e-8,
        );
        let zcr = analyzer.signal().zero_crossing_rate;
        assert_eq!(analyzer.push(&[0.0; 63]), 0);
        assert_eq!(analyzer.spectral().unwrap().end_sample, 64);
        assert_eq!(analyzer.push(&[0.0]), 1);
        assert_eq!(analyzer.spectral().unwrap().end_sample, 128);
        assert!(analyzer.spectral().unwrap().centroid_hz.is_none());
        assert!(analyzer
            .spectral()
            .unwrap()
            .contrast_db
            .iter()
            .all(Option::is_none));
        close(zcr, 16.0 / 64.0, 1.0 / 63.0);
        analyzer.reset();
        analyzer.push(&samples);
        let fresh = {
            let mut fresh = DescriptorAnalyzer::new(&settings, rate).unwrap();
            fresh.push(&samples);
            fresh
        };
        assert_eq!(
            spectral_bits(analyzer.spectral().unwrap()),
            spectral_bits(fresh.spectral().unwrap())
        );
        assert_eq!(signal_bits(analyzer.signal()), signal_bits(fresh.signal()));
    }
}

fn spectral_bits(frame: &SpectralDescriptors) -> Vec<Option<u64>> {
    [
        frame.centroid_hz,
        frame.bandwidth_hz,
        frame.rolloff_hz,
        frame.flatness,
    ]
    .into_iter()
    .chain(frame.contrast_db.iter().copied())
    .map(|v| v.map(f64::to_bits))
    .collect()
}

fn signal_bits(signal: &SignalMeasurements) -> Vec<Option<u64>> {
    [
        signal.sample_peak,
        signal.rms,
        signal.crest_factor,
        signal.dc_offset,
        signal.clipping_ratio,
        signal.zero_crossing_rate,
    ]
    .map(|v| v.map(f64::to_bits))
    .to_vec()
}

#[test]
fn every_publication_and_signal_accumulation_are_bitwise_chunk_invariant() {
    let settings = DescriptorConfig {
        hop_size: 137,
        window: WindowFunction::Hann,
        ..config(1024)
    };
    let samples: Vec<_> = (0..8197)
        .map(|i| (i as f64 * 0.031).sin() * (i % 47) as f64 / 47.0)
        .collect();
    let expected = analyze(&settings, &samples);
    for chunk in [1, 7, 64, 1000, samples.len()] {
        let mut actual = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
        let mut reference = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
        let mut count = 0;
        for block in samples.chunks(chunk) {
            count += actual.push(block);
            for sample in block {
                reference.push(&[*sample]);
            }
            assert_eq!(
                actual.spectral().map(spectral_bits),
                reference.spectral().map(spectral_bits)
            );
            assert_eq!(
                signal_bits(actual.signal()),
                signal_bits(reference.signal())
            );
        }
        assert_eq!(
            count,
            1 + (samples.len() - settings.fft_size) / settings.hop_size
        );
        assert_eq!(
            actual.spectral().unwrap().end_sample,
            (1024 + (count - 1) * 137) as u64
        );
        assert_eq!(
            spectral_bits(actual.spectral().unwrap()),
            spectral_bits(expected.spectral().unwrap())
        );
        assert_eq!(signal_bits(actual.signal()), signal_bits(expected.signal()));
        assert_eq!(actual.signal().sample_count, samples.len() as u64);
        assert_eq!(
            actual.signal().clipping_count,
            expected.signal().clipping_count
        );
    }
}

#[test]
fn no_allocation_on_first_frame_steady_state_accessors_or_reset() {
    let samples = tone(8192, 57.0);
    let mut analyzer = DescriptorAnalyzer::new(&config(1024), 48_000).unwrap();
    for _ in 0..2 {
        assert_no_alloc::assert_no_alloc(|| {
            assert!(analyzer.push(&samples) > 0);
            std::hint::black_box(analyzer.spectral());
            std::hint::black_box(analyzer.signal());
        });
    }
    assert_no_alloc::assert_no_alloc(|| analyzer.reset());
    assert!(analyzer.spectral().is_none());
    assert_eq!(analyzer.signal(), &SignalMeasurements::default());
    analyzer.push(&samples[..1023]);
    assert!(analyzer.spectral().is_none());
    analyzer.push(&samples[1023..1024]);
    assert_eq!(analyzer.spectral().unwrap().end_sample, 1024);
}

#[test]
fn invalid_configuration_is_rejected_before_allocating() {
    fn reject(settings: &DescriptorConfig, rate: u32, class: &str) {
        assert_no_alloc::assert_no_alloc(|| {
            let error = DescriptorAnalyzer::new(settings, rate).err().unwrap();
            match class {
                "geometry" => assert!(matches!(error, ProcessError::InvalidGeometry { .. })),
                "rate" => assert!(matches!(error, ProcessError::InvalidSampleRate { .. })),
                _ => assert!(
                    matches!(error, ProcessError::InvalidParameter { parameter, .. } if parameter == class)
                ),
            }
        });
    }
    reject(&config(16), 0, "rate");
    for n in [0, 1, 2, 3, 6, 1_usize << (usize::BITS - 1)] {
        reject(&config(n), 48_000, "geometry");
    }
    for hop_size in [0, 17, usize::MAX] {
        reject(
            &DescriptorConfig {
                hop_size,
                ..config(16)
            },
            48_000,
            "geometry",
        );
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0] {
        reject(
            &DescriptorConfig {
                rolloff_fraction: value,
                ..config(16)
            },
            48_000,
            "rolloff_fraction",
        );
        reject(
            &DescriptorConfig {
                flatness_floor: value,
                ..config(16)
            },
            48_000,
            "flatness_floor",
        );
        reject(
            &DescriptorConfig {
                clipping_threshold: value,
                ..config(16)
            },
            48_000,
            "clipping_threshold",
        );
        reject(
            &DescriptorConfig {
                band_rise_floor: value,
                ..config(16)
            },
            48_000,
            "band_rise_floor",
        );
    }
    reject(
        &DescriptorConfig {
            band_edges_hz: vec![3000.0],
            ..config(16)
        },
        48_000,
        "band_edges_hz",
    );
    reject(
        &DescriptorConfig {
            rolloff_fraction: 1.01,
            ..config(16)
        },
        48_000,
        "rolloff_fraction",
    );
    for beta in [f64::NAN, f64::INFINITY, -1.0, 50.01] {
        reject(
            &DescriptorConfig {
                window: WindowFunction::Kaiser { beta },
                ..config(16)
            },
            48_000,
            "window.beta",
        );
    }
    for edges in [
        vec![3000.0],
        vec![0.0, 24000.0],
        vec![3000.0, 24001.0],
        vec![6000.0, 3000.0],
        vec![3000.0, 3000.0],
        vec![3000.0, f64::NAN],
        vec![3000.0, f64::INFINITY],
    ] {
        reject(
            &DescriptorConfig {
                contrast_band_edges_hz: edges,
                ..config(16)
            },
            48_000,
            "contrast_band_edges_hz",
        );
    }
    assert!(DescriptorAnalyzer::new(&config(4), 1).is_ok());
    assert!(DescriptorAnalyzer::new(
        &DescriptorConfig {
            hop_size: 16,
            rolloff_fraction: 1.0,
            ..config(16)
        },
        48_000
    )
    .is_ok());
}

#[test]
fn bad_input_recovers_spectral_windows_but_not_cumulative_measurements() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut analyzer = analyze(&config(16), &[bad; 16]);
        assert!(analyzer.spectral().unwrap().flatness.is_none());
        assert!(analyzer.signal().rms.is_none());
        analyzer.push(&tone(16, 3.0));
        assert!(analyzer.spectral().unwrap().centroid_hz.is_some());
        assert!(analyzer.signal().sample_peak.is_none());
        analyzer.reset();
        analyzer.push(&tone(16, 3.0));
        assert!(analyzer.signal().rms.is_some());
    }
}

#[test]
fn finite_extremes_and_floor_are_explicit() {
    for amplitude in [f64::MAX, f64::MIN_POSITIVE, 1e-300, 1e200] {
        let samples: Vec<_> = (0..16)
            .map(|i| if i % 2 == 0 { amplitude } else { -amplitude })
            .collect();
        let analyzer = analyze(&config(16), &samples);
        close(analyzer.signal().rms, amplitude, 0.0);
        close(analyzer.signal().crest_factor, 1.0, 0.0);
        close(analyzer.signal().dc_offset, 0.0, 0.0);
        close(analyzer.spectral().unwrap().centroid_hz, 24000.0, 0.0);
        assert!(analyzer.spectral().unwrap().flatness.unwrap().is_finite());
        assert!(analyzer
            .spectral()
            .unwrap()
            .contrast_db
            .iter()
            .all(Option::is_none));
    }
    let quiet: Vec<_> = tone(1024, 32.0).iter().map(|x| x * 1e-8).collect();
    let low_floor = analyze(
        &DescriptorConfig {
            flatness_floor: 1e-40,
            ..config(1024)
        },
        &quiet,
    );
    let high_floor = analyze(
        &DescriptorConfig {
            flatness_floor: 1e-10,
            ..config(1024)
        },
        &quiet,
    );
    assert!(low_floor.spectral().unwrap().flatness.unwrap() < 1e-8);
    close(high_floor.spectral().unwrap().flatness, 1.0, 0.0);
}

#[test]
fn shared_windows_match_frozen_original_formulas_bit_for_bit() {
    for n in [4, 16, 1024] {
        for window in [
            WindowFunction::Hann,
            WindowFunction::BlackmanHarris4,
            WindowFunction::Kaiser { beta: 0.0 },
            WindowFunction::Kaiser { beta: 8.6 },
            WindowFunction::Kaiser { beta: 50.0 },
        ] {
            let settings = DescriptorConfig {
                window,
                ..config(n)
            };
            let analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
            for (i, &actual) in analyzer.window.iter().enumerate() {
                let original = match window {
                    WindowFunction::Hann => 0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos()),
                    WindowFunction::BlackmanHarris4 => {
                        let x = 2.0 * PI * i as f64 / n as f64;
                        0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos()
                            - 0.01168 * (3.0 * x).cos()
                    }
                    WindowFunction::Kaiser { beta } => {
                        let x = 2.0 * i as f64 / (n - 1) as f64 - 1.0;
                        crate::dsp::modified_bessel_i0(beta * (1.0 - x * x).sqrt())
                            / crate::dsp::modified_bessel_i0(beta)
                    }
                };
                assert_eq!(actual.to_bits(), original.to_bits());
            }
        }
    }
}

#[test]
fn independent_dft_oracle_excludes_display_and_removes_dc() {
    let n = 64;
    let settings = DescriptorConfig {
        window: WindowFunction::Hann,
        ..config(n)
    };
    let samples: Vec<_> = (0..n)
        .map(|i| 0.25 + 0.001 * (i as f64 * 0.773).sin() + 0.0003 * (i as f64 * 1.113).cos())
        .collect();
    let analyzer = analyze(&settings, &samples);
    let mean = samples.iter().sum::<f64>() / n as f64;
    let power: Vec<_> = (1..=n / 2)
        .map(|k| {
            let mut value = Complex::<f64>::default();
            for (i, &x) in samples.iter().enumerate() {
                let window = 0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos());
                value += Complex::from_polar(
                    (x - mean) * window,
                    -2.0 * PI * k as f64 * i as f64 / n as f64,
                );
            }
            value.norm_sqr() / (n * n) as f64
        })
        .collect();
    let total: f64 = power.iter().sum();
    let centroid = power
        .iter()
        .enumerate()
        .map(|(i, p)| (i + 1) as f64 * 750.0 * p)
        .sum::<f64>()
        / total;
    close(analyzer.spectral().unwrap().centroid_hz, centroid, 1e-8);
    let variance = power
        .iter()
        .enumerate()
        .map(|(i, p)| (((i + 1) as f64 * 750.0) - centroid).powi(2) * p)
        .sum::<f64>()
        / total;
    close(
        analyzer.spectral().unwrap().bandwidth_hz,
        variance.sqrt(),
        1e-8,
    );
    let flatness = (power
        .iter()
        .map(|p| p.max(settings.flatness_floor).ln())
        .sum::<f64>()
        / power.len() as f64)
        .exp()
        / (power
            .iter()
            .map(|p| p.max(settings.flatness_floor))
            .sum::<f64>()
            / power.len() as f64);
    close(analyzer.spectral().unwrap().flatness, flatness, 1e-10);
    // Display controls strongly alter visual output, but none enter this oracle.
    let mut plain =
        SpectrumAnalyzer::with_config(super::super::SpectrumConfig::legacy(n, 16), 48_000).unwrap();
    let mut styled = SpectrumAnalyzer::with_config(
        super::super::SpectrumConfig::legacy(n, 16)
            .with_tilt(12.0)
            .with_db_range(-20.0, -10.0)
            .with_attack_ms(Some(500.0))
            .with_peak_hold_ms(Some(1000.0)),
        48_000,
    )
    .unwrap();
    plain.push(&samples);
    styled.push(&samples);
    assert_ne!(plain.spectrum().unwrap(), styled.spectrum().unwrap());
    let bypassed = analyze(&settings, &samples);
    assert_eq!(
        spectral_bits(analyzer.spectral().unwrap()),
        spectral_bits(bypassed.spectral().unwrap())
    );
}

#[test]
fn raw_power_and_band_measurements_follow_absolute_oracles() {
    let n = 64;
    let amplitude = 0.5;
    let samples: Vec<_> = (0..n)
        .map(|i| amplitude * (2.0 * PI * 8.0 * i as f64 / n as f64 + 0.17).sin())
        .collect();
    let settings = DescriptorConfig {
        fft_size: n,
        hop_size: n,
        window: WindowFunction::Hann,
        ..DescriptorConfig::default()
    };
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    assert!(analyzer.power_spectrum().is_none());
    assert_eq!(analyzer.push(&samples), 1);
    let power = analyzer.power_spectrum().unwrap();
    assert_eq!(power.len(), n / 2);
    assert!((power[7] - amplitude * amplitude / 16.0).abs() < 1e-14);
    assert!((power[6] - amplitude * amplitude / 64.0).abs() < 1e-14);
    assert!((power[8] - amplitude * amplitude / 64.0).abs() < 1e-14);
    let bands = analyzer.bands().unwrap();
    assert!(bands.power.iter().all(Option::is_some));
    let summed: f64 = bands.power.iter().map(|value| value.unwrap()).sum();
    let total: f64 = power.iter().sum();
    assert!((summed - total).abs() <= total * 1e-13);
    assert!(bands.rise_db.iter().all(Option::is_none));
}

#[test]
fn raw_and_bands_have_explicit_silence_and_invalid_states() {
    let settings = DescriptorConfig {
        fft_size: 16,
        hop_size: 16,
        ..DescriptorConfig::default()
    };
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    analyzer.push(&[0.0; 16]);
    assert_eq!(analyzer.power_spectrum().unwrap(), &[0.0; 8]);
    let bands = analyzer.bands().unwrap();
    assert!(bands.power.iter().all(|value| *value == Some(0.0)));
    assert!(bands.level_db.iter().all(Option::is_none));
    assert!(bands.rise_db.iter().all(Option::is_none));
    analyzer.push(&[0.0; 16]);
    assert!(analyzer
        .bands()
        .unwrap()
        .rise_db
        .iter()
        .all(|value| *value == Some(0.0)));
    analyzer.push(&[f64::NAN; 16]);
    assert!(analyzer.power_spectrum().is_none());
    assert!(analyzer.bands().unwrap().power.iter().all(Option::is_none));
}

#[test]
#[ignore = "manual host-specific cost sample, not a performance gate"]
fn descriptor_cost_sample() {
    let samples = tone(48000, 1000.0);
    let mut analyzer = DescriptorAnalyzer::new(&DescriptorConfig::default(), 48000).unwrap();
    analyzer.push(&samples);
    let start = std::time::Instant::now();
    for _ in 0..100 {
        std::hint::black_box(analyzer.push(std::hint::black_box(&samples)));
    }
    eprintln!(
        "descriptor default: {:.2} ns/input sample (100 x 48000 samples)",
        start.elapsed().as_nanos() as f64 / 4_800_000.0
    );
}

// Raw power and band oracles O1-O20 from the slice-1 research table
// (`band-energy-onset.md` section 6), plus the F4a-F4c band fixes.

const ORACLE_WINDOWS: [WindowFunction; 4] = [
    WindowFunction::Hann,
    WindowFunction::BlackmanHarris4,
    WindowFunction::Kaiser { beta: 0.0 },
    WindowFunction::Kaiser { beta: 8.6 },
];

// The frozen window formulas of
// `shared_windows_match_frozen_original_formulas_bit_for_bit`, written out.
fn window_formula(window: WindowFunction, n: usize, i: usize) -> f64 {
    match window {
        WindowFunction::Hann => 0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos()),
        WindowFunction::BlackmanHarris4 => {
            let x = 2.0 * PI * i as f64 / n as f64;
            0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos()
        }
        WindowFunction::Kaiser { beta } => {
            let x = 2.0 * i as f64 / (n - 1) as f64 - 1.0;
            crate::dsp::modified_bessel_i0(beta * (1.0 - x * x).sqrt())
                / crate::dsp::modified_bessel_i0(beta)
        }
    }
}

// `y = (x - mean(x)) * w`, computed in the time domain without rescaling.
fn windowed(frame: &[f64], window: WindowFunction) -> Vec<f64> {
    let n = frame.len();
    let mean = frame.iter().sum::<f64>() / n as f64;
    let values = frame.iter().enumerate();
    values
        .map(|(i, &x)| (x - mean) * window_formula(window, n, i))
        .collect()
}

// Independent O(N^2) DFT power `|Y[k] / N|^2` for `k = 0..=N/2`.
fn direct_power(y: &[f64]) -> Vec<f64> {
    let n = y.len();
    (0..=n / 2)
        .map(|k| {
            let mut value = Complex::<f64>::default();
            for (i, &v) in y.iter().enumerate() {
                value += Complex::from_polar(v, -2.0 * PI * ((k * i) % n) as f64 / n as f64);
            }
            value.norm_sqr() / (n * n) as f64
        })
        .collect()
}

// Multi-tone input with a DC offset and no bin-centred component.
fn multitone(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let i = i as f64;
            0.3 + 0.5 * (2.0 * PI * 0.071 * i + 0.2).sin()
                + 0.2 * (2.0 * PI * 0.23 * i).cos()
                + 0.05 * (2.0 * PI * 0.41 * i + 1.1).sin()
        })
        .collect()
}

// Bin-centred sine `amplitude * sin(2*pi*bin*i/n + phase)` over one period.
fn centred(n: usize, bin: usize, amplitude: f64, phase: f64) -> Vec<f64> {
    (0..n)
        .map(|i| amplitude * (2.0 * PI * ((bin * i) % n) as f64 / n as f64 + phase).sin())
        .collect()
}

fn frame_settings(n: usize, hop: usize, window: WindowFunction) -> DescriptorConfig {
    DescriptorConfig {
        fft_size: n,
        hop_size: hop,
        window,
        ..DescriptorConfig::default()
    }
}

fn one_frame(window: WindowFunction, samples: &[f64]) -> DescriptorAnalyzer {
    let n = samples.len();
    let mut analyzer = DescriptorAnalyzer::new(&frame_settings(n, n, window), 48_000).unwrap();
    assert_eq!(analyzer.push(samples), 1);
    analyzer
}

type BandBits = (u64, Vec<[Option<u64>; 3]>);

fn band_bits(bands: &BandMeasurements) -> BandBits {
    let bits = |value: Option<f64>| value.map(f64::to_bits);
    let fields = (0..bands.power.len())
        .map(|b| [bands.power[b], bands.level_db[b], bands.rise_db[b]].map(bits))
        .collect();
    (bands.end_sample, fields)
}

fn raw_bits(power: &[f64]) -> Vec<u64> {
    power.iter().map(|value| value.to_bits()).collect()
}

// O1: every raw bin matches an independent direct DFT of `(x - mean) * w`.
#[test]
fn o1_raw_bins_match_direct_dft_for_every_window_and_size() {
    for n in [16, 64, 1024] {
        let samples = multitone(n);
        for window in ORACLE_WINDOWS {
            let analyzer = one_frame(window, &samples);
            let actual = analyzer.power_spectrum().unwrap();
            let expected = direct_power(&windowed(&samples, window));
            let max = expected.iter().copied().fold(0.0, f64::max);
            assert_eq!(actual.len(), n / 2);
            for (k, (&a, &e)) in (1..).zip(actual.iter().zip(&expected[1..])) {
                let error = (a - e).abs() / max;
                assert!(
                    error <= 1e-11,
                    "{window:?} N={n} k={k}: {a} vs {e} ({error:e})"
                );
            }
        }
    }
}

// O2: one-sided Parseval with the unpublished DC bin from the time domain.
#[test]
fn o2_raw_bins_satisfy_one_sided_parseval() {
    for n in [16, 64, 1024] {
        let samples = mixed(n);
        for window in ORACLE_WINDOWS {
            let analyzer = one_frame(window, &samples);
            let power = analyzer.power_spectrum().unwrap();
            let y = windowed(&samples, window);
            let dc = (y.iter().sum::<f64>() / n as f64).powi(2);
            let interior: f64 = power[..n / 2 - 1].iter().sum();
            let lhs = dc + 2.0 * interior + power[n / 2 - 1];
            let rhs = y.iter().map(|v| v * v).sum::<f64>() / n as f64;
            let error = (lhs - rhs).abs() / rhs;
            assert!(
                error <= 1e-12,
                "{window:?} N={n}: {lhs} vs {rhs} ({error:e})"
            );
        }
    }
}

// Broadband noise plus the multi-tone input, so every bin carries power.
fn mixed(n: usize) -> Vec<f64> {
    let noise = shaped_noise(n, n / 2, 1.0);
    (noise.iter().zip(multitone(n)))
        .map(|(a, b)| a * n as f64 / 4.0 + b)
        .collect()
}

// O3: bin-centred tone power follows the window power `A^2 * mean(w^2) / 4`.
#[test]
fn o3_bin_centred_tone_power_follows_window_power() {
    let bh4 = 0.35875_f64.powi(2)
        + (0.48829_f64.powi(2) + 0.14128_f64.powi(2) + 0.01168_f64.powi(2)) / 2.0;
    // The table rounds this to 10 digits; the exact coefficients are used.
    assert!((bh4 - 0.2579633550).abs() < 1e-10);
    // (window, mean(w^2), k0 margin from DC and Nyquist, main-lobe half width)
    for (window, mean_square, margin, lobe) in [
        (WindowFunction::Hann, 3.0 / 8.0, 2, 1),
        (WindowFunction::BlackmanHarris4, bh4, 4, 3),
    ] {
        for n in [64, 1024] {
            for k0 in [margin, 17, n / 2 - margin] {
                for (amplitude, phase) in [(0.5, 0.0), (3.0, 0.7), (1e-3, 2.1)] {
                    let analyzer = one_frame(window, &centred(n, k0, amplitude, phase));
                    let power = analyzer.power_spectrum().unwrap();
                    let a2 = amplitude * amplitude;
                    let total: f64 = power.iter().sum();
                    let expected = a2 * mean_square / 4.0;
                    let error = (total - expected).abs() / expected;
                    assert!(error <= 1e-12, "{window:?} N={n} k0={k0}: {error:e}");
                    for (k, &p) in (1_usize..).zip(power) {
                        if k.abs_diff(k0) > lobe {
                            assert!(p <= 1e-25 * a2, "{window:?} N={n} k0={k0} k={k}: {p:e}");
                        }
                    }
                    if lobe == 1 {
                        for (k, value) in
                            [(k0 - 1, a2 / 64.0), (k0, a2 / 16.0), (k0 + 1, a2 / 64.0)]
                        {
                            let error = (power[k - 1] - value).abs() / value;
                            assert!(error <= 1e-12, "Hann N={n} k0={k0} k={k}: {error:e}");
                        }
                    }
                }
            }
        }
    }
}

// O4: a tone lands in its default band; `k0 = 64` pins the half-open rule.
#[test]
fn o4_bin_centred_tone_lands_in_its_default_band() {
    let amplitude: f64 = 0.5;
    let a2 = amplitude * amplitude;
    assert_eq!(3.0 * a2 / 32.0, 0.0234375);
    for (k0, expected) in [
        (96, [0.0, 3.0 * a2 / 32.0, 0.0, 0.0, 0.0, 0.0]),
        (64, [a2 / 64.0, 5.0 * a2 / 64.0, 0.0, 0.0, 0.0, 0.0]),
    ] {
        let analyzer = one_frame(WindowFunction::Hann, &centred(4096, k0, amplitude, 0.3));
        // Bins 1..=63, 64..=127, 128..=255, 256..=511, 512..=1023, 1024..=2048.
        let default_bins = [0..63, 63..127, 127..255, 255..511, 511..1023, 1023..2048];
        assert_eq!(analyzer.band_bins(), &default_bins);
        let bands = analyzer.bands().unwrap();
        for (b, &value) in expected.iter().enumerate() {
            let power = bands.power[b].unwrap();
            if value == 0.0 {
                assert!(power <= 1e-25 * a2, "k0={k0} band {b}: {power:e}");
                continue;
            }
            assert!((power - value).abs() <= 1e-12 * value, "k0={k0} band {b}");
            let level = bands.level_db[b].unwrap();
            assert!(
                (level - 10.0 * value.log10()).abs() <= 1e-12,
                "k0={k0} band {b}"
            );
        }
    }
}

// O5: the default bands partition bins `1..=N/2`; band powers add to the
// raw total, which is `(mean(y^2) - P[0] + P[N/2]) / 2` by Parseval.
#[test]
fn o5_default_bands_partition_the_raw_bins() {
    for n in [16, 64, 1024, 4096] {
        let samples = mixed(n);
        for window in ORACLE_WINDOWS {
            let analyzer = one_frame(window, &samples);
            let bins = analyzer.band_bins();
            assert_eq!((bins[0].start, bins[bins.len() - 1].end), (0, n / 2));
            assert!(bins.windows(2).all(|pair| pair[0].end == pair[1].start));
            let power = analyzer.power_spectrum().unwrap();
            let total: f64 = power.iter().sum();
            let bands = analyzer.bands().unwrap();
            let banded: f64 = bands.power.iter().map(|value| value.unwrap()).sum();
            let error = (banded - total).abs() / total;
            // Regrouping a sum of `N/2` non-negative terms changes rounding, so
            // the stated 1e-15 is unattainable for broadband input (observed
            // 1.32e-15, BH4 at N = 4096). Bound: first-order recursive-summation
            // error of two orders, `(N/2 - 1) * eps`. Tone frames keep 1e-15 below.
            let bound = (n / 2 - 1) as f64 * f64::EPSILON;
            assert!(error <= bound, "{window:?} N={n}: partition {error:e}");
            let y = windowed(&samples, window);
            let dc = (y.iter().sum::<f64>() / n as f64).powi(2);
            let mean_square = y.iter().map(|v| v * v).sum::<f64>() / n as f64;
            let parseval = (mean_square - dc + power[n / 2 - 1]) / 2.0;
            // This second equality is Parseval, held to the O2 tolerance.
            let error = (total - parseval).abs() / total;
            assert!(error <= 1e-12, "{window:?} N={n}: Parseval {error:e}");
        }
    }
    // Tone-dominated frames (the O4 setting) meet the stated 1e-15.
    for k0 in [64, 96, 400] {
        let analyzer = one_frame(WindowFunction::Hann, &centred(4096, k0, 0.5, 0.3));
        let total: f64 = analyzer.power_spectrum().unwrap().iter().sum();
        let bands = analyzer.bands().unwrap();
        let banded: f64 = bands.power.iter().map(|value| value.unwrap()).sum();
        assert!((banded - total).abs() <= 1e-15 * total, "k0={k0}");
    }
}

// O7: an exact step (hop = N) in band 3 rises by `10*log10((3B^2/32+eps)/eps)`;
// the table-driven reference tone and the following frame do not rise.
#[test]
fn o7_band_step_at_a_frame_boundary_rises_by_the_analytic_value() {
    let n = 4096;
    let settings = frame_settings(n, n, WindowFunction::Hann);
    let eps = settings.band_rise_floor;
    assert_eq!(eps, 1e-10);
    let amplitude: f64 = 0.25;
    let expected = 10.0 * ((3.0 * amplitude * amplitude / 32.0 + eps) / eps).log10();
    assert!((expected - 77.6785130).abs() < 5e-8, "{expected}");
    let reference = centred(n, 96, 0.5, 0.3);
    let step = centred(n, 400, amplitude, 1.1);
    let both: Vec<_> = reference.iter().zip(&step).map(|(a, b)| a + b).collect();
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    // k = 400 lies in band 3 (bins 256..=511).
    assert_eq!(analyzer.band_bins()[3], 255..511);
    assert_eq!(analyzer.push(&reference), 1);
    assert_eq!(analyzer.push(&both), 1);
    let bands = analyzer.bands().unwrap();
    for (band, rise) in bands.rise_db.iter().enumerate() {
        let rise = rise.unwrap();
        if band == 3 {
            assert!((rise - expected).abs() <= 1e-9, "{rise} vs {expected}");
        } else {
            assert!((0.0..=1e-9).contains(&rise), "band {band}: {rise}");
        }
    }
    assert_eq!(analyzer.push(&both), 1);
    for rise in &analyzer.bands().unwrap().rise_db {
        assert!((0.0..=1e-9).contains(&rise.unwrap()), "{rise:?}");
    }
}

// O8: stationary table-driven input whose period (64) divides the hop (256)
// rises by exactly `+0.0` after the first frame, which has no predecessor.
#[test]
fn o8_stationary_input_has_an_exact_zero_rise() {
    let period: Vec<f64> = (0..64)
        .map(|i| ((i * 37 % 64) as f64 / 32.0 - 1.0) * 0.7 + 0.1)
        .collect();
    let samples: Vec<_> = (0..1024 * 6).map(|i| period[i % 64]).collect();
    let settings = frame_settings(1024, 256, WindowFunction::Hann);
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    let mut frames = 0;
    for chunk in samples.chunks(256) {
        if analyzer.push(chunk) == 0 {
            continue;
        }
        frames += 1;
        let bands = analyzer.bands().unwrap();
        assert!(bands.power.iter().any(|power| power.unwrap() > 0.0));
        for rise in &bands.rise_db {
            let expected = (frames > 1).then_some(0.0_f64.to_bits());
            assert_eq!(rise.map(f64::to_bits), expected, "frame {frames}");
        }
    }
    assert_eq!(frames, 21);
}

// O9: silence publishes zero raw bins and `Some(0.0)` power, no level, and a
// zero rise after the first frame; silence -> tone gives the finite
// `10*log10((E+eps)/eps)`, and tone -> silence gives exactly `0.0`.
#[test]
fn o9_silence_and_silence_tone_transitions_are_explicit() {
    let n = 64;
    let settings = frame_settings(n, n, WindowFunction::Hann);
    let eps = settings.band_rise_floor;
    let zero = Some(0.0_f64.to_bits());
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    for frame in 0..2 {
        assert_eq!(analyzer.push(&[0.0; 64]), 1);
        let raw = raw_bits(analyzer.power_spectrum().unwrap());
        assert_eq!(raw, vec![0.0_f64.to_bits(); n / 2]);
        let bands = analyzer.bands().unwrap();
        for band in 0..bands.power.len() {
            assert_eq!(bands.power[band].map(f64::to_bits), zero);
            assert_eq!(bands.level_db[band], None);
            assert_eq!(
                bands.rise_db[band].map(f64::to_bits),
                zero.filter(|_| frame > 0)
            );
        }
    }
    assert_eq!(analyzer.push(&centred(n, 8, 0.5, 0.4)), 1);
    let bands = analyzer.bands().unwrap();
    for (power, rise) in bands.power.iter().zip(&bands.rise_db) {
        let expected = 10.0 * ((power.unwrap() + eps) / eps).log10();
        let rise = rise.unwrap();
        assert!(rise.is_finite() && (rise - expected).abs() <= 1e-9);
    }
    assert!(bands.rise_db.iter().any(|rise| rise.unwrap() > 60.0));
    assert_eq!(analyzer.push(&[0.0; 64]), 1);
    let raw = raw_bits(analyzer.power_spectrum().unwrap());
    assert_eq!(raw, vec![0.0_f64.to_bits(); n / 2]);
    for rise in &analyzer.bands().unwrap().rise_db {
        assert_eq!(rise.map(f64::to_bits), zero);
    }
}

// A loud frame after silence keeps a finite rise where the ratio
// `(E + eps) / eps` would overflow (E above about 1.8e298): a band whose power
// dwarfs the floor rises by its level plus `-10*log10(eps) = 100` dB.
#[test]
fn band_rise_after_silence_does_not_overflow() {
    let n = 64;
    let settings = frame_settings(n, n, WindowFunction::Hann);
    assert_eq!(settings.band_rise_floor, 1e-10);
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    assert_eq!(analyzer.push(&[0.0; 64]), 1);
    assert_eq!(analyzer.push(&centred(n, 8, 1e152, 0.4)), 1);
    let bands = analyzer.bands().unwrap();
    let mut huge = 0;
    for band in 0..bands.power.len() {
        let rise = bands.rise_db[band].unwrap();
        assert!(rise.is_finite() && rise >= 0.0, "band {band}: {rise}");
        if bands.power[band].unwrap() > 1e298 {
            let level = bands.level_db[band].unwrap();
            assert!(
                (rise - (level + 100.0)).abs() <= 1e-9,
                "band {band}: {rise} vs {level}"
            );
            huge += 1;
        }
    }
    assert!(huge > 0);
}

// O10: a non-finite frame has no raw bins and all-`None` bands; the next
// finite frame has no rise, the one after does, and descriptors recover.
#[test]
fn o10_non_finite_frames_break_the_rise_chain() {
    let n = 16;
    let good = mixed(n);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let settings = frame_settings(n, n, WindowFunction::Hann);
        let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
        analyzer.push(&good);
        let mut poisoned = good.clone();
        poisoned[5] = bad;
        assert_eq!(analyzer.push(&poisoned), 1);
        assert!(analyzer.power_spectrum().is_none());
        let bands = analyzer.bands().unwrap();
        assert_eq!(bands.end_sample, 32);
        let fields = || bands.power.iter().chain(&bands.level_db);
        assert!(fields().chain(&bands.rise_db).all(Option::is_none));
        assert!(analyzer.spectral().unwrap().centroid_hz.is_none());
        analyzer.push(&good);
        assert!(analyzer.power_spectrum().is_some());
        let bands = analyzer.bands().unwrap();
        assert!(bands
            .power
            .iter()
            .chain(&bands.level_db)
            .all(Option::is_some));
        assert!(bands.rise_db.iter().all(Option::is_none));
        assert!(analyzer.spectral().unwrap().centroid_hz.is_some());
        analyzer.push(&good);
        assert!(analyzer
            .bands()
            .unwrap()
            .rise_db
            .iter()
            .all(Option::is_some));
    }
}

// O11: raw bins, power, level and rise are bitwise chunk invariant after every
// publishing push, against 1-sample pushes, through silence, steps and NaN.
#[test]
fn o11_raw_and_band_outputs_are_bitwise_chunk_invariant() {
    let settings = frame_settings(1024, 137, WindowFunction::Hann);
    let samples: Vec<f64> = (0..9000_usize)
        .map(|i| match i {
            0..=1499 | 7500.. => 0.0,
            4000..=4009 => f64::NAN,
            _ => {
                let level = [0.05, 0.6, 0.2][i / 3000];
                level * ((i as f64 * 0.031).sin() + 0.5 * (i as f64 * 0.47).sin())
            }
        })
        .collect();
    let snapshot = |analyzer: &DescriptorAnalyzer| {
        let raw = analyzer.power_spectrum().map(raw_bits);
        (raw, analyzer.bands().map(band_bits))
    };
    // The fixture must reach silent, invalid and rising frames.
    let mut states = [false; 3];
    let mut probe = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    for sample in &samples {
        if probe.push(&[*sample]) > 0 {
            let bands = probe.bands().unwrap();
            states[0] |= bands.power.iter().all(|power| *power == Some(0.0));
            states[1] |= probe.power_spectrum().is_none();
            states[2] |= bands
                .rise_db
                .iter()
                .any(|rise| rise.is_some_and(|r| r > 10.0));
        }
    }
    assert_eq!(states, [true; 3]);
    for chunk in [1, 7, 64, 1000, samples.len()] {
        let mut actual = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
        let mut reference = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
        let mut publishing = 0;
        for block in samples.chunks(chunk) {
            let published = actual.push(block);
            for sample in block {
                reference.push(&[*sample]);
            }
            if published > 0 {
                publishing += 1;
                assert_eq!(snapshot(&actual), snapshot(&reference), "chunk {chunk}");
            }
        }
        assert!(publishing > 0);
        assert_eq!(snapshot(&actual), snapshot(&probe), "chunk {chunk}");
    }
}

// O12: gain `2^m` scales raw bins and band power by exactly `4^m`, shifts
// level by `20*m*log10(2)` dB, and leaves rise unchanged while `E >> eps`.
#[test]
fn o12_power_of_two_gain_scales_power_exactly() {
    // A negligible floor keeps every band far above eps at every gain.
    let settings = DescriptorConfig {
        band_rise_floor: 1e-30,
        ..frame_settings(1024, 256, WindowFunction::Hann)
    };
    let noise = shaped_noise(4096, 2048, 1.0);
    let samples: Vec<f64> = (noise.iter().enumerate())
        .map(|(i, x)| {
            let step = if i < 2048 { 1.0 } else { 3.0 };
            let tone = if i < 3000 { 0.0 } else { 0.5 };
            x * 1024.0 * step + tone * (i as f64 * 0.2).sin()
        })
        .collect();
    let frames = |gain: f64| {
        let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
        let mut frames = Vec::new();
        for chunk in samples.chunks(256) {
            let scaled: Vec<f64> = chunk.iter().map(|x| x * gain).collect();
            if analyzer.push(&scaled) > 0 {
                let raw = analyzer.power_spectrum().unwrap().to_vec();
                frames.push((raw, analyzer.bands().unwrap().clone()));
            }
        }
        frames
    };
    let base = frames(1.0);
    assert_eq!(base.len(), 13);
    for m in [-8_i32, -1, 3, 8] {
        let factor = 4.0_f64.powi(m);
        let shift = 20.0 * m as f64 * 2.0_f64.log10();
        let scaled = frames(2.0_f64.powi(m));
        assert_eq!(scaled.len(), base.len());
        for ((raw, bands), (base_raw, base_bands)) in scaled.iter().zip(&base) {
            let expected: Vec<f64> = base_raw.iter().map(|p| p * factor).collect();
            assert_eq!(raw_bits(raw), raw_bits(&expected), "m={m}");
            for b in 0..bands.power.len() {
                let power = bands.power[b].unwrap();
                assert_eq!(
                    power.to_bits(),
                    (base_bands.power[b].unwrap() * factor).to_bits()
                );
                let level = bands.level_db[b].unwrap() - base_bands.level_db[b].unwrap();
                assert!((level - shift).abs() <= 1e-12, "m={m} band {b}: {level}");
                match (bands.rise_db[b], base_bands.rise_db[b]) {
                    (Some(rise), Some(base_rise)) => assert!((rise - base_rise).abs() <= 1e-9),
                    (rise, base_rise) => assert_eq!(rise, base_rise),
                }
            }
        }
    }
}

// O13: adding a DC offset leaves every raw bin unchanged (per-bin relative).
#[test]
fn o13_dc_offset_leaves_raw_bins_unchanged() {
    let n = 1024;
    // Flat-spectrum noise with RMS near 0.5, so every bin carries power.
    let base: Vec<f64> = (shaped_noise(n, n / 2, 0.0).iter())
        .map(|x| x * 16.0)
        .collect();
    for window in ORACLE_WINDOWS {
        let reference = one_frame(window, &base).power_spectrum().unwrap().to_vec();
        for offset in [0.5, -2.0] {
            let shifted: Vec<f64> = base.iter().map(|x| x + offset).collect();
            let analyzer = one_frame(window, &shifted);
            let power = analyzer.power_spectrum().unwrap();
            for (k, (&a, &e)) in (1..).zip(power.iter().zip(&reference)) {
                let error = (a - e).abs() / e;
                assert!(error <= 1e-12, "{window:?} c={offset} k={k}: {error:e}");
            }
        }
    }
}

// O14: the same samples at 44.1/48/96 kHz give bitwise identical raw bins,
// default band ranges and band outputs; only `bin_width_hz` scales.
#[test]
fn o14_sample_rate_relabelling_changes_only_bin_width() {
    let n = 1024;
    let samples = mixed(n);
    let settings = frame_settings(n, n, WindowFunction::Hann);
    let run = |rate: u32| {
        let mut analyzer = DescriptorAnalyzer::new(&settings, rate).unwrap();
        assert_eq!(analyzer.push(&samples), 1);
        assert_eq!(analyzer.bin_width_hz(), rate as f64 / n as f64);
        let raw = raw_bits(analyzer.power_spectrum().unwrap());
        let bins = analyzer.band_bins().to_vec();
        (raw, bins, analyzer.bands().map(band_bits))
    };
    let reference = run(48_000);
    for rate in [44_100, 96_000] {
        assert_eq!(run(rate), reference, "{rate} Hz");
    }
}

// O15: finite extremes whose raw power overflows (+-MAX, +-1e200) or flushes
// (+-1e-300, +-MIN_POSITIVE) publish no raw bins and all-`None` bands; the
// scale-free descriptors match `finite_extremes_and_floor_are_explicit`.
#[test]
fn o15_unrepresentable_raw_power_publishes_none() {
    let settings = frame_settings(16, 16, WindowFunction::Kaiser { beta: 0.0 });
    for amplitude in [f64::MAX, 1e200, 1e-300, f64::MIN_POSITIVE] {
        let samples: Vec<_> = (0..16)
            .map(|i| if i % 2 == 0 { amplitude } else { -amplitude })
            .collect();
        let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
        assert_eq!(analyzer.push(&samples), 1);
        assert!(analyzer.power_spectrum().is_none(), "{amplitude:e}");
        let bands = analyzer.bands().unwrap();
        assert_eq!(bands.end_sample, 16);
        let fields = || bands.power.iter().chain(&bands.level_db);
        assert!(fields().chain(&bands.rise_db).all(Option::is_none));
        let spectral = analyzer.spectral().unwrap();
        close(spectral.centroid_hz, 24000.0, 0.0);
        assert!(spectral.flatness.unwrap().is_finite());
        assert!(spectral.contrast_db.iter().all(Option::is_none));
        // The rise chain restarts after the unrepresentable frame.
        analyzer.push(&mixed(16));
        assert!(analyzer
            .bands()
            .unwrap()
            .rise_db
            .iter()
            .all(Option::is_none));
        analyzer.push(&mixed(16));
        assert!(analyzer
            .bands()
            .unwrap()
            .rise_db
            .iter()
            .all(Option::is_some));
    }
}

// O16 (descriptor non-regression) needs no new test: these spectral-descriptor
// oracles are unchanged since 14a92b4 and must keep passing:
// `tone_centroid_bandwidth_rolloff_and_flatness_match_oracles`,
// `brickwall_rolloff_is_a_power_quantile_and_tilt_orders_centroid`,
// `equal_power_impulse_has_unit_flatness_and_zero_contrast`,
// `white_noise_flatness_has_periodogram_bias`,
// `contrast_uses_top_and_bottom_twenty_percent_power_means`,
// `silence_omits_only_undefined_measurements`,
// `sample_rate_units_nonoverlap_and_replacement_of_stale_frames`,
// `every_publication_and_signal_accumulation_are_bitwise_chunk_invariant`,
// `bad_input_recovers_spectral_windows_but_not_cumulative_measurements`,
// `finite_extremes_and_floor_are_explicit` and
// `independent_dft_oracle_excludes_display_and_removes_dc`.

// O17: an independent pipeline (written-out window, rustfft complex DFT,
// ascending band sums, level, rise) matches level and rise within 1e-9 dB on
// overlapping hops while level and timbre change.
#[test]
fn o17_independent_pipeline_matches_level_and_rise() {
    let (n, hop) = (1024, 256);
    let settings = frame_settings(n, hop, WindowFunction::Hann);
    let eps = settings.band_rise_floor;
    let noise = shaped_noise(n * 12, n * 6, 0.5);
    // Stepped envelope and sweep as in the AutoMix flux fixture, a high tone
    // switched on halfway, and broadband noise so every band carries power.
    let samples: Vec<f64> = (0..n * 12)
        .map(|i| {
            let t = i as f64 / 48_000.0;
            let envelope = 0.2 + 0.8 * ((i / (n * 3)) % 3) as f64 / 2.0;
            let sweep = 220.0 + 4000.0 * (i as f64 / (n * 12) as f64);
            let harmonic =
                (2.0 * PI * sweep * t).sin() * 0.6 + (2.0 * PI * 3.0 * sweep * t).sin() * 0.3;
            let high = if i < n * 6 {
                0.0
            } else {
                0.3 * (2.0 * PI * 9000.0 * t).sin()
            };
            envelope * harmonic + high + 30.0 * noise[i]
        })
        .collect();
    // Default bands at N = 1024: 1..=15, 16..=31, ..., 256..=512.
    let edges = [1, 16, 32, 64, 128, 256, 513];
    let fft = rustfft::FftPlanner::new().plan_fft_forward(n);
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    let mut previous: Option<Vec<f64>> = None;
    let (mut pushed, mut rising) = (0, 0);
    for end in (n..=samples.len()).step_by(hop) {
        assert_eq!(analyzer.push(&samples[pushed..end]), 1);
        pushed = end;
        let y = windowed(&samples[end - n..end], WindowFunction::Hann);
        let mut spectrum: Vec<_> = y.iter().map(|&v| Complex::new(v, 0.0)).collect();
        fft.process(&mut spectrum);
        let energy: Vec<f64> = (edges.windows(2))
            .map(|pair| {
                let bins = pair[0]..pair[1];
                bins.fold(0.0, |sum, k| sum + (spectrum[k] / n as f64).norm_sqr())
            })
            .collect();
        let bands = analyzer.bands().unwrap();
        assert_eq!(bands.end_sample, end as u64);
        for (b, &e) in energy.iter().enumerate() {
            let level = bands.level_db[b].unwrap();
            let expected = 10.0 * e.log10();
            assert!(
                (level - expected).abs() <= 1e-9,
                "end {end} band {b}: level {level} vs {expected}"
            );
            let Some(previous) = &previous else {
                assert!(bands.rise_db[b].is_none());
                continue;
            };
            let expected = (10.0 * ((e + eps) / (previous[b] + eps)).log10()).max(0.0);
            let rise = bands.rise_db[b].unwrap();
            assert!(
                rise >= 0.0 && (rise - expected).abs() <= 1e-9,
                "end {end} band {b}: rise {rise} vs {expected}"
            );
            rising += usize::from(rise > 1.0);
        }
        previous = Some(energy);
    }
    assert!(rising >= 3, "the fixture must produce several rises");
}

// O18: for a tone starting at s = 8492 after digital silence, rise is exactly
// `0.0` for every frame ending at or before s, and the first frame ending in
// `(s, s + hop]` (end_sample 9216) rises in the tone's band.
#[test]
fn o18_first_rise_is_timestamped_by_the_frame_end() {
    let (n, hop, start) = (4096, 1024, 8492);
    let settings = frame_settings(n, hop, WindowFunction::Hann);
    let samples: Vec<f64> = (0..10240_usize)
        .map(|i| {
            let phase = 2.0 * PI * 400.0 * i.saturating_sub(start) as f64 / n as f64;
            if i < start {
                0.0
            } else {
                0.5 * phase.sin()
            }
        })
        .collect();
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    // k = 400 lies in band 3.
    assert_eq!(analyzer.band_bins()[3], 255..511);
    let mut first_rise = None;
    for (i, sample) in samples.iter().enumerate() {
        if analyzer.push(&[*sample]) == 0 {
            continue;
        }
        let bands = analyzer.bands().unwrap();
        let end = bands.end_sample;
        assert_eq!(end, i as u64 + 1);
        if end == n as u64 {
            assert!(bands.rise_db.iter().all(Option::is_none));
        } else if end <= start as u64 {
            for rise in &bands.rise_db {
                assert_eq!(rise.map(f64::to_bits), Some(0.0_f64.to_bits()), "end {end}");
            }
        } else if first_rise.is_none() {
            assert!(end <= (start + hop) as u64);
            assert!(bands.rise_db[3].unwrap() > 0.0, "end {end}");
            first_rise = Some(end);
        }
    }
    assert_eq!(first_rise, Some(9216));
}

// O19: primed `push`, every accessor and `reset` allocate nothing, through
// normal, non-finite, silent and unrepresentable frames.
#[test]
fn o19_raw_and_band_paths_do_not_allocate() {
    let settings = frame_settings(1024, 256, WindowFunction::Hann);
    let mut samples = mixed(4096);
    samples[1500] = f64::NAN;
    samples[2600..3700].fill(0.0);
    let extreme: Vec<f64> = (0..1024)
        .map(|i| if i % 2 == 0 { 1e200 } else { -1e200 })
        .collect();
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    for _ in 0..2 {
        assert_no_alloc::assert_no_alloc(|| {
            assert!(analyzer.push(&samples) > 0);
            std::hint::black_box(analyzer.power_spectrum());
            std::hint::black_box(analyzer.bands());
            std::hint::black_box(analyzer.band_bins());
            std::hint::black_box(analyzer.spectral());
            std::hint::black_box(analyzer.signal());
            std::hint::black_box(analyzer.bin_width_hz());
            assert!(analyzer.push(&extreme) > 0);
            std::hint::black_box(analyzer.bands());
            analyzer.reset();
        });
    }
    assert!(analyzer.bands().is_none() && analyzer.power_spectrum().is_none());
}

// O20: invalid band edges and a non-finite or non-positive rise floor are
// rejected with `InvalidParameter` before any allocation.
#[test]
fn o20_invalid_band_parameters_are_rejected_before_allocating() {
    let reject = |settings: DescriptorConfig, parameter: &str| {
        assert_no_alloc::assert_no_alloc(|| {
            let error = DescriptorAnalyzer::new(&settings, 48_000).err().unwrap();
            assert!(
                matches!(error, ProcessError::InvalidParameter { parameter: p, .. } if p == parameter),
                "{error:?}"
            );
        });
    };
    for edges in [
        vec![3000.0],
        vec![0.0, 24000.0],
        vec![2999.0, 24000.0],
        vec![3000.0, 24001.0],
        vec![6000.0, 3000.0],
        vec![3000.0, 3000.0],
        vec![3000.0, f64::NAN],
        vec![3000.0, f64::INFINITY],
        vec![f64::NEG_INFINITY, 3000.0],
    ] {
        let settings = DescriptorConfig {
            band_edges_hz: edges,
            ..config(16)
        };
        reject(settings, "band_edges_hz");
    }
    for floor in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, -0.0, 0.0] {
        let settings = DescriptorConfig {
            band_rise_floor: floor,
            ..config(16)
        };
        reject(settings, "band_rise_floor");
    }
    // The first bin and Nyquist are inclusive edges; any positive floor works.
    for (edges, floor) in [
        (vec![3000.0, 24000.0], f64::MIN_POSITIVE),
        (Vec::new(), 1e300),
    ] {
        let settings = DescriptorConfig {
            band_edges_hz: edges,
            band_rise_floor: floor,
            ..config(16)
        };
        assert!(DescriptorAnalyzer::new(&settings, 48_000).is_ok());
    }
}

// F4a: when a later band sum overflows although every raw bin is finite, all
// bands of the frame are `None` (frame-level representability, not just the
// overflowing band), and the rise chain restarts at the next valid frame.
#[test]
fn f4a_band_sum_overflow_clears_every_band_of_the_frame() {
    // N = 16, rectangular window: bin-centred tones land in single bins.
    // Default bands are k = 1, k = 2..=3 and k = 4..=8.
    let settings = frame_settings(16, 16, WindowFunction::Kaiser { beta: 0.0 });
    // Bins 5 and 6 each hold about 1.32e308; their band-2 sum overflows.
    let samples: Vec<f64> = (0..16)
        .map(|i| {
            let phase = |k: usize| 2.0 * PI * ((k * i) % 16) as f64 / 16.0;
            1e150 * phase(1).sin() + 2.3e154 * (phase(5).sin() + phase(6).sin())
        })
        .collect();
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    assert_eq!(analyzer.band_bins(), &[0..1, 1..3, 3..8]);
    assert_eq!(analyzer.push(&samples), 1);
    let raw = analyzer.power_spectrum().unwrap();
    assert!(raw.iter().all(|p| p.is_finite()));
    assert!(raw[0] > 0.0 && !(raw[4] + raw[5]).is_finite());
    let bands = analyzer.bands().unwrap();
    assert_eq!(bands.end_sample, 16);
    let fields = || bands.power.iter().chain(&bands.level_db);
    assert!(fields().chain(&bands.rise_db).all(Option::is_none));
    assert!(analyzer.spectral().unwrap().centroid_hz.is_some());
    analyzer.push(&mixed(16));
    let bands = analyzer.bands().unwrap();
    assert!(bands.power.iter().all(Option::is_some));
    assert!(bands.rise_db.iter().all(Option::is_none));
    analyzer.push(&mixed(16));
    assert!(analyzer
        .bands()
        .unwrap()
        .rise_db
        .iter()
        .all(Option::is_some));
}

// F4b: an invalid first frame after construction or reset still publishes
// bands like `spectral()`: `Some`, every field `None`, `end_sample` set; the
// next valid frame has no rise. Warm-up and `reset` stay `None`. The FFT-error
// return shares this state path but cannot be triggered with valid buffers.
#[test]
fn f4b_invalid_first_frame_publishes_empty_bands() {
    let n = 16;
    let settings = frame_settings(n, n, WindowFunction::Hann);
    let good = mixed(n);
    let mut poisoned = good.clone();
    poisoned[3] = f64::NAN;
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    for _ in 0..2 {
        assert_eq!(analyzer.push(&poisoned[..n - 1]), 0);
        assert!(analyzer.bands().is_none() && analyzer.spectral().is_none());
        assert_eq!(analyzer.push(&poisoned[n - 1..]), 1);
        assert!(analyzer.power_spectrum().is_none());
        let bands = analyzer.bands().unwrap();
        assert_eq!((bands.end_sample, bands.power.len()), (n as u64, 3));
        let fields = || bands.power.iter().chain(&bands.level_db);
        assert!(fields().chain(&bands.rise_db).all(Option::is_none));
        assert!(analyzer.spectral().unwrap().centroid_hz.is_none());
        assert_eq!(analyzer.push(&good), 1);
        let bands = analyzer.bands().unwrap();
        assert_eq!(bands.end_sample, 2 * n as u64);
        assert!(bands
            .power
            .iter()
            .chain(&bands.level_db)
            .all(Option::is_some));
        assert!(bands.rise_db.iter().all(Option::is_none));
        analyzer.push(&good);
        assert!(analyzer
            .bands()
            .unwrap()
            .rise_db
            .iter()
            .all(Option::is_some));
        analyzer.reset();
        assert!(analyzer.bands().is_none() && analyzer.power_spectrum().is_none());
    }
}

// F4c: explicit edges that leave a band without bins make that band `None` in
// power, level and rise for every frame, including silence.
#[test]
fn f4c_empty_band_is_always_none() {
    // N = 16 at 48 kHz has 3000 Hz bins; [3100, 3200) contains no bin centre.
    let settings = DescriptorConfig {
        band_edges_hz: vec![3000.0, 3100.0, 3200.0, 24000.0],
        ..frame_settings(16, 16, WindowFunction::Hann)
    };
    let mut analyzer = DescriptorAnalyzer::new(&settings, 48_000).unwrap();
    assert_eq!(analyzer.band_bins(), &[0..1, 1..1, 1..8]);
    for frame in [mixed(16), mixed(16), vec![0.0; 16], vec![0.0; 16]] {
        assert_eq!(analyzer.push(&frame), 1);
        let bands = analyzer.bands().unwrap();
        assert_eq!(
            [bands.power[1], bands.level_db[1], bands.rise_db[1]],
            [None; 3]
        );
        assert!(bands.power[0].is_some() && bands.power[2].is_some());
    }
    assert_eq!(analyzer.bands().unwrap().rise_db[2], Some(0.0));
}

// O6: each band power is the ascending sum of its raw bins, bit for bit.
#[test]
fn o6_band_power_is_the_ascending_raw_sum() {
    for edges in [Vec::new(), vec![100.0, 1000.0, 5000.0, 20000.0]] {
        for window in ORACLE_WINDOWS {
            let settings = DescriptorConfig {
                band_edges_hz: edges.clone(),
                ..frame_settings(1024, 1024, window)
            };
            let analyzer = analyze(&settings, &mixed(1024));
            let power = analyzer.power_spectrum().unwrap();
            let bands = analyzer.bands().unwrap();
            assert_eq!(bands.power.len(), analyzer.band_bins().len());
            for (range, value) in analyzer.band_bins().iter().zip(&bands.power) {
                let sum = power[range.clone()].iter().fold(0.0, |sum, &p| sum + p);
                assert_eq!(value.unwrap().to_bits(), sum.to_bits());
            }
        }
    }
}
