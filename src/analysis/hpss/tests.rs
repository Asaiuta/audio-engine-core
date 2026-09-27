use super::*;
use crate::analysis::{DescriptorAnalyzer, DescriptorConfig};
use std::f64::consts::PI;

fn small_config() -> HpssConfig {
    HpssConfig {
        fft_size: 32,
        hop_size: 8,
        harmonic_median_frames: 3,
        percussive_median_bins: 3,
        ..HpssConfig::default()
    }
}

#[test]
fn hpss_has_fixed_context_latency_and_complementary_masks() {
    let config = small_config();
    let mut analyzer = HpssAnalyzer::new(&config, 8_000).unwrap();
    let samples: Vec<_> = (0..96)
        .map(|i| (2.0 * PI * 4.0 * i as f64 / 32.0).sin())
        .collect();
    assert_eq!(analyzer.push(&samples[..31]), 0);
    assert!(analyzer.frame().is_none());
    assert_eq!(analyzer.push(&samples[31..48]), 1);
    let frame = analyzer.frame().unwrap();
    assert_eq!(analyzer.lookahead_samples(), 8);
    assert_eq!(frame.end_sample, 40);
    for (harmonic, percussive) in frame.harmonic_mask.iter().zip(&frame.percussive_mask) {
        if let (Some(harmonic), Some(percussive)) = (harmonic, percussive) {
            assert!((harmonic + percussive - 1.0).abs() <= 1e-12);
        }
    }
    assert!(frame.unassigned_fraction.unwrap_or(0.0) >= 0.0);
}

#[test]
fn hpss_silence_has_undefined_masks_and_no_fraction() {
    let mut analyzer = HpssAnalyzer::new(&small_config(), 8_000).unwrap();
    analyzer.push(&[0.0; 64]);
    let frame = analyzer.frame().unwrap();
    assert!(frame.power.iter().all(|value| *value == Some(0.0)));
    assert!(frame.harmonic_mask.iter().all(Option::is_none));
    assert_eq!(frame.harmonic_fraction, None);
}

#[test]
fn hpss_steady_pushes_do_not_allocate() {
    let mut analyzer = HpssAnalyzer::new(&small_config(), 8_000).unwrap();
    let samples = vec![0.1; 128];
    assert_no_alloc::assert_no_alloc(|| {
        analyzer.push(&samples);
        std::hint::black_box(analyzer.frame());
    });
}

fn lcg_uniform(state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 11) as f64 / (1_u64 << 53) as f64 * 2.0 - 1.0
}

/// Absolute log power of bins `1..=n/2`: mean removal, periodic Hann,
/// direct DFT, no per-frame normalisation.
fn reference_log_power(frame: &[f64]) -> Vec<f64> {
    let n = frame.len();
    let mean = frame.iter().sum::<f64>() / n as f64;
    let windowed: Vec<f64> = frame
        .iter()
        .enumerate()
        .map(|(i, x)| (x - mean) * 0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos()))
        .collect();
    (1..=n / 2)
        .map(|k| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, x) in windowed.iter().enumerate() {
                let phase = -2.0 * PI * ((k * i) % n) as f64 / n as f64;
                re += x * phase.cos();
                im += x * phase.sin();
            }
            let power = (re / n as f64).powi(2) + (im / n as f64).powi(2);
            if power > 0.0 {
                power.ln()
            } else {
                f64::NEG_INFINITY
            }
        })
        .collect()
}

fn median_of(values: &mut [f64]) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    values[values.len() / 2]
}

/// Reference masks for `mask_exponent = 2`, `margin = 1` (q = 1).
fn reference_masks(h: f64, v: f64) -> Option<(f64, f64)> {
    match (h == f64::NEG_INFINITY, v == f64::NEG_INFINITY) {
        (true, true) => None,
        (true, false) => Some((0.0, 1.0)),
        (false, true) => Some((1.0, 0.0)),
        (false, false) => {
            let harmonic = 1.0 / (1.0 + (v - h).exp());
            Some((harmonic, 1.0 - harmonic))
        }
    }
}

