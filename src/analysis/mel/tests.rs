use super::*;
use std::f64::consts::PI;

#[test]
fn named_frontends_publish_finite_chunk_invariant_frames() {
    let mut config = MelConfig::neural_frontend();
    config.expected_sample_rate_hz = None;
    config.fft_size = 64;
    config.hop_size = 16;
    config.bands = 8;
    config.fmax_hz = 8_000.0;
    let samples: Vec<_> = (0..257)
        .map(|i| (2.0 * PI * 5.0 * i as f64 / 64.0).sin())
        .collect();
    let mut whole = MelAnalyzer::new(&config, 16_000).unwrap();
    let whole_count = whole.push(&samples);
    let whole_bits: Vec<_> = whole
        .frame()
        .unwrap()
        .values
        .iter()
        .map(|value| value.to_bits())
        .collect();
    let mut chunked = MelAnalyzer::new(&config, 16_000).unwrap();
    let mut count = 0;
    for chunk in samples.chunks(7) {
        count += chunked.push(chunk);
    }
    assert_eq!(count, whole_count);
    assert!(chunked.frame().unwrap().valid);
    assert_eq!(
        chunked
            .frame()
            .unwrap()
            .values
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        whole_bits
    );
}

#[test]
fn mfcc_matches_orthonormal_dct_shape_and_invalidates_nonfinite_frames() {
    let mut mel = MelConfig::neural_frontend();
    mel.expected_sample_rate_hz = None;
    mel.fft_size = 32;
    mel.hop_size = 16;
    mel.bands = 4;
    mel.fmax_hz = 8_000.0;
    let config = MfccConfig {
        mel,
        coefficients: 4,
        apply_log: false,
        log_floor: 1e-12,
    };
    let mut analyzer = MfccAnalyzer::new(&config, 16_000).unwrap();
    assert_eq!(analyzer.push(&[0.25; 32]), 1);
    let frame = analyzer.frame().unwrap();
    assert!(frame.valid);
    assert_eq!(frame.coefficients.len(), 4);
    assert!(frame.coefficients.iter().all(|value| value.is_finite()));
    analyzer.push(&[f64::NAN; 16]);
    assert!(!analyzer.frame().unwrap().valid);
    assert!(analyzer.coefficients().is_none());
}

#[test]
fn named_geometry_rates_are_checked() {
    assert!(matches!(
        MelAnalyzer::new(&MelConfig::neural_frontend(), 22_050),
        Err(ProcessError::SampleRateMismatch { .. })
    ));
    assert!(matches!(
        MfccAnalyzer::new(&MfccConfig::from_mel(MelConfig::domain_128(), 13), 11_025),
        Err(ProcessError::SampleRateMismatch { .. })
    ));
}

#[test]
fn mel_and_mfcc_steady_pushes_do_not_allocate() {
    let mut mel_config = MelConfig::neural_frontend();
    mel_config.expected_sample_rate_hz = None;
    mel_config.fft_size = 64;
    mel_config.hop_size = 16;
    mel_config.bands = 8;
    mel_config.fmax_hz = 8_000.0;
    let samples = vec![0.1; 512];
    let mut mel = MelAnalyzer::new(&mel_config, 16_000).unwrap();
    assert_no_alloc::assert_no_alloc(|| {
        mel.push(&samples);
        std::hint::black_box(mel.frame());
    });
    let mut mfcc = MfccAnalyzer::new(&MfccConfig::from_mel(mel_config, 4), 16_000).unwrap();
    assert_no_alloc::assert_no_alloc(|| {
        mfcc.push(&samples);
        std::hint::black_box(mfcc.frame());
    });
}

const BINS: usize = 513;

/// Deterministic 64-bit LCG mapped to `[-1, 1)`.
fn lcg(state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    ((*state >> 11) as f64 / (1_u64 << 53) as f64) * 2.0 - 1.0
}

fn close_rel(actual: f64, expected: f64, tolerance: f64) -> bool {
    (actual - expected).abs() <= tolerance * expected.abs()
}

