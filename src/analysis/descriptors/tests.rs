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