/// Frames inside one temporal median window carry different peak scales
/// (gain steps 1 -> 8 -> 0.25), so the harmonic median must compare
/// absolute log power, not per-frame normalised log power.
#[test]
fn hpss_masks_match_absolute_log_power_reference_across_gain_steps() {
    const N: usize = 64;
    const HOP: usize = 16;
    const BINS: usize = N / 2;
    let config = HpssConfig {
        fft_size: N,
        hop_size: HOP,
        window: WindowFunction::Hann,
        harmonic_median_frames: 5,
        percussive_median_bins: 5,
        mask_exponent: 2.0,
        margin: 1.0,
    };
    let mut state = 0x0123_4567_89ab_cdef_u64;
    let signal: Vec<f64> = (0..384)
        .map(|i| {
            let gain = if i < 160 {
                1.0
            } else if i < 224 {
                8.0
            } else {
                0.25
            };
            let tone = 0.3 * (2.0 * PI * 8.0 * i as f64 / N as f64).sin();
            gain * lcg_uniform(&mut state) + tone
        })
        .collect();
    let logs: Vec<Vec<f64>> = (0..=(signal.len() - N) / HOP)
        .map(|s| reference_log_power(&signal[s * HOP..s * HOP + N]))
        .collect();
    let mut analyzer = HpssAnalyzer::new(&config, 8_000).unwrap();
    let mut published = 0;
    for (chunk, samples) in signal.chunks(HOP).enumerate() {
        if analyzer.push(samples) == 0 {
            continue;
        }
        let completed = ((chunk + 1) * HOP - N) / HOP;
        let t = completed - 2;
        let frame = analyzer.frame().unwrap();
        assert_eq!(frame.end_sample, (N + HOP * t) as u64);
        for bin in 0..BINS {
            let mut temporal: Vec<f64> = (t - 2..=t + 2).map(|s| logs[s][bin]).collect();
            let mut spectral: Vec<f64> = (-2..=2_isize)
                .map(|delta| {
                    let j = bin as isize + delta;
                    let j = if j < 0 {
                        -j - 1
                    } else if j >= BINS as isize {
                        2 * BINS as isize - j - 1
                    } else {
                        j
                    };
                    logs[t][j as usize]
                })
                .collect();
            let harmonic_log = median_of(&mut temporal);
            let percussive_log = median_of(&mut spectral);
            let expected = reference_masks(harmonic_log, percussive_log);
            let actual = frame.harmonic_mask[bin].zip(frame.percussive_mask[bin]);
            match (expected, actual) {
                (None, None) => {}
                (Some((eh, ep)), Some((ah, ap))) => assert!(
                    (ah - eh).abs() <= 1e-9 && (ap - ep).abs() <= 1e-9,
                    "frame {t} bin {bin}: harmonic {ah} vs {eh}, percussive {ap} vs {ep}"
                ),
                _ => panic!("frame {t} bin {bin}: masks {actual:?}, reference {expected:?}"),
            }
        }
        published += 1;
    }
    assert_eq!(published, 17);
}

// Oracle suite from research/hpss-design.md section 5. Unless a test says
// otherwise: periodic Hann, 8000 Hz, p = 2, margin 1, N = 64, hop = 16.
// STFT frame `s` ends at `N + hop * s`; the frame published after STFT frame
// `s` describes centre `t = s - K_t`. Bin vectors are indexed `i = k - 1`.

fn oracle_config(harmonic_median_frames: usize, percussive_median_bins: usize) -> HpssConfig {
    HpssConfig {
        fft_size: 64,
        hop_size: 16,
        window: WindowFunction::Hann,
        harmonic_median_frames,
        percussive_median_bins,
        mask_exponent: 2.0,
        margin: 1.0,
    }
}