/// `(first, last, count)` of the nonzero bins of one band-major filter row.
fn support(filters: &[f64], band: usize) -> (usize, usize, usize) {
    let row = &filters[band * BINS..(band + 1) * BINS];
    let nonzero: Vec<usize> = (0..BINS).filter(|&k| row[k] != 0.0).collect();
    (nonzero[0], *nonzero.last().unwrap(), nonzero.len())
}

// M1: Slaney scale reference points and round trip of the private helpers.
#[test]
fn m1_slaney_scale_reference_points_and_round_trip() {
    assert!((hz_to_mel(20.0) - 0.3).abs() <= 1e-15);
    assert!((hz_to_mel(30.0) - 0.45).abs() <= 1e-15);
    assert_eq!(hz_to_mel(1000.0), 15.0);
    assert!((hz_to_mel(5000.0) - 38.409401).abs() <= 1e-6);
    assert!((hz_to_mel(11_000.0) - 49.877575).abs() <= 1e-6);
    for hz in [20.0, 30.0, 500.0, 999.9, 1000.0, 1000.1, 5000.0, 11_000.0] {
        let back = mel_to_hz(hz_to_mel(hz));
        assert!(close_rel(back, hz, 1e-12), "{hz} -> {back}");
    }
}

// M2: frozen N2 geometry (`neural_frontend`) at 11025 Hz via `filters()`.
#[test]
fn m2_neural_frontend_filterbank_geometry() {
    let analyzer = MelAnalyzer::new(&MelConfig::neural_frontend(), 11_025).unwrap();
    let filters = analyzer.filters();
    assert_eq!(filters.len(), 40 * BINS);
    assert_eq!(filters.iter().filter(|&&w| w != 0.0).count(), 891);
    assert!(filters.iter().all(|&w| w >= 0.0));
    let bin_width = analyzer.bin_width_hz();
    assert_eq!(bin_width, 11_025.0 / 1024.0);
    let areas: Vec<f64> = filters
        .chunks_exact(BINS)
        .map(|row| row.iter().sum::<f64>() * bin_width)
        .collect();
    let min_area = areas.iter().copied().fold(f64::INFINITY, f64::min);
    let max_area = areas.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    // The brief's [0.9982, 1.0038] bracket is these extremes rounded inward to
    // four decimals (independently recomputed: 0.9981617013, 1.0038184640), so
    // the exact extremes are pinned instead, which is strictly tighter.
    assert!(
        (min_area - 0.998_161_701).abs() <= 1e-9 && (max_area - 1.003_818_464).abs() <= 1e-9,
        "area range [{min_area}, {max_area}]"
    );
    let lower = hz_to_mel(20.0);
    let upper = hz_to_mel(5000.0);
    let step = (upper - lower) / 41.0;
    let edge = |i: usize| mel_to_hz(lower + (upper - lower) * i as f64 / 41.0);
    assert!(close_rel(edge(0), 20.0, 1e-12), "{}", edge(0));
    assert!(close_rel(edge(41), 5000.0, 1e-12), "{}", edge(41));
    assert!((step - 0.929498).abs() <= 1e-6, "{step}");
    let scale_0 = 2.0 / (edge(2) - edge(0));
    let scale_39 = 2.0 / (edge(41) - edge(39));
    assert!(close_rel(scale_0, 1.613775e-2, 1e-6), "{scale_0}");
    assert!(close_rel(scale_39, 3.333924e-3, 1e-6), "{scale_39}");
    // The recomputed edges are the production supports: a weight is nonzero
    // exactly on the bins strictly inside (e_band, e_band+2).
    for band in 0..40 {
        for k in 0..BINS {
            let hz = k as f64 * bin_width;
            let inside = hz > edge(band) && hz < edge(band + 2);
            assert_eq!(filters[band * BINS + k] != 0.0, inside, "{band} {k}");
        }
    }
}

