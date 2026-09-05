//! Stateless, low-level DSP mathematics shared by analysis and processors.
//!
//! This module intentionally contains only pure helpers with no callback state,
//! allocation, or parameter publication. Stateful realtime processors remain
//! under [`crate::processor`].

/// Convert a decibel value to a linear amplitude multiplier.
#[inline(always)]
pub fn db_to_linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

/// Convert a linear amplitude multiplier to decibels.
///
/// Non-positive values map to negative infinity, matching the historical
/// processor helper and preserving silence semantics.
#[inline(always)]
pub fn linear_to_db(linear: f64) -> f64 {
    if linear > 0.0 {
        20.0 * linear.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Modified Bessel function of the first kind, order zero.
///
/// Windowed FIR and spectrum designs call this bounded series during setup.
/// Keeping the pure scalar helper here lets analysis and processors share the
/// implementation without making either layer depend on the other.
pub(crate) fn modified_bessel_i0(value: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half_squared = (value * value) * 0.25;
    for order in 1..=64 {
        term *= half_squared / (order as f64 * order as f64);
        sum += term;
        if term.abs() <= sum.abs() * 1.0e-16 {
            break;
        }
    }
    sum
}

/// Design a Kaiser-windowed half-band lowpass FIR filter kernel.
///
/// Returns a symmetric kernel of length `taps` with unity DC gain, zero at even
/// offsets (except the center), and ideal half-band sinc shape at odd offsets.
/// The design spec targets passband edge at 0.2·fs_in (= 0.4·fs_out) and
/// stopband edge at 0.3·fs_in with symmetric response about fs_in/4.
///
/// `taps` must be odd with `(taps - 1) / 2` also odd (i.e., taps = 4m + 3) so
/// that the outermost taps are nonzero, preserving the symmetric half-band
/// structure.
///
/// # Panics
///
/// Panics if `taps` is even or if `(taps - 1) / 2` is even.
pub(crate) fn halfband_lowpass_kernel(taps: usize, beta: f64) -> Vec<f64> {
    assert!(
        taps % 2 == 1 && ((taps - 1) / 2) % 2 == 1,
        "taps must be odd with (taps - 1) / 2 also odd (taps = 4m + 3)"
    );

    let center = taps / 2;
    let mut kernel = vec![0.0; taps];

    // Kaiser window
    let i0_beta = modified_bessel_i0(beta);
    for (i, coeff) in kernel.iter_mut().enumerate() {
        let x = 2.0 * i as f64 / (taps - 1) as f64 - 1.0; // -1 to 1
        let arg = beta * (1.0 - x * x).sqrt();
        let window = modified_bessel_i0(arg) / i0_beta;

        // Ideal half-band sinc
        let d = i as isize - center as isize;
        let h_ideal = if d == 0 {
            0.5
        } else if d.abs() % 2 == 0 {
            0.0
        } else {
            let arg = std::f64::consts::PI * d as f64 * 0.5;
            arg.sin() / (std::f64::consts::PI * d as f64)
        };

        *coeff = h_ideal * window;
    }

    // Normalize to unity DC gain while preserving exact zeros at even offsets.
    // Sum only the non-zero (odd offset) coefficients.
    let mut sum = 0.0;
    for (i, &coeff) in kernel.iter().enumerate() {
        let d = i as isize - center as isize;
        if d == 0 || d.abs() % 2 != 0 {
            sum += coeff;
        }
    }

    if sum.abs() > 1e-12 {
        for (i, coeff) in kernel.iter_mut().enumerate() {
            let d = i as isize - center as isize;
            if d == 0 || d.abs() % 2 != 0 {
                *coeff /= sum;
            }
        }
    }

    kernel
}

#[cfg(test)]
mod tests {
    use super::{db_to_linear, linear_to_db, modified_bessel_i0};

    #[test]
    fn conversions_match_the_audio_gain_contract() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-12);
        assert!((db_to_linear(-6.0) - 0.501).abs() < 0.01);
        assert!((linear_to_db(1.0) - 0.0).abs() < 1e-12);
        assert!((linear_to_db(0.5) + 6.0206).abs() < 0.01);
        assert!(linear_to_db(0.0).is_infinite());
        assert!(linear_to_db(-1.0).is_infinite());
    }

    #[test]
    fn modified_bessel_matches_known_values() {
        assert_eq!(modified_bessel_i0(0.0), 1.0);
        assert!((modified_bessel_i0(1.0) - 1.266_065_877_752_008_2).abs() < 1.0e-14);
        assert!((modified_bessel_i0(14.0) - 129_418.562_700_648_56).abs() < 1.0e-8);
    }

    #[test]
    fn halfband_kernel_is_symmetric_with_unity_dc_gain() {
        let kernel = super::halfband_lowpass_kernel(63, 9.6);
        assert_eq!(kernel.len(), 63);

        // Symmetric
        for i in 0..31 {
            assert!((kernel[i] - kernel[62 - i]).abs() < 1e-15);
        }

        // Unity DC gain
        let sum: f64 = kernel.iter().sum();
        assert!((sum - 1.0).abs() < 1e-12);

        // Even offsets from center are zero (except center itself)
        let center = 31;
        for (i, &coeff) in kernel.iter().enumerate() {
            let offset = i as isize - center as isize;
            if offset != 0 && offset.abs() % 2 == 0 {
                assert!(
                    coeff.abs() < 1e-12,
                    "kernel[{}] (offset {}) = {} exceeds tolerance",
                    i,
                    offset,
                    coeff
                );
            }
        }

        // Center coefficient should be close to 0.5 (but not exact after normalization)
        assert!(
            (kernel[center] - 0.5).abs() < 0.001,
            "kernel[{}] = {}, expected ~0.5",
            center,
            kernel[center]
        );
    }

    #[test]
    fn halfband_kernel_measured_frequency_response() {
        use std::f64::consts::PI;

        let kernel = super::halfband_lowpass_kernel(63, 9.6);
        let n_fft = 8192;

        // Zero-padded DTFT
        let mut max_passband_deviation_db: f64 = 0.0;
        let mut max_stopband_level_db = f64::NEG_INFINITY;

        for k in 0..n_fft / 2 {
            let omega = 2.0 * PI * k as f64 / n_fft as f64;
            let mut re = 0.0;
            let mut im = 0.0;
            for (n, &h) in kernel.iter().enumerate() {
                let arg = omega * n as f64;
                re += h * arg.cos();
                im += h * arg.sin();
            }
            let mag = (re * re + im * im).sqrt();
            let mag_db = 20.0 * mag.log10();

            // Normalized frequency: omega / (2*pi) = k / n_fft
            let f_norm = k as f64 / n_fft as f64;

            if f_norm <= 0.2 {
                // Passband: deviation from unity
                max_passband_deviation_db = max_passband_deviation_db.max(mag_db.abs());
            } else if f_norm >= 0.3 {
                // Stopband
                max_stopband_level_db = max_stopband_level_db.max(mag_db);
            }
        }

        // Conservative gates per design doc: stopband ≥ 90 dB, passband ≤ 0.01 dB
        assert!(
            max_stopband_level_db <= -90.0,
            "Stopband attenuation {} dB, expected ≤ -90 dB",
            max_stopband_level_db
        );
        assert!(
            max_passband_deviation_db <= 0.01,
            "Passband deviation {} dB, expected ≤ 0.01 dB",
            max_passband_deviation_db
        );
    }
}