/// Push one hop at a time and clone every published frame with its centre
/// index `t`, checking the centre schedule and publication count.
fn published_frames(
    config: &HpssConfig,
    sample_rate_hz: u32,
    signal: &[f64],
) -> Vec<(usize, HpssFrame)> {
    let half = config.harmonic_median_frames / 2;
    let mut analyzer = HpssAnalyzer::new(config, sample_rate_hz).unwrap();
    let mut frames = Vec::new();
    for hop in signal.chunks(config.hop_size) {
        let updates = analyzer.push(hop);
        assert!(updates <= 1, "one hop completes at most one STFT frame");
        if updates == 1 {
            let t = frames.len() + half;
            let frame = analyzer.frame().unwrap().clone();
            assert_eq!(
                frame.end_sample,
                (config.fft_size + t * config.hop_size) as u64
            );
            frames.push((t, frame));
        }
    }
    let stft_frames = 1 + (signal.len() - config.fft_size) / config.hop_size;
    assert_eq!(frames.len(), stft_frames.saturating_sub(2 * half));
    frames
}

/// The F1 regression signal (same LCG seed, gain steps 1 -> 8 -> 0.25 and
/// tone), extended to `len` samples.
fn gain_step_noise(len: usize) -> Vec<f64> {
    let mut state = 0x0123_4567_89ab_cdef_u64;
    (0..len)
        .map(|i| {
            let gain = if i < 160 {
                1.0
            } else if i < 224 {
                8.0
            } else {
                0.25
            };
            let tone = 0.3 * (2.0 * PI * 8.0 * i as f64 / 64.0).sin();
            gain * lcg_uniform(&mut state) + tone
        })
        .collect()
}

fn fractions(frame: &HpssFrame) -> [Option<f64>; 4] {
    [
        frame.harmonic_fraction,
        frame.percussive_fraction,
        frame.residual_fraction,
        frame.unassigned_fraction,
    ]
}

/// A valid zero-energy centre: exact zero power, no masks, no fractions.
fn assert_zero_energy_frame(t: usize, frame: &HpssFrame) {
    assert!(frame.power.iter().all(|p| *p == Some(0.0)), "frame {t}");
    for masks in [
        &frame.harmonic_mask,
        &frame.percussive_mask,
        &frame.residual_mask,
    ] {
        assert!(masks.iter().all(Option::is_none), "frame {t}");
    }
    assert_eq!(fractions(frame), [None; 4], "frame {t}");
}

/// O1: a bin-centred sinusoid has exactly three nonzero periodic-Hann bins,
/// which are exactly harmonic in every published frame. Their frequency
/// median is FFT rounding noise, far below `1e-20` of the peak, so the
/// percussive minority keeps that tiny value instead of rounding to zero.
#[test]
fn o1_bin_centred_sinusoid_is_exactly_harmonic() {
    let config = oracle_config(5, 7);
    let signal: Vec<f64> = (0..256)
        .map(|i| 0.5 * (2.0 * PI * 12.0 * i as f64 / 64.0).sin())
        .collect();
    let frames = published_frames(&config, 8_000, &signal);
    assert_eq!(frames.len(), 9);
    for (t, frame) in &frames {
        for i in 10..=12 {
            assert_eq!(frame.harmonic_mask[i], Some(1.0), "frame {t} bin {i}");
            let percussive = frame.percussive_mask[i].unwrap();
            assert!(
                (0.0..=1e-18).contains(&percussive),
                "frame {t} bin {i}: {percussive}"
            );
        }
        let fraction = frame.harmonic_fraction.unwrap();
        assert!(
            (1.0 - 1e-12..=1.0).contains(&fraction),
            "frame {t}: {fraction}"
        );
    }
}