// M3: frozen H2 geometry (`domain_128`) at 22050 Hz via `filters()`.
#[test]
fn m3_domain_128_filterbank_geometry() {
    let analyzer = MelAnalyzer::new(&MelConfig::domain_128(), 22_050).unwrap();
    let filters = analyzer.filters();
    assert_eq!(filters.len(), 128 * BINS);
    assert_eq!(filters.iter().filter(|&&w| w != 0.0).count(), 1004);
    assert_eq!(support(filters, 0), (2, 3, 2));
    assert_eq!(support(filters, 127), (485, 510, 26));
    let peaks: Vec<f64> = filters
        .chunks_exact(BINS)
        .map(|row| row.iter().copied().fold(0.0, f64::max))
        .collect();
    let min_peak = peaks.iter().copied().fold(f64::INFINITY, f64::min);
    let max_peak = peaks.iter().copied().fold(0.0, f64::max);
    assert!((min_peak - 0.586).abs() <= 5e-4, "{min_peak}");
    // research/mel-mfcc-geometry.md quotes a maximum of 0.967, which its own
    // bit-exact librosa geometry contradicts: 35 of 128 bands peak above 0.967
    // and band 13 peaks at 0.9993002886 (recomputed independently), so the
    // maximum and its band are pinned here instead.
    assert!((max_peak - 0.999_300_289).abs() <= 1e-9, "{max_peak}");
    assert_eq!(peaks[13], max_peak);
    assert_eq!(peaks.iter().filter(|&&peak| peak > 0.967).count(), 35);
    let mut unity_bins = 0;
    for k in 0..BINS {
        let hz = k as f64 * 22_050.0 / 1024.0;
        if (55.5440..=10_714.011_6).contains(&hz) {
            unity_bins += 1;
            let sum: f64 = (0..128).map(|m| filters[m * BINS + k]).sum();
            assert!((sum - 1.0).abs() <= 1e-12, "bin {k}: {sum}");
        }
    }
    assert_eq!(unity_bins, 495);
}

/// Independent mel reference for one 1024-sample frame: periodic Hann, O(N^2)
/// DFT with an exact `(k * n) % N` phase index, `|X_k|` for `k = 0..=512`
/// (optionally over `sqrt(N)`), the given filter rows, then optional
/// `ln(1 + 1000 * x)`. No DC removal and no peak scaling.
fn direct_mel(frame: &[f64], filters: &[f64], sqrt_scale: bool, log: bool) -> Vec<f64> {
    const N: usize = 1024;
    let angle = |j: usize| 2.0 * PI * j as f64 / N as f64;
    let cos: Vec<f64> = (0..N).map(|j| angle(j).cos()).collect();
    let sin: Vec<f64> = (0..N).map(|j| angle(j).sin()).collect();
    let windowed: Vec<f64> = frame
        .iter()
        .enumerate()
        .map(|(n, &x)| x * 0.5 * (1.0 - angle(n).cos()))
        .collect();
    let magnitudes: Vec<f64> = (0..BINS)
        .map(|k| {
            let (mut re, mut im) = (0.0, 0.0);
            for (n, &value) in windowed.iter().enumerate() {
                re += value * cos[(k * n) % N];
                im -= value * sin[(k * n) % N];
            }
            let magnitude = re.hypot(im);
            if sqrt_scale {
                magnitude / (N as f64).sqrt()
            } else {
                magnitude
            }
        })
        .collect();
    filters
        .chunks_exact(BINS)
        .map(|row| {
            let value: f64 = row.iter().zip(&magnitudes).map(|(w, m)| w * m).sum();
            if log {
                (1.0 + 1000.0 * value).ln()
            } else {
                value
            }
        })
        .collect()
}

