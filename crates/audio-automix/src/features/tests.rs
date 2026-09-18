use super::*;

#[test]
fn onset_bands_partition_resolved_bins_without_area_normalization() {
    for sample_rate in [8_000, 22_050, 44_100, 48_000, 96_000, 192_000] {
        let fft_size = spectral_frame_size(sample_rate);
        let bands = onset_bands(sample_rate, fft_size);
        assert!(bands.len() > 2);
        let last_bin = ((16_000.0_f64.min(f64::from(sample_rate) / 2.0) * fft_size as f64
            / f64::from(sample_rate))
        .round() as usize)
            .min(fft_size / 2);
        let mut summed_weights = vec![0.0; fft_size / 2];
        let mut centers = Vec::new();
        for band in &bands {
            assert!(band.start_bin + band.weights.len() <= last_bin);
            let center = band
                .weights
                .iter()
                .position(|&weight| weight == 1.0)
                .unwrap();
            centers.push(band.start_bin + center);
            // The discrete integral of a unit-height triangle equals
            // half its base width; wider bands must keep their area.
            let area = band.weights.iter().sum::<f32>();
            assert!((area - band.weights.len() as f32 / 2.0).abs() < 1e-5);
            for (offset, &weight) in band.weights.iter().enumerate() {
                assert!((0.0..=1.0).contains(&weight));
                summed_weights[band.start_bin + offset] += weight;
            }
        }
        for &weight in &summed_weights[centers[0]..=*centers.last().unwrap()] {
            assert!((weight - 1.0).abs() < 1e-6, "rate={sample_rate}, {weight}");
        }
    }
    assert!(onset_bands(0, spectral_frame_size(0)).is_empty());
    assert!(onset_bands(50, spectral_frame_size(50)).is_empty());
}

#[test]
fn spectral_flux_clock_locates_an_isolated_impulse_across_sample_rates() {
    for sample_rate in [22_050, 44_100, 48_000, 96_000, 192_000] {
        let size = spectral_frame_size(sample_rate);
        let hop = spectral_hop_size(sample_rate);
        let impulse = 2 * size + hop / 3 + 37;
        let mut accumulator = SpectralFluxAccumulator::new(sample_rate);
        let flux: Vec<_> = (0..4 * size)
            .filter_map(|frame| accumulator.process(if frame == impulse { 1e-6 } else { 0.0 }))
            .collect();
        let peak = (1..flux.len() - 1)
            .max_by(|&left, &right| flux[left].total_cmp(&flux[right]))
            .unwrap();
        let (left, center, right) = (flux[peak - 1], flux[peak], flux[peak + 1]);
        let delta = 0.5 * (left - right) / (left - 2.0 * center + right);
        let observed = (peak as f64 + f64::from(delta)) * hop as f64 / f64::from(sample_rate)
            + spectral_observation_offset_sec(sample_rate);
        let expected = impulse as f64 / f64::from(sample_rate);
        assert!(
            (observed - expected).abs() < 0.002,
            "rate={sample_rate}, error={}",
            observed - expected
        );
    }
    assert_eq!(spectral_frame_size(0), 1024);
    assert_eq!(spectral_frame_size(u32::MAX), 8192);
}