/// Fractions are ratios, so a valid frame whose absolute total power exceeds
/// `f64::MAX` still publishes them: the O1 tone at amplitude 4.5e154 has a
/// finite peak bin (1.27e308) but a total of 1.9e308.
#[test]
fn fractions_survive_a_total_power_beyond_f64_range() {
    let config = oracle_config(5, 7);
    let signal: Vec<f64> = (0..256)
        .map(|i| 4.5e154 * (2.0 * PI * 12.0 * i as f64 / 64.0).sin())
        .collect();
    let frames = published_frames(&config, 8_000, &signal);
    assert_eq!(frames.len(), 9);
    for (t, frame) in &frames {
        let total: f64 = frame.power.iter().map(|power| power.unwrap()).sum();
        assert_eq!(total, f64::INFINITY, "frame {t}");
        let fraction = frame.harmonic_fraction.unwrap();
        assert!(
            (1.0 - 1e-12..=1.0).contains(&fraction),
            "frame {t}: {fraction}"
        );
        let sum: f64 = fractions(frame).iter().map(|value| value.unwrap()).sum();
        assert!((sum - 1.0).abs() <= 1e-12, "frame {t}: {sum}");
    }
}

/// O2: regression bound, not a closed form. A half-bin sinusoid at the design
/// geometry stays near-harmonic (the design measured 0.999918).
#[test]
fn o2_half_bin_sinusoid_is_near_harmonic_at_design_geometry() {
    let config = HpssConfig {
        fft_size: 2048,
        hop_size: 512,
        window: WindowFunction::Hann,
        harmonic_median_frames: 17,
        percussive_median_bins: 17,
        mask_exponent: 2.0,
        margin: 1.0,
    };
    let signal: Vec<f64> = (0..2048 + 20 * 512)
        .map(|i| 0.5 * (2.0 * PI * 100.5 * i as f64 / 2048.0).sin())
        .collect();
    let frames = published_frames(&config, 22_050, &signal);
    assert_eq!(frames.len(), 5);
    for (t, frame) in &frames {
        let fraction = frame.harmonic_fraction.unwrap();
        assert!(
            fraction >= 0.9999,
            "frame {t}: harmonic fraction {fraction}"
        );
    }
}

/// Every mask triple of the frame is exactly percussive, as are the fractions.
fn assert_exactly_percussive(t: usize, frame: &HpssFrame) {
    assert_eq!(frame.percussive_fraction, Some(1.0), "frame {t}");
    assert_eq!(frame.harmonic_fraction, Some(0.0), "frame {t}");
    for i in 0..frame.power.len() {
        assert_eq!(
            (
                frame.harmonic_mask[i],
                frame.percussive_mask[i],
                frame.residual_mask[i]
            ),
            (Some(0.0), Some(1.0), Some(0.0)),
            "frame {t} bin {i}"
        );
    }
}

/// O3: an impulse in digital silence. STFT frames 9..=12 contain sample 203
/// (never on a frame's first sample), and the 9-frame time median sees at
/// most four finite rows, so it stays `-inf`.
#[test]
fn o3_impulse_in_silence_is_exactly_percussive() {
    let config = oracle_config(9, 5);
    let mut signal = vec![0.0; 384];
    signal[203] = 1.0;
    let frames = published_frames(&config, 8_000, &signal);
    let centres: Vec<usize> = frames.iter().map(|(t, _)| *t).collect();
    assert_eq!(centres, (4..=16).collect::<Vec<_>>());
    for (t, frame) in &frames {
        if (9..=12).contains(t) {
            assert_exactly_percussive(*t, frame);
        } else {
            assert_zero_energy_frame(*t, frame);
        }
    }
}

/// O4: a click train with at most `K_t` click frames in any time window.
#[test]
fn o4_click_train_is_exactly_percussive() {
    let config = oracle_config(9, 5);
    let mut signal = vec![0.0; 704];
    for click in [43, 203, 363, 523] {
        signal[click] = 1.0;
    }
    let frames = published_frames(&config, 8_000, &signal);
    assert_eq!(frames.len(), 33);
    for (t, frame) in &frames {
        if [9..=12, 19..=22, 29..=32]
            .iter()
            .any(|range| range.contains(t))
        {
            assert_eq!(frame.percussive_fraction, Some(1.0), "frame {t}");
        } else {
            assert_eq!(fractions(frame), [None; 4], "frame {t}");
        }
    }
}