// M4: first and second frame of both named geometries at their own rates
// against the independent direct-DFT reference above.
#[test]
fn m4_named_frontend_frames_match_direct_dft() {
    let geometries = [
        (MelConfig::neural_frontend(), 11_025_u32),
        (MelConfig::domain_128(), 22_050),
    ];
    for (config, rate) in geometries {
        let domain = config.log_compression == MelLogCompression::Log1pThousand;
        let hop = config.hop_size;
        let sr = rate as f64;
        let samples: Vec<f64> = (0..1024 + hop)
            .map(|i| {
                0.3 * (2.0 * PI * 440.7 * i as f64 / sr).sin()
                    + 0.2 * (2.0 * PI * 1234.5 * i as f64 / sr + 0.4).sin()
                    + 0.05
            })
            .collect();
        let mut analyzer = MelAnalyzer::new(&config, rate).unwrap();
        for (start, pushed) in [(0, &samples[..1024]), (hop, &samples[1024..])] {
            assert_eq!(analyzer.push(pushed), 1);
            let frame = analyzer.frame().unwrap();
            assert!(frame.valid);
            assert_eq!(frame.end_sample, (start + 1024) as u64);
            let window = &samples[start..start + 1024];
            let expected = direct_mel(window, analyzer.filters(), domain, domain);
            let peak = expected
                .iter()
                .fold(0.0_f64, |acc, value| acc.max(value.abs()));
            for (band, (&actual, &reference)) in frame.values.iter().zip(&expected).enumerate() {
                // Log values: 1e-12 absolute. Linear values: 1e-12 relative to
                // the frame's largest band. FFT rounding is absolute at the
                // frame's scale, so the leakage-only top N2 band matches only to
                // about 2e-9 per-band relative while the frame-relative error
                // stays below 6e-16.
                let bound = if domain { 1e-12 } else { 1e-12 * peak };
                assert!(
                    (actual - reference).abs() <= bound,
                    "{rate} Hz frame at {start} band {band}: {actual} vs {reference}"
                );
            }
        }
    }
}

/// Custom M5/M7 geometry for 8000 Hz: FFT 64, hop 16, 8 area-normalized
/// bands from 50 to 3500 Hz, raw magnitude, no log, no expected rate.
fn custom() -> MelConfig {
    MelConfig {
        fft_size: 64,
        hop_size: 16,
        window: WindowFunction::Hann,
        bands: 8,
        fmin_hz: 50.0,
        fmax_hz: 3500.0,
        normalization: MelNormalization::Area,
        spectrum_scale: MelSpectrumScale::RawMagnitude,
        log_compression: MelLogCompression::None,
        expected_sample_rate_hz: None,
    }
}

/// 640 samples of LCG noise with gain steps every 128 samples and NaN at
/// samples 300..=303.
fn m5_signal() -> Vec<f64> {
    let mut state = 0x5eed_u64;
    (0..640)
        .map(|i| {
            let noise = [0.05, 0.8, 0.2, 1.5, 0.01][i / 128] * lcg(&mut state);
            if (300..=303).contains(&i) {
                f64::NAN
            } else {
                noise
            }
        })
        .collect()
}

type Bits = (u64, bool, Vec<u64>);

fn bits(end_sample: u64, valid: bool, values: &[f64]) -> Bits {
    let values = values.iter().map(|value| value.to_bits()).collect();
    (end_sample, valid, values)
}

