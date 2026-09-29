use super::*;
use std::f64::consts::PI;

mod lifecycle;
mod prior;
mod synthetic;

/// O1 geometry: `W = tau_max = 1024` at 44.1 kHz via `fmin = fs/1024`
/// (exact in binary) and `tau_min = floor(44100/4000) = 11`.
fn o1_config() -> PitchConfig {
    PitchConfig {
        fmin_hz: 44_100.0 / 1024.0,
        fmax_hz: 4000.0,
        ..PitchConfig::default()
    }
}

/// Small 8 kHz geometry: `tau_max = 80`, `N = 161`, `tau_min = 8`, hop 16.
fn small_config() -> PitchConfig {
    PitchConfig {
        fmin_hz: 100.0,
        fmax_hz: 1000.0,
        hop_size: 16,
        ..PitchConfig::default()
    }
}

/// `x[n] = p[n mod T]` with one period of a sine: bitwise periodic.
fn tiled_sine(period: usize, len: usize) -> Vec<f64> {
    let cycle: Vec<f64> = (0..period)
        .map(|n| (2.0 * PI * n as f64 / period as f64).sin())
        .collect();
    (0..len).map(|n| cycle[n % period]).collect()
}

fn sine(frequency_hz: f64, rate: u32, len: usize) -> Vec<f64> {
    (0..len)
        .map(|n| (2.0 * PI * frequency_hz * n as f64 / f64::from(rate)).sin())
        .collect()
}

fn lcg_uniform(state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 11) as f64 / (1_u64 << 53) as f64 * 2.0 - 1.0
}

fn cents(ratio: f64) -> f64 {
    1200.0 * ratio.log2()
}

/// Every published frame, pushing one sample at a time.
fn all_frames(config: &PitchConfig, rate: u32, samples: &[f64]) -> Vec<PitchFrame> {
    let mut analyzer = PitchAnalyzer::new(config, rate).unwrap();
    let mut frames = Vec::new();
    for sample in samples {
        if analyzer.push(std::slice::from_ref(sample)) > 0 {
            frames.push(analyzer.frame().unwrap().clone());
        }
    }
    frames
}

type FrameBits = (u64, Option<u64>, Option<u64>, Option<u64>, bool);

fn frame_bits(frame: &PitchFrame) -> FrameBits {
    (
        frame.end_sample,
        frame.period_samples.map(f64::to_bits),
        frame.f0_hz.map(f64::to_bits),
        frame.aperiodicity.map(f64::to_bits),
        frame.voiced,
    )
}

fn assert_undefined(frame: &PitchFrame) {
    assert_eq!(frame.period_samples, None);
    assert_eq!(frame.f0_hz, None);
    assert_eq!(frame.aperiodicity, None);
    assert!(!frame.voiced);
    assert_eq!(frame.voicing_probability, None);
    assert!(frame.candidates.is_empty());
}

// Kernel boundary: the production FFT form of d (eq. 7) agrees with the
// direct form (eq. 6) on the same conditioned buffer, including after the ring
// wraps and with a DC offset far above the signal.
#[test]
fn fft_difference_matches_direct_form_on_random_buffers() {
    let config = PitchConfig::default();
    let mut worst: f64 = 0.0;
    for (seed, offset) in [(1_u64, 0.0), (2, 0.0), (3, 1e6), (4, -1e6)] {
        let mut state = seed;
        let samples: Vec<f64> = (0..4000)
            .map(|n| offset + (1.0 + (n / 700) as f64) * lcg_uniform(&mut state))
            .collect();
        let mut analyzer = PitchAnalyzer::new(&config, 44_100).unwrap();
        let (n, w) = (analyzer.buffer_samples(), analyzer.window);
        assert_eq!((n, w, analyzer.tau_min), (1605, 802, 25));
        let mut direct = vec![0.0; analyzer.tau_max + 2];
        let mut end = 0;
        for chunk in [n, 256, 512, 768] {
            analyzer.push(&samples[end..end + chunk]);
            end += chunk;
            let frame = analyzer.frame().unwrap();
            assert_eq!(frame.end_sample, end as u64);
            // The conditioned buffer is the chronological window, peak-scaled
            // and mean-removed in the same operation order.
            let window = &samples[end - n..end];
            let scale = window.iter().fold(0.0_f64, |peak, x| peak.max(x.abs()));
            let mean = window.iter().map(|x| x / scale).sum::<f64>() / n as f64;
            for (value, raw) in analyzer.conditioned.iter().zip(window) {
                assert_eq!(value.to_bits(), (raw / scale - mean).to_bits());
            }
            direct_difference(&analyzer.conditioned, w, &mut direct);
            let block_energy: f64 = analyzer.conditioned[n - w..].iter().map(|x| x * x).sum();
            assert_eq!(analyzer.difference[0], 0.0);
            for (fft, exact) in analyzer.difference.iter().zip(&direct) {
                let error = (fft - exact).abs() / block_energy;
                worst = worst.max(error);
                assert!(error <= 1e-12, "seed {seed}: {fft} vs {exact}");
            }
        }
    }
    eprintln!("worst FFT/direct error relative to sum a^2: {worst:e}");
}