/// O5 exact partition and O6 fraction sum on the F1 gain-step noise.
#[test]
fn o5_o6_masks_partition_exactly_and_fractions_sum_to_one() {
    let config = oracle_config(5, 5);
    let frames = published_frames(&config, 8_000, &gain_step_noise(384));
    assert_eq!(frames.len(), 17);
    let mut defined = 0;
    for (t, frame) in &frames {
        for i in 0..frame.power.len() {
            let (Some(h), Some(p)) = (frame.harmonic_mask[i], frame.percussive_mask[i]) else {
                assert_eq!(frame.residual_mask[i], None, "frame {t} bin {i}");
                continue;
            };
            // O5: bitwise partition and exact zero residual at margin one.
            assert_eq!((h + p).to_bits(), 1.0f64.to_bits(), "frame {t} bin {i}");
            assert_eq!(frame.residual_mask[i], Some(0.0), "frame {t} bin {i}");
            defined += 1;
        }
        // O6: H + P + R + U = 1.
        let sum: f64 = fractions(frame).iter().map(|value| value.unwrap()).sum();
        assert!((sum - 1.0).abs() <= 1e-12, "frame {t}: {sum}");
    }
    assert!(defined > 0);
}

fn mask_kernel(margin: f64, mask_exponent: f64) -> HpssAnalyzer {
    let config = HpssConfig {
        margin,
        mask_exponent,
        ..oracle_config(5, 5)
    };
    HpssAnalyzer::new(&config, 8_000).unwrap()
}

/// O7: with equal finite medians the margin gives
/// `M_H = M_P = 1 / (1 + beta^p)` and `R = (beta^p - 1) / (beta^p + 1)`.
#[test]
fn o7_margin_residual_matches_closed_forms_and_is_monotone() {
    let third = 1.0 / 3.0;
    for (margin, exponent, expected) in [
        (2.0, 2.0, [0.2, 0.2, 0.6]),
        (3.0, 2.0, [0.1, 0.1, 0.8]),
        (2.0, 1.0, [third, third, third]),
    ] {
        let kernel = mask_kernel(margin, exponent);
        for log_power in [-40.0, -3.5, 0.0, 2.25, 30.0] {
            let (h, p, r) = kernel.masks(log_power, log_power);
            let actual = [h.unwrap(), p.unwrap(), r.unwrap()];
            for (value, reference) in actual.iter().zip(expected) {
                assert!(
                    (value - reference).abs() <= 1e-15,
                    "beta {margin} p {exponent} log {log_power}: {actual:?}"
                );
            }
        }
    }
    // Raising beta never raises either mask, over the full range including
    // extreme ratios, and `M_H + M_P + R = 1` within a few ulp.
    let kernels: Vec<HpssAnalyzer> = [1.0, 1.5, 2.0, 4.0]
        .into_iter()
        .map(|margin| mask_kernel(margin, 2.0))
        .collect();
    let mut state = 0x0bad_5eed_u64;
    let extremes = [(40.0, -40.0), (-40.0, 40.0), (20.0, -20.0), (-18.25, 18.5)];
    let random = (0..2_000).map(|_| {
        let a = 40.0 * lcg_uniform(&mut state);
        (a, 40.0 * lcg_uniform(&mut state))
    });
    for (a, b) in extremes.into_iter().chain(random) {
        let masks: Vec<(f64, f64, f64)> = kernels
            .iter()
            .map(|kernel| {
                let (h, p, r) = kernel.masks(a, b);
                (h.unwrap(), p.unwrap(), r.unwrap())
            })
            .collect();
        for pair in masks.windows(2) {
            assert!(
                pair[1].0 <= pair[0].0 && pair[1].1 <= pair[0].1,
                "a {a} b {b}: {masks:?}"
            );
        }
        for (h, p, r) in &masks {
            assert!(
                (h + p + r - 1.0).abs() <= 4.0 * f64::EPSILON,
                "a {a} b {b}: {masks:?}"
            );
        }
    }
    // The minority mask and the residual keep relative precision where the
    // complement `1 - M_H` would round them to zero (log ratio 40, p = 2).
    let tiny = 1.0 / (1.0 + 40.0f64.exp());
    let (h, p, r) = kernels[0].masks(20.0, -20.0);
    assert_eq!((h.unwrap(), r), (1.0, Some(0.0)));
    assert!((p.unwrap() / tiny - 1.0).abs() <= 1e-15, "{p:?} vs {tiny}");
    let (h, p, r) = kernels[2].masks(-20.0, 20.0);
    assert!((h.unwrap() / (tiny / 4.0) - 1.0).abs() <= 1e-15, "{h:?}");
    assert_eq!(p, Some(1.0));
    assert!((r.unwrap() / (3.75 * tiny) - 1.0).abs() <= 1e-15, "{r:?}");
}