/// The M5 protocol for one analyzer type: a 1-sample reference that records
/// every published frame, chunked replays compared bitwise after every
/// publishing push, and warm-up after `reset`. `latest` returns the published
/// frame's bits and whether the valid-only accessor returned `Some`.
fn m5_protocol<A>(
    samples: &[f64],
    make: impl Fn() -> A,
    push: impl Fn(&mut A, &[f64]) -> usize,
    latest: impl Fn(&A) -> Option<(Bits, bool)>,
    reset: impl Fn(&mut A),
) {
    let touches_nan = |end: u64| end > 300 && end - 64 <= 303;
    let mut analyzer = make();
    let mut reference = Vec::new();
    for sample in samples {
        if push(&mut analyzer, std::slice::from_ref(sample)) == 1 {
            let (frame, accessible) = latest(&analyzer).unwrap();
            assert_eq!(
                frame.1,
                !touches_nan(frame.0),
                "frame ending at {}",
                frame.0
            );
            assert_eq!(accessible, frame.1, "frame ending at {}", frame.0);
            reference.push(frame);
        }
    }
    assert_eq!(reference.len(), 37);
    let invalid: Vec<u64> = reference.iter().filter(|f| !f.1).map(|f| f.0).collect();
    assert_eq!(invalid, [304, 320, 336, 352]);
    assert!(reference.iter().any(|f| f.0 == 368 && f.1));
    for chunk in [7, 64, 1000, samples.len()] {
        let mut chunked = make();
        let mut count = 0;
        for piece in samples.chunks(chunk) {
            let published = push(&mut chunked, piece);
            count += published;
            if published > 0 {
                let (frame, _) = latest(&chunked).unwrap();
                assert_eq!(frame, reference[count - 1], "chunk {chunk}");
            }
        }
        assert_eq!(count, reference.len(), "chunk {chunk}");
    }
    reset(&mut analyzer);
    assert!(latest(&analyzer).is_none());
    assert_eq!(push(&mut analyzer, &samples[..63]), 0);
    assert!(latest(&analyzer).is_none());
    assert_eq!(push(&mut analyzer, &samples[63..64]), 1);
    assert_eq!(latest(&analyzer).unwrap().0, reference[0]);
}

// M5: chunk invariance, NaN contamination and reset warm-up for the mel and
// MFCC analyzers on the custom geometry at 8000 Hz.
#[test]
fn m5_chunk_invariance_nan_contamination_and_reset() {
    let samples = m5_signal();
    m5_protocol(
        &samples,
        || MelAnalyzer::new(&custom(), 8000).unwrap(),
        |analyzer: &mut MelAnalyzer, chunk: &[f64]| analyzer.push(chunk),
        |analyzer: &MelAnalyzer| {
            let frame = analyzer.frame()?;
            let accessible = analyzer.mel().is_some();
            Some((
                bits(frame.end_sample, frame.valid, &frame.values),
                accessible,
            ))
        },
        MelAnalyzer::reset,
    );
    let config = MfccConfig::from_mel(custom(), 8);
    assert!(config.apply_log);
    m5_protocol(
        &samples,
        || MfccAnalyzer::new(&config, 8000).unwrap(),
        |analyzer: &mut MfccAnalyzer, chunk: &[f64]| analyzer.push(chunk),
        |analyzer: &MfccAnalyzer| {
            let frame = analyzer.frame()?;
            let accessible = analyzer.coefficients().is_some();
            let coefficients = &frame.coefficients;
            Some((
                bits(frame.end_sample, frame.valid, coefficients),
                accessible,
            ))
        },
        MfccAnalyzer::reset,
    );
}