/// Reference spectral-flux implementation using a full complex FFT — the
/// formulation this module used before moving to `realfft`.
///
/// Deliberately built on `rustfft` so it remains an independent oracle.
fn reference_spectral_flux(samples: &[f32], sample_rate: u32, fft_size: usize) -> Vec<f32> {
    use rustfft::{num_complex::Complex32, FftPlanner};

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);
    let mut frame = vec![Complex32::new(0.0, 0.0); fft_size];
    let bands = onset_bands(sample_rate, fft_size);
    let mut previous = vec![0.0f32; bands.len()];
    let mut scratch = vec![0.0f32; fft_size];
    let mut pos = 0usize;
    let mut out = Vec::new();
    let hop = (sample_rate as usize / 200).clamp(1, 512);

    for &sample in samples {
        scratch[pos] = sample;
        pos += 1;
        if pos < fft_size {
            continue;
        }
        for i in 0..fft_size {
            let window =
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (fft_size - 1) as f32).cos();
            frame[i] = Complex32::new(scratch[i] * window, 0.0);
        }
        fft.process(&mut frame);

        let mut flux = 0.0;
        for (band, previous) in bands.iter().zip(&mut previous) {
            let magnitude = band
                .weights
                .iter()
                .enumerate()
                .map(|(offset, weight)| frame[band.start_bin + offset].norm() * weight)
                .sum::<f32>()
                .ln_1p();
            flux += (magnitude - *previous).max(0.0);
            *previous = magnitude;
        }
        scratch.copy_within(hop..fft_size, 0);
        pos = fft_size - hop;
        out.push(flux / bands.len().max(1) as f32);
    }
    out
}

/// The real forward transform must reproduce the complex formulation's flux
/// sequence. This matters beyond raw magnitudes: flux is differential and
/// carries `previous_magnitudes` across hops, so a per-bin indexing mistake
/// would accumulate rather than cancel.
///
/// Accumulating band differences in f32 makes bit-exactness
/// unrealistic; the tolerance is relative to the largest reference flux.
#[test]
fn cached_hann_window_is_bit_identical_to_evaluating_it_per_hop() {
    // The accumulator used to rebuild this window with 1,024 `cos()` calls on
    // every hop. Caching it is only a performance change if every cached
    // coefficient is the exact same `f32`, so compare bit patterns rather
    // than using a tolerance: a tolerance here would hide a real change in
    // the reported flux.
    for fft_size in [1024, 2048, 4096, 8192] {
        let cached = hann_window(fft_size);
        assert_eq!(cached.len(), fft_size);
        for (i, &coefficient) in cached.iter().enumerate() {
            let per_hop =
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (fft_size - 1) as f32).cos();
            assert_eq!(
                coefficient.to_bits(),
                per_hop.to_bits(),
                "size={fft_size}, window[{i}]: cached {coefficient} vs per-hop {per_hop}"
            );
        }
    }
}

#[test]
fn spectral_flux_matches_complex_reference_formulation() {
    // Level and timbre both change over time so flux is genuinely non-zero:
    // a steady tone settles to ~0 flux after the first hop and would let an
    // indexing bug pass unnoticed.
    for (sample_rate, fft_size) in [
        (22_050, 1024),
        (44_100, 2048),
        (48_000, 2048),
        (96_000, 4096),
        (192_000, 8192),
    ] {
        let samples: Vec<f32> = (0..fft_size * 12)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                let envelope = 0.2 + 0.8 * ((i / (fft_size * 3)) % 3) as f32 / 2.0;
                let sweep = 220.0 + 400.0 * (i as f32 / (fft_size * 12) as f32);
                envelope
                    * ((2.0 * std::f32::consts::PI * sweep * t).sin() * 0.6
                        + (2.0 * std::f32::consts::PI * 3.0 * sweep * t).sin() * 0.3)
            })
            .collect();

        let expected = reference_spectral_flux(&samples, sample_rate, fft_size);
        let mut accumulator = SpectralFluxAccumulator::new(sample_rate);
        let actual: Vec<f32> = samples
            .iter()
            .filter_map(|&sample| accumulator.process(sample))
            .collect();

        assert_eq!(actual.len(), expected.len());
        assert!(expected.len() >= 8, "fixture must produce several hops");

        let peak = expected.iter().fold(0.0f32, |acc, f| acc.max(f.abs()));
        assert!(peak > 0.0, "reference flux must not be all zeros");
        let tolerance = peak * 1e-4;

        for (hop, (got, want)) in actual.iter().zip(&expected).enumerate() {
            assert!(
                (got - want).abs() <= tolerance,
                "hop {hop}: {got} vs {want} (diff {:.3e} > tol {:.3e})",
                (got - want).abs(),
                tolerance
            );
        }
    }
}