/// O8: constant input is exactly zero after mean removal, so every published
/// frame has zero power and undefined masks, not 0.5.
#[test]
fn o8_dc_input_has_zero_power_and_undefined_masks() {
    let config = oracle_config(5, 5);
    let frames = published_frames(&config, 8_000, &[0.7; 384]);
    assert_eq!(frames.len(), 17);
    for (t, frame) in &frames {
        assert_zero_energy_frame(*t, frame);
    }
}

fn option_bits(values: &[Option<f64>]) -> Vec<Option<u64>> {
    values.iter().map(|value| value.map(f64::to_bits)).collect()
}

fn assert_bitwise_equal(actual: &HpssFrame, expected: &HpssFrame, context: &str) {
    assert_eq!(actual.end_sample, expected.end_sample, "{context}");
    for (actual, expected) in [
        (&actual.power, &expected.power),
        (&actual.harmonic_mask, &expected.harmonic_mask),
        (&actual.percussive_mask, &expected.percussive_mask),
        (&actual.residual_mask, &expected.residual_mask),
    ] {
        assert_eq!(option_bits(actual), option_bits(expected), "{context}");
    }
    assert_eq!(
        option_bits(&fractions(actual)),
        option_bits(&fractions(expected)),
        "{context}"
    );
}

/// O9: every observable frame is bitwise identical to the sample-by-sample
/// reference, whatever the chunking.
#[test]
fn o9_chunking_is_bitwise_invariant() {
    let config = oracle_config(5, 5);
    let signal = gain_step_noise(640);
    let mut analyzer = HpssAnalyzer::new(&config, 8_000).unwrap();
    let mut reference = Vec::new();
    for sample in &signal {
        if analyzer.push(std::slice::from_ref(sample)) == 1 {
            reference.push(analyzer.frame().unwrap().clone());
        }
    }
    let stft_frames = 1 + (signal.len() - 64) / 16;
    assert_eq!(reference.len(), stft_frames - 2 * 2);
    let last_end = 64 + (stft_frames - 1 - 2) * 16;
    assert_eq!(reference.last().unwrap().end_sample, last_end as u64);
    for chunk_size in [7, 64, 1000, signal.len()] {
        let mut analyzer = HpssAnalyzer::new(&config, 8_000).unwrap();
        let mut published = 0;
        for chunk in signal.chunks(chunk_size) {
            let updates = analyzer.push(chunk);
            if updates > 0 {
                published += updates;
                let context = format!("chunk {chunk_size}, publication {published}");
                let frame = analyzer.frame().unwrap();
                assert_bitwise_equal(frame, &reference[published - 1], &context);
            }
        }
        assert_eq!(published, reference.len(), "chunk {chunk_size}");
    }
}