// M6: orthonormal DCT-II identities on the private helper, then the MFCC
// analyzer end to end against an independent direct DCT formula.
#[test]
fn m6_dct2_ortho_identities_and_end_to_end_mfcc() {
    let mut output = [0.0; 8];
    dct2_ortho(&[0.37; 8], &mut output);
    assert!(
        (output[0] - 8.0_f64.sqrt() * 0.37).abs() <= 1e-15,
        "{output:?}"
    );
    // The brief's 1e-15 assumed exact cancellation; the settled formula takes
    // cos of unreduced angles up to 20.6 rad (ulp 3.6e-15), whose worst-case
    // rounding bound here is about 3.6e-15. Observed: c7 = 1.33e-15.
    assert!(output[1..].iter().all(|c| c.abs() <= 4e-15), "{output:?}");

    let basis: Vec<f64> = (0..8)
        .map(|n| (PI / 8.0 * (n as f64 + 0.5) * 3.0).cos())
        .collect();
    dct2_ortho(&basis, &mut output);
    for (j, &c) in output.iter().enumerate() {
        let expected = if j == 3 { 2.0 } else { 0.0 };
        assert!((c - expected).abs() <= 1e-14, "{j}: {c}");
    }

    let mut state = 0x0dc7_u64;
    let vector: Vec<f64> = (0..16).map(|_| lcg(&mut state)).collect();
    let mut coefficients = [0.0; 16];
    dct2_ortho(&vector, &mut coefficients);
    let energy: f64 = vector.iter().map(|x| x * x).sum();
    let transformed: f64 = coefficients.iter().map(|c| c * c).sum();
    assert!(
        close_rel(transformed, energy, 1e-12),
        "{transformed} vs {energy}"
    );

    let config = MfccConfig::from_mel(MelConfig::neural_frontend(), 13);
    let mut mfcc = MfccAnalyzer::new(&config, 11_025).unwrap();
    let samples: Vec<f64> = (0..1536).map(|_| 0.5 * lcg(&mut state)).collect();
    assert_eq!(mfcc.push(&samples), 2);
    let logs: Vec<f64> = mfcc
        .mel()
        .unwrap()
        .values
        .iter()
        .map(|value| value.max(1e-12).ln())
        .collect();
    let m = logs.len();
    let actual = mfcc.coefficients().unwrap();
    assert_eq!(actual.len(), 13);
    for (j, &coefficient) in actual.iter().enumerate() {
        // c_j = s_j * sum_n x_n cos(pi (2n + 1) j / (2M)), with the angle
        // reduced exactly modulo 2*pi in integers.
        let scale = if j == 0 { 1.0 } else { 2.0 };
        let sum: f64 = logs
            .iter()
            .enumerate()
            .map(|(n, &x)| x * (PI * (((2 * n + 1) * j) % (4 * m)) as f64 / (2 * m) as f64).cos())
            .sum();
        let expected = (scale / m as f64).sqrt() * sum;
        assert!(
            (coefficient - expected).abs() <= 1e-12,
            "{j}: {coefficient} vs {expected}"
        );
    }
}

fn mel_error(config: &MelConfig, rate: u32) -> ProcessError {
    assert_no_alloc::assert_no_alloc(|| MelAnalyzer::new(config, rate).err())
        .expect("configuration must be rejected")
}

fn mfcc_error(config: &MfccConfig, rate: u32) -> ProcessError {
    assert_no_alloc::assert_no_alloc(|| MfccAnalyzer::new(config, rate).err())
        .expect("configuration must be rejected")
}

fn assert_parameter(error: ProcessError, processor: &str, parameter: &str) {
    assert!(
        matches!(&error, ProcessError::InvalidParameter { processor: p, parameter: q, .. }
            if *p == processor && *q == parameter),
        "{error:?} is not {processor}.{parameter}"
    );
}

fn assert_geometry(error: ProcessError) {
    assert!(
        matches!(
            &error,
            ProcessError::InvalidGeometry {
                processor: "MelAnalyzer",
                ..
            }
        ),
        "{error:?}"
    );
}

