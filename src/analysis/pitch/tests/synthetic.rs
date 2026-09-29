use super::*;

fn harmonic(frequency: f64, rate: u32, len: usize, phase: f64, harmonics: usize) -> Vec<f64> {
    (0..len)
        .map(|n| {
            let angle = 2.0 * PI * frequency * n as f64 / f64::from(rate) + phase;
            (1..=harmonics)
                .map(|k| (k as f64 * angle).sin() / k as f64)
                .sum()
        })
        .collect()
}

// O3/O4: full 50-frequency x 3-phase x 2-rate sweep for a pure sine and
// band-limited sawtooth complexes (harmonics below fs/4 and fs/2).
#[test]
fn o3_o4_sine_and_harmonic_sweeps() {
    let config = PitchConfig::default();
    let mut worst = [0.0_f64; 3];
    let mut state = 187_u64;
    for rate in [44_100, 48_000] {
        let n = PitchAnalyzer::new(&config, rate).unwrap().buffer_samples();
        for index in 0..50 {
            let lower = 1.02 * config.fmin_hz;
            let upper = 0.98 * config.fmax_hz;
            let frequency = lower * (upper / lower).powf(index as f64 / 49.0);
            for _ in 0..3 {
                let phase = PI * lcg_uniform(&mut state);
                for (kind, cutoff) in [0.0, 0.25, 0.5].iter().enumerate() {
                    let harmonics = if kind == 0 {
                        1
                    } else {
                        (f64::from(rate) * cutoff / frequency).floor() as usize
                    };
                    let samples = harmonic(frequency, rate, n + 512, phase, harmonics);
                    for frame in all_frames(&config, rate, &samples) {
                        let error = cents(frame.f0_hz.unwrap() / frequency).abs();
                        worst[kind] = worst[kind].max(error);
                        let bound = [0.25, 1.5, 5.0][kind];
                        assert!(
                            error <= bound,
                            "kind={kind}, fs={rate}, f={frequency}, cents={error}"
                        );
                        assert!(frame.voiced);
                        assert!(frame.aperiodicity.unwrap() <= [1e-3, 0.05, 0.05][kind]);
                        assert!(frame.voicing_probability.is_some());
                        assert!(frame.candidates.len() <= 101);
                    }
                }
            }
        }
    }
    eprintln!("O3/O4 worst cents: {worst:?}");
}

// O5: missing fundamental with relatively prime harmonics; even-only
// harmonics correctly report the actual doubled periodicity.
#[test]
fn o5_missing_fundamental_and_even_only() {
    let config = o1_config();
    let mut state = 51_u64;
    let mut worst = 0.0_f64;
    for frequency in [100.0, 150.0, 220.0, 331.0] {
        for ks in [
            &[2, 3, 4, 5, 6][..],
            &[3, 4, 5],
            &[2, 3],
            &[4, 5, 6, 7],
            &[2, 4, 6],
        ] {
            let phases: Vec<_> = ks.iter().map(|_| PI * lcg_uniform(&mut state)).collect();
            let samples: Vec<_> = (0..2561)
                .map(|n| {
                    ks.iter()
                        .zip(&phases)
                        .map(|(&k, &phase)| {
                            (2.0 * PI * frequency * k as f64 * n as f64 / 44_100.0 + phase).sin()
                        })
                        .sum()
                })
                .collect();
            let expected = frequency * if ks == [2, 4, 6] { 2.0 } else { 1.0 };
            for frame in all_frames(&config, 44_100, &samples) {
                let error = cents(frame.f0_hz.unwrap() / expected).abs();
                worst = worst.max(error);
                assert!(error < 0.1, "{frequency}, {ks:?}: {error}");
                assert!(frame.voiced);
            }
        }
    }
    eprintln!("O5 worst cents: {worst:e}");
}