/// O16: input times `2^m` keeps the `None` layout and moves masks and
/// fractions only by rounding (`2 ln(scale)` rounds differently).
#[test]
fn o16_masks_and_fractions_are_scale_covariant() {
    let config = oracle_config(5, 5);
    let signal = gain_step_noise(640);
    let reference = published_frames(&config, 8_000, &signal);
    for gain in [1024.0, 1.0 / 1024.0] {
        let scaled: Vec<f64> = signal.iter().map(|x| x * gain).collect();
        let frames = published_frames(&config, 8_000, &scaled);
        assert_eq!(frames.len(), reference.len());
        for ((t, frame), (_, expected)) in frames.iter().zip(&reference) {
            let (actual_fractions, expected_fractions) = (fractions(frame), fractions(expected));
            for (actual, expected) in [
                (&frame.harmonic_mask[..], &expected.harmonic_mask[..]),
                (&frame.percussive_mask[..], &expected.percussive_mask[..]),
                (&frame.residual_mask[..], &expected.residual_mask[..]),
                (&actual_fractions[..], &expected_fractions[..]),
            ] {
                for (a, e) in actual.iter().zip(expected) {
                    match (a, e) {
                        (None, None) => {}
                        (Some(a), Some(e)) => {
                            assert!((a - e).abs() <= 1e-12, "gain {gain} frame {t}: {a} vs {e}")
                        }
                        _ => panic!("gain {gain} frame {t}: {a:?} vs {e:?}"),
                    }
                }
            }
        }
    }
}

/// O10: nothing is published before `N + 2 K_t hop` samples, the first frame
/// describes centre `K_t`, and every further hop publishes exactly one frame.
#[test]
fn o10_warm_up_and_lag_follow_the_context_formula() {
    let config = oracle_config(5, 5);
    let signal = gain_step_noise(640);
    let mut analyzer = HpssAnalyzer::new(&config, 8_000).unwrap();
    assert_eq!(analyzer.lookahead_samples(), 32);
    assert_eq!(analyzer.push(&signal[..127]), 0);
    assert!(analyzer.frame().is_none());
    assert_eq!(analyzer.push(&signal[127..128]), 1);
    assert_eq!(analyzer.frame().unwrap().end_sample, 96);
    for (index, hop) in signal[128..].chunks(16).enumerate() {
        assert_eq!(analyzer.push(hop), 1);
        let end_sample = 96 + 16 * (index as u64 + 1);
        assert_eq!(analyzer.frame().unwrap().end_sample, end_sample);
    }
}

/// O11: one NaN at sample 200 invalidates STFT frames 9..=12, so centres
/// 7..=14 are entirely `None`; every other frame is defined again, and
/// `reset` re-arms the warm-up.
#[test]
fn o11_non_finite_input_contaminates_its_context_then_recovers() {
    let config = oracle_config(5, 5);
    let mut signal = gain_step_noise(640);
    signal[200] = f64::NAN;
    let frames = published_frames(&config, 8_000, &signal);
    assert_eq!(frames.len(), 33);
    for (t, frame) in &frames {
        let valid = !(7..=14).contains(t);
        for values in [
            &frame.power,
            &frame.harmonic_mask,
            &frame.percussive_mask,
            &frame.residual_mask,
        ] {
            assert!(
                values.iter().all(|value| value.is_some() == valid),
                "frame {t}"
            );
        }
        let defined = fractions(frame).map(|value| value.is_some());
        assert_eq!(defined, [valid; 4], "frame {t}");
    }
    let mut analyzer = HpssAnalyzer::new(&config, 8_000).unwrap();
    analyzer.push(&signal);
    assert!(analyzer.frame().is_some());
    analyzer.reset();
    assert!(analyzer.frame().is_none());
    let clean = gain_step_noise(128);
    assert_eq!(analyzer.push(&clean[..127]), 0);
    assert!(analyzer.frame().is_none());
    assert_eq!(analyzer.push(&clean[127..]), 1);
    assert_eq!(analyzer.frame().unwrap().end_sample, 96);
}

