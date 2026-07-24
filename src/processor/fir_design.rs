//! Setup-time FIR design helpers shared by the EQ and resampler modules.

use rustfft::{num_complex::Complex, FftPlanner};

/// Convert a Hermitian log-magnitude spectrum into a causal minimum-phase IR.
///
/// The input contains natural-log magnitude for every FFT bin, with negative
/// frequency bins already mirrored. This helper is setup-only: it allocates
/// and plans FFTs, so callers must not invoke it from a processing callback.
pub(crate) fn minimum_phase_from_log_magnitude(
    log_magnitude: &[f64],
    output_len: usize,
) -> Vec<f64> {
    if output_len == 0 || log_magnitude.is_empty() {
        return Vec::new();
    }

    let fft_size = log_magnitude.len();
    let mut spectrum: Vec<Complex<f64>> = log_magnitude
        .iter()
        .map(|&value| Complex::new(value, 0.0))
        .collect();

    let mut planner = FftPlanner::new();
    let ifft = planner.plan_fft_inverse(fft_size);
    ifft.process(&mut spectrum);

    // rustfft's inverse transform is intentionally unnormalised.
    let inverse_scale = 1.0 / fft_size as f64;
    for value in &mut spectrum {
        *value *= inverse_scale;
    }

    // Keep the causal cepstrum: DC and Nyquist stay unchanged, positive
    // quefrencies are doubled, and negative quefrencies are discarded.
    let half = fft_size / 2;
    for (index, value) in spectrum.iter_mut().enumerate() {
        if index == 0 || index == half {
            continue;
        }
        if index < half {
            *value *= 2.0;
        } else {
            *value = Complex::new(0.0, 0.0);
        }
    }

    let fft = planner.plan_fft_forward(fft_size);
    fft.process(&mut spectrum);
    for value in &mut spectrum {
        *value = value.exp();
    }

    ifft.process(&mut spectrum);
    let output_len = output_len.min(fft_size);
    spectrum[..output_len]
        .iter()
        .map(|value| value.re * inverse_scale)
        .collect()
}