// M7: every invalid configuration returns its variant before allocating.
#[test]
fn m7_invalid_configuration_is_rejected_before_allocating() {
    let rate = |sample_rate_hz| ProcessError::InvalidSampleRate {
        processor: "MelAnalyzer",
        sample_rate_hz,
    };
    let mismatch = |expected, actual| ProcessError::SampleRateMismatch {
        processor: "MelAnalyzer",
        expected_sample_rate_hz: expected,
        actual_sample_rate_hz: actual,
    };
    assert_eq!(mel_error(&custom(), 0), rate(0));
    assert_eq!(mel_error(&MelConfig::neural_frontend(), 0), rate(0));
    let neural = MelConfig::neural_frontend();
    assert_eq!(mel_error(&neural, 22_050), mismatch(11_025, 22_050));
    let domain = MelConfig::domain_128();
    assert_eq!(mel_error(&domain, 11_025), mismatch(22_050, 11_025));
    for fft_size in [2, 48, 0] {
        assert_geometry(mel_error(
            &MelConfig {
                fft_size,
                ..custom()
            },
            8000,
        ));
    }
    for hop_size in [0, 65] {
        assert_geometry(mel_error(
            &MelConfig {
                hop_size,
                ..custom()
            },
            8000,
        ));
    }
    for bands in [0, 34] {
        let error = mel_error(&MelConfig { bands, ..custom() }, 8000);
        assert_parameter(error, "MelAnalyzer", "bands");
    }
    for fmin_hz in [f64::NAN, -1.0, 3500.0, 3600.0] {
        let error = mel_error(
            &MelConfig {
                fmin_hz,
                ..custom()
            },
            8000,
        );
        assert_parameter(error, "MelAnalyzer", "fmin_hz/fmax_hz");
    }
    for fmax_hz in [4000.5, f64::INFINITY] {
        let error = mel_error(
            &MelConfig {
                fmax_hz,
                ..custom()
            },
            8000,
        );
        assert_parameter(error, "MelAnalyzer", "fmin_hz/fmax_hz");
    }
    for beta in [f64::NAN, -1.0, 51.0] {
        let window = WindowFunction::Kaiser { beta };
        let error = mel_error(&MelConfig { window, ..custom() }, 8000);
        assert_parameter(error, "MelAnalyzer", "window.beta");
    }

    let base = MfccConfig::from_mel(custom(), 8);
    for coefficients in [0, 9] {
        let config = MfccConfig {
            coefficients,
            ..base.clone()
        };
        assert_parameter(mfcc_error(&config, 8000), "MfccAnalyzer", "coefficients");
    }
    for log_floor in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let config = MfccConfig {
            log_floor,
            ..base.clone()
        };
        assert_parameter(mfcc_error(&config, 8000), "MfccAnalyzer", "log_floor");
    }
    let compressed = MfccConfig {
        apply_log: true,
        ..MfccConfig::from_mel(MelConfig::domain_128(), 13)
    };
    assert_parameter(mfcc_error(&compressed, 22_050), "MfccAnalyzer", "apply_log");
    let broken_mel = MfccConfig {
        mel: MelConfig {
            fmin_hz: f64::NAN,
            ..custom()
        },
        ..base.clone()
    };
    let error = mfcc_error(&broken_mel, 8000);
    assert_parameter(error, "MelAnalyzer", "fmin_hz/fmax_hz");
    assert!(!MfccConfig::from_mel(MelConfig::domain_128(), 13).apply_log);
    assert!(MfccConfig::from_mel(MelConfig::neural_frontend(), 13).apply_log);
    assert!(MfccAnalyzer::new(&MfccConfig::from_mel(MelConfig::domain_128(), 13), 22_050).is_ok());
}

// M8: the first push after construction, a primed push, every accessor and
// `reset()` allocate nothing, for both analyzers on both named geometries.
#[test]
fn m8_pushes_accessors_and_reset_do_not_allocate() {
    let geometries = [
        (MelConfig::neural_frontend(), 11_025_u32),
        (MelConfig::domain_128(), 22_050),
    ];
    for (config, rate) in geometries {
        let mut state = 0x88_u64;
        let samples: Vec<f64> = (0..4096).map(|_| lcg(&mut state)).collect();
        let mut mel = MelAnalyzer::new(&config, rate).unwrap();
        let mut mfcc = MfccAnalyzer::new(&MfccConfig::from_mel(config, 13), rate).unwrap();
        for _ in 0..2 {
            assert_no_alloc::assert_no_alloc(|| {
                assert!(mel.push(&samples) > 0);
                std::hint::black_box((mel.frame(), mel.mel(), mel.filters()));
                std::hint::black_box((mel.bin_width_hz(), mel.sample_rate_hz()));
                assert!(mfcc.push(&samples) > 0);
                std::hint::black_box((mfcc.frame(), mfcc.coefficients(), mfcc.mel()));
            });
        }
        assert_no_alloc::assert_no_alloc(|| {
            mel.reset();
            mfcc.reset();
        });
        assert!(mel.frame().is_none() && mfcc.frame().is_none());
    }
}