/// O12: the first push after construction, a primed push, the accessors and
/// `reset` do not allocate.
#[test]
fn o12_streaming_calls_do_not_allocate() {
    let config = oracle_config(5, 5);
    let signal = gain_step_noise(640);
    let mut analyzer = HpssAnalyzer::new(&config, 8_000).unwrap();
    let mut updates = (0, 0);
    assert_no_alloc::assert_no_alloc(|| {
        updates.0 = analyzer.push(&signal[..320]);
        updates.1 = analyzer.push(&signal[320..]);
        std::hint::black_box(analyzer.frame());
        std::hint::black_box(analyzer.lookahead_samples());
        std::hint::black_box(analyzer.sample_rate_hz());
        analyzer.reset();
    });
    assert_eq!(updates, (13, 20));
    assert!(analyzer.frame().is_none());
}

/// O12: every invalid configuration class returns its error before allocating.
#[test]
fn o12_invalid_configuration_is_rejected_before_allocating() {
    fn reject(config: &HpssConfig, rate: u32, class: &str) {
        assert_no_alloc::assert_no_alloc(|| {
            let error = HpssAnalyzer::new(config, rate).err().unwrap();
            match class {
                "geometry" => assert!(matches!(error, ProcessError::InvalidGeometry { .. })),
                "rate" => assert!(matches!(error, ProcessError::InvalidSampleRate { .. })),
                _ => assert!(
                    matches!(error, ProcessError::InvalidParameter { parameter, .. } if parameter == class)
                ),
            }
        });
    }
    let base = oracle_config(5, 5);
    reject(&base, 0, "rate");
    for fft_size in [2, 48, 0] {
        reject(&HpssConfig { fft_size, ..base }, 8_000, "geometry");
    }
    for hop_size in [0, 65] {
        reject(&HpssConfig { hop_size, ..base }, 8_000, "geometry");
    }
    for harmonic_median_frames in [1, 2, 4] {
        let config = HpssConfig {
            harmonic_median_frames,
            ..base
        };
        reject(&config, 8_000, "harmonic_median_frames");
    }
    for percussive_median_bins in [1, 2, 4, 33] {
        let config = HpssConfig {
            percussive_median_bins,
            ..base
        };
        reject(&config, 8_000, "percussive_median_bins");
    }
    for mask_exponent in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let config = HpssConfig {
            mask_exponent,
            ..base
        };
        reject(&config, 8_000, "mask_exponent");
    }
    for margin in [0.5, f64::NAN, f64::INFINITY] {
        reject(&HpssConfig { margin, ..base }, 8_000, "margin");
    }
    for beta in [f64::NAN, -1.0, 51.0] {
        let window = WindowFunction::Kaiser { beta };
        reject(&HpssConfig { window, ..base }, 8_000, "window.beta");
    }
    // Odd and >= 3, but the context storage is not representable.
    let config = HpssConfig {
        harmonic_median_frames: usize::MAX,
        ..base
    };
    reject(&config, 8_000, "geometry");
}

/// Descriptor bit identity (no design ID): HPSS `power` equals the descriptor
/// analyzer's absolute power spectrum of the same STFT frame, bitwise.
#[test]
fn hpss_power_is_bit_identical_to_descriptor_power_spectrum() {
    let config = oracle_config(5, 5);
    let signal = gain_step_noise(640);
    let descriptor_config = DescriptorConfig {
        fft_size: 64,
        hop_size: 16,
        window: WindowFunction::Hann,
        ..DescriptorConfig::default()
    };
    let mut descriptors = DescriptorAnalyzer::new(&descriptor_config, 8_000).unwrap();
    let mut spectra = Vec::new();
    for hop in signal.chunks(16) {
        if descriptors.push(hop) == 1 {
            spectra.push(descriptors.power_spectrum().unwrap().to_vec());
        }
    }
    let frames = published_frames(&config, 8_000, &signal);
    assert_eq!(frames.len(), spectra.len() - 4);
    for (t, frame) in &frames {
        for (i, power) in frame.power.iter().enumerate() {
            let expected = spectra[*t][i];
            assert_eq!(
                power.map(f64::to_bits),
                Some(expected.to_bits()),
                "frame {t} bin {i}: {power:?} vs {expected}"
            );
        }
    }
}