// O6/O7: deliberately pin the threshold's octave traps, not an imaginary
// guarantee of recovering a weak fundamental under all mixtures.
#[test]
fn o6_o7_octave_traps_and_depth_formula() {
    let config = o1_config();
    for epsilon in [0.05, 0.1, 0.5, 1.0] {
        let samples: Vec<_> = (0..2049)
            .map(|n| {
                let angle = 2.0 * PI * 220.0 * n as f64 / 44_100.0;
                epsilon * angle.sin() + (2.0 * angle).sin()
            })
            .collect();
        let mut analyzer = PitchAnalyzer::new(&config, 44_100).unwrap();
        analyzer.push(&samples);
        let frame = analyzer.frame().unwrap();
        let expected = if epsilon < 0.2 { 440.0 } else { 220.0 };
        assert!(cents(frame.f0_hz.unwrap() / expected).abs() < 50.0);
        let lag = (44_100.0_f64 / 440.0).round() as usize;
        // This approximation is for d'(T/2), not the vertex of a trough:
        // at epsilon=1 the half-period need not even be a local minimum.
        let depth = analyzer.normalized[lag];
        let formula = 2.0 * epsilon * epsilon / (1.0 + epsilon * epsilon);
        assert!(
            (depth / formula - 1.0).abs() < 0.03,
            "eps={epsilon}: {depth}/{formula}"
        );
    }
    for epsilon in [0.1, 0.5] {
        let samples: Vec<_> = (0..2561)
            .map(|n| {
                let angle = 2.0 * PI * 220.0 * n as f64 / 44_100.0;
                angle.sin() + epsilon * (0.5 * angle).sin()
            })
            .collect();
        let expected = if epsilon < 0.2 { 220.0 } else { 110.0 };
        for frame in all_frames(&config, 44_100, &samples) {
            assert!(cents(frame.f0_hz.unwrap() / expected).abs() < 25.0);
        }
    }
}

fn moving_signal(
    rate: u32,
    seconds: f64,
    harmonics: usize,
    frequency: impl Fn(f64) -> f64,
) -> Vec<f64> {
    let mut phase = 0.0;
    (0..(f64::from(rate) * seconds) as usize)
        .map(|n| {
            let sample = (1..=harmonics)
                .map(|k| (phase * k as f64).sin() / k as f64)
                .sum();
            // Midpoint integration gives the phase at the next sample position.
            phase += 2.0 * PI * frequency((n as f64 + 0.5) / f64::from(rate)) / f64::from(rate);
            sample
        })
        .collect()
}

// O8/O9: compare against the frequency at the end-anchored integration
// reference, not at the buffer end or a modulo-period clock.
#[test]
fn o8_vibrato_reference_time() {
    let config = PitchConfig::default();
    let rate = 44_100;
    let window = PitchAnalyzer::new(&config, rate)
        .unwrap()
        .integration_samples() as f64;
    let mut worst = [0.0_f64; 4];
    for (case, (f0, amplitude, speed)) in [(220.0, 50.0, 5.5), (440.0, 100.0, 6.5)]
        .into_iter()
        .enumerate()
    {
        let frequency =
            |t: f64| f0 * 2.0_f64.powf(amplitude / 1200.0 * (2.0 * PI * speed * t).sin());
        for (kind, harmonics) in [1, 8].into_iter().enumerate() {
            let index = case * 2 + kind;
            let samples = moving_signal(rate, 1.0, harmonics, frequency);
            for frame in all_frames(&config, rate, &samples) {
                let t = (frame.end_sample as f64 - (window + frame.period_samples.unwrap()) / 2.0)
                    / f64::from(rate);
                let error = cents(frame.f0_hz.unwrap() / frequency(t)).abs();
                worst[index] = worst[index].max(error);
                assert!(
                    error <= [2.0, 5.0, 5.0, 6.0][index],
                    "vibrato {index}: {error}"
                );
                assert!(frame.voiced);
            }
        }
    }
    eprintln!("O8 worst cents: {worst:?}");
}