// O1 (exact integer period) and O2 (vertex on d, not d'): tiled sines with
// T | W. The kernel on the direct-form d is exact; the FFT analyzer is within
// 1e-12, and the period error stays below 1e-9 cents in both, where a vertex on
// d' would miss by about +866/T^2 cents.
#[test]
fn o1_o2_exact_integer_period_is_recovered_exactly() {
    let config = o1_config();
    let mut worst_aperiodicity: f64 = 0.0;
    let mut worst_cents: f64 = 0.0;
    for period in [16_usize, 32, 64, 128, 256, 512] {
        let samples = tiled_sine(period, 2049 + 3 * 256);
        let mut difference = vec![0.0; 1026];
        let mut normalized = vec![0.0; 1026];
        direct_difference(&samples[..2049], 1024, &mut difference);
        let estimate = yin_kernel(&difference, &mut normalized, 11, 1024, 0.1).unwrap();
        assert_eq!(
            estimate.aperiodicity.to_bits(),
            0.0_f64.to_bits(),
            "T {period}"
        );
        assert_eq!(estimate.period, period as f64, "T {period}");

        // O2 discriminates: the d' vertex at the same dip is visibly biased.
        let (left, right) = (normalized[period - 1], normalized[period + 1]);
        let offset = (left - right) / (2.0 * (left - 2.0 * normalized[period] + right));
        let biased = cents(period as f64 / (period as f64 + offset));
        let predicted = 866.0 / (period * period) as f64;
        assert!(
            biased > 0.5 * predicted && biased < 2.0 * predicted,
            "{biased}"
        );

        let frames = all_frames(&config, 44_100, &samples);
        assert_eq!(frames.len(), 4);
        for frame in &frames {
            let aperiodicity = frame.aperiodicity.unwrap();
            let ratio = frame.period_samples.unwrap() / period as f64;
            assert!(aperiodicity <= 1e-12, "T {period}: {aperiodicity:e}");
            assert!((ratio - 1.0).abs() <= 1e-12, "T {period}: {ratio}");
            assert!(cents(ratio).abs() < 1e-9, "T {period}: {ratio}");
            assert!(frame.voiced);
            assert!((frame.voicing_probability.unwrap() - 1.0).abs() <= 1e-12);
            assert!(
                (frame.candidates.iter().map(|c| c.probability).sum::<f64>() - 1.0).abs() <= 1e-12
            );
            assert_eq!(
                frame.f0_hz.unwrap(),
                44_100.0 / frame.period_samples.unwrap()
            );
            worst_aperiodicity = worst_aperiodicity.max(aperiodicity);
            worst_cents = worst_cents.max(cents(ratio).abs());
        }
    }
    eprintln!("O1 FFT form: aperiodicity <= {worst_aperiodicity:e}, |cents| <= {worst_cents:e}");
}

// Default-geometry smoke test: a 220 Hz sine at 44.1 kHz is voiced within
// 0.25 cents in every frame (O3 bound at one frequency).
#[test]
fn default_config_measures_a_220_hz_sine() {
    let samples = sine(220.0, 44_100, 22_050);
    let frames = all_frames(&PitchConfig::default(), 44_100, &samples);
    assert_eq!(frames.len(), 1 + (22_050 - 1605) / 256);
    let mut worst: f64 = 0.0;
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.end_sample, (1605 + index * 256) as u64);
        assert!(frame.voiced);
        let error = cents(frame.f0_hz.unwrap() / 220.0).abs();
        worst = worst.max(error);
        assert!(error <= 0.25, "frame {index}: {error} c");
    }
    eprintln!("220 Hz sine: worst {worst:e} c");
}