#[test]
fn o9_glides_reference_time_and_aperiodicity() {
    let config = PitchConfig::default();
    let rate = 44_100;
    let window = PitchAnalyzer::new(&config, rate)
        .unwrap()
        .integration_samples() as f64;
    let mut worst = [0.0_f64; 6];
    for (case, (start, slope)) in [(110.0, 1.0), (110.0, 4.0), (1760.0, -4.0)]
        .into_iter()
        .enumerate()
    {
        let frequency = |t: f64| start * 2.0_f64.powf(slope * t);
        for (kind, harmonics) in [1, 8].into_iter().enumerate() {
            let samples = moving_signal(rate, 0.9, harmonics, frequency);
            for frame in all_frames(&config, rate, &samples) {
                let t = (frame.end_sample as f64 - (window + frame.period_samples.unwrap()) / 2.0)
                    / f64::from(rate);
                let error = cents(frame.f0_hz.unwrap() / frequency(t)).abs();
                worst[case * 2 + kind] = worst[case * 2 + kind].max(error);
                let bound = if case == 0 {
                    [1.5, 6.0][kind]
                } else {
                    [5.0, 20.0][kind]
                };
                assert!(error <= bound, "glide {case}/{kind}: {error}");
                if harmonics == 1 {
                    let predicted =
                        PI * PI / 6.0 * (slope * 2.0_f64.ln() * window / f64::from(rate)).powi(2);
                    assert!(frame.aperiodicity.unwrap() <= 1.5 * predicted);
                }
            }
        }
    }
    eprintln!("O9 worst cents: {worst:?}");
}

fn gaussian(state: &mut u64) -> f64 {
    let u = ((lcg_uniform(state) + 1.0) * 0.5).max(f64::MIN_POSITIVE);
    let v = (lcg_uniform(state) + 1.0) * 0.5;
    (-2.0 * u.ln()).sqrt() * (2.0 * PI * v).cos()
}

// O10: seeded normal white noise, all 300 frames at both sample rates.
#[test]
fn o10_white_noise_is_unvoiced() {
    let config = PitchConfig::default();
    let mut state = 12345;
    let mut minimum = 1.0_f64;
    let mut maximum = 0.0_f64;
    for rate in [44_100, 48_000] {
        let n = PitchAnalyzer::new(&config, rate).unwrap().buffer_samples();
        let samples: Vec<_> = (0..n + 299 * 256).map(|_| gaussian(&mut state)).collect();
        let frames = all_frames(&config, rate, &samples);
        assert_eq!(frames.len(), 300);
        for frame in frames {
            assert!(!frame.voiced);
            minimum = minimum.min(frame.aperiodicity.unwrap());
            maximum = maximum.max(frame.voicing_probability.unwrap());
            assert!(frame.aperiodicity.unwrap() >= 0.5);
            assert!(frame.voicing_probability.unwrap() <= 0.0101);
        }
    }
    eprintln!("O10 minimum aperiodicity={minimum}, maximum probability={maximum}");
}

// O11: the noise/signal ratio predicts aperiodicity. Measure the finite
// sample distribution as well as the mean, with explicit frame denominators.
#[test]
fn o11_tone_in_noise_matches_snr() {
    let config = o1_config();
    let mut state = 6789;
    for snr_db in [0.0, 5.0, 10.0, 15.0, 20.0, 30.0] {
        let ratio = 10.0_f64.powf(snr_db / 10.0);
        let noise_amplitude = (0.5 / ratio).sqrt();
        let samples: Vec<_> = (0..2049 + 99 * 256)
            .map(|n| {
                (2.0 * PI * 220.0 * n as f64 / 44_100.0).sin()
                    + noise_amplitude * gaussian(&mut state)
            })
            .collect();
        let frames = all_frames(&config, 44_100, &samples);
        assert_eq!(frames.len(), 100);
        let expected = 1.0 / (1.0 + ratio);
        let mut sum = 0.0;
        for frame in frames {
            let measured = frame.aperiodicity.unwrap();
            sum += measured;
            if snr_db >= 15.0 {
                assert!(frame.voiced);
            }
            if snr_db <= 5.0 {
                assert!(!frame.voiced);
            }
            if [10.0, 20.0, 30.0].contains(&snr_db) {
                assert!(
                    (measured / expected - 1.0).abs() <= 0.2,
                    "SNR={snr_db}: {measured}/{expected}"
                );
            }
        }
        eprintln!(
            "O11 SNR={snr_db}, mean aperiodicity={}, theory={expected}",
            sum / 100.0
        );
    }
}
