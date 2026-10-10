//! Bounded-angle probe signals and small compensated sinusoid fits.

use std::f64::consts::TAU;

pub(crate) const DB_DISPLAY_FLOOR: f64 = -400.0;
const MAX_COLUMNS: usize = 5;

#[derive(Clone, Copy, Default)]
struct Sum {
    value: f64,
    correction: f64,
}

impl Sum {
    fn add(&mut self, value: f64) {
        let corrected = value - self.correction;
        let next = self.value + corrected;
        self.correction = (next - self.value) - corrected;
        self.value = next;
    }
}

pub(crate) fn tone_sample(frame: usize, rate: u32, hz: u32, amplitude: f64, phase: f64) -> f64 {
    amplitude * (angle(frame, rate, hz) + phase).sin()
}

fn angle(frame: usize, rate: u32, hz: u32) -> f64 {
    let residue = ((frame as u128) * u128::from(hz)) % u128::from(rate);
    TAU * residue as f64 / f64::from(rate)
}

pub(crate) fn fold_frequency(hz: u32, rate: u32) -> u32 {
    let residue = hz % rate;
    residue.min(rate - residue)
}

pub(crate) fn db_ratio(numerator: f64, denominator: f64) -> Result<f64, String> {
    if !numerator.is_finite() || !denominator.is_finite() || numerator < 0.0 || denominator <= 0.0 {
        return Err(format!("invalid dB ratio {numerator}/{denominator}"));
    }
    Ok(if numerator == 0.0 {
        DB_DISPLAY_FLOOR
    } else {
        (20.0 * (numerator / denominator).log10()).max(DB_DISPLAY_FLOOR)
    })
}

#[derive(Clone, Debug)]
pub(crate) struct Component {
    pub(crate) amplitude: f64,
    pub(crate) phase_radians: Option<f64>,
    pub(crate) rms: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct Fit {
    pub(crate) components: Vec<Component>,
    pub(crate) residual_rms: f64,
    pub(crate) frames: usize,
}

/// Fits one tone + DC, or fundamental + image + DC simultaneously. Boundary
/// components use a single real column, avoiding singular DC/Nyquist fits.
#[allow(clippy::too_many_arguments, clippy::needless_range_loop)]
pub(crate) fn fit_components(
    samples: &[f64],
    channels: usize,
    channel: usize,
    rate: u32,
    frequencies: &[u32],
    start: usize,
    count: usize,
) -> Result<Fit, String> {
    if channels == 0
        || channel >= channels
        || !samples.len().is_multiple_of(channels)
        || rate == 0
        || frequencies.is_empty()
        || frequencies.len() > 2
        || frequencies
            .iter()
            .any(|&hz| u64::from(hz) * 2 > u64::from(rate))
        || (frequencies.len() == 2 && frequencies[0] == frequencies[1])
    {
        return Err("invalid geometry/frequencies for component fit".to_string());
    }
    let available = (samples.len() / channels).saturating_sub(start);
    let count = count.min(available);
    if count < 32 {
        return Err("not enough samples for component fit".to_string());
    }
    let mut columns = Vec::new();
    let mut components = Vec::new();
    for &hz in frequencies {
        let first = columns.len();
        let boundary = hz == 0 || u64::from(hz) * 2 == u64::from(rate);
        if boundary {
            columns.push((hz, true));
        } else {
            columns.push((hz, false));
            columns.push((hz, true));
        }
        components.push((first, boundary));
    }
    if !frequencies.contains(&0) {
        columns.push((0, true));
    }
    let width = columns.len();
    let basis_at = |frame: usize| {
        let mut basis = [0.0; MAX_COLUMNS];
        for (index, &(hz, cosine)) in columns.iter().enumerate() {
            basis[index] = if hz == 0 {
                1.0
            } else if u64::from(hz) * 2 == u64::from(rate) {
                if frame.is_multiple_of(2) {
                    1.0
                } else {
                    -1.0
                }
            } else {
                let phase = angle(frame, rate, hz);
                if cosine {
                    phase.cos()
                } else {
                    phase.sin()
                }
            };
        }
        basis
    };
    let mut matrix = [[Sum::default(); MAX_COLUMNS]; MAX_COLUMNS];
    let mut rhs = [Sum::default(); MAX_COLUMNS];
    for frame in start..start + count {
        let basis = basis_at(frame);
        let sample = samples[frame * channels + channel];
        if !sample.is_finite() {
            return Err("non-finite fit input".to_string());
        }
        for row in 0..width {
            rhs[row].add(basis[row] * sample);
            for col in row..width {
                matrix[row][col].add(basis[row] * basis[col]);
            }
        }
    }
    let mut normal = [[0.0; MAX_COLUMNS]; MAX_COLUMNS];
    for row in 0..width {
        for col in row..width {
            normal[row][col] = matrix[row][col].value;
            normal[col][row] = normal[row][col];
        }
    }
    let coeffs = solve(
        normal,
        rhs.map(|sum| sum.value),
        width,
        count as f64 * 1.0e-14,
    )?;
    let mut residual = Sum::default();
    for frame in start..start + count {
        let basis = basis_at(frame);
        let fitted = (0..width).map(|i| coeffs[i] * basis[i]).sum::<f64>();
        let error = samples[frame * channels + channel] - fitted;
        residual.add(error * error);
    }
    Ok(Fit {
        components: components
            .into_iter()
            .map(|(first, boundary)| {
                let amplitude = if boundary {
                    coeffs[first].abs()
                } else {
                    coeffs[first].hypot(coeffs[first + 1])
                };
                Component {
                    amplitude,
                    phase_radians: (!boundary).then(|| coeffs[first + 1].atan2(coeffs[first])),
                    rms: if boundary {
                        amplitude
                    } else {
                        amplitude / 2.0_f64.sqrt()
                    },
                }
            })
            .collect(),
        residual_rms: (residual.value / count as f64).sqrt(),
        frames: count,
    })
}

#[allow(clippy::needless_range_loop)]
fn solve(
    mut matrix: [[f64; MAX_COLUMNS]; MAX_COLUMNS],
    mut rhs: [f64; MAX_COLUMNS],
    width: usize,
    floor: f64,
) -> Result<[f64; MAX_COLUMNS], String> {
    for pivot in 0..width {
        let best = (pivot..width)
            .max_by(|&a, &b| matrix[a][pivot].abs().total_cmp(&matrix[b][pivot].abs()))
            .ok_or_else(|| "empty fit matrix".to_string())?;
        if matrix[best][pivot].abs() <= floor {
            return Err("singular component-fit matrix".to_string());
        }
        matrix.swap(best, pivot);
        rhs.swap(best, pivot);
        let scale = matrix[pivot][pivot];
        for col in pivot..width {
            matrix[pivot][col] /= scale;
        }
        rhs[pivot] /= scale;
        for row in 0..width {
            if row == pivot {
                continue;
            }
            let factor = matrix[row][pivot];
            for col in pivot..width {
                matrix[row][col] -= factor * matrix[pivot][col];
            }
            rhs[row] -= factor * rhs[pivot];
        }
    }
    Ok(rhs)
}

pub(crate) fn channel_rms(
    samples: &[f64],
    channels: usize,
    channel: usize,
    start: usize,
    count: usize,
) -> Result<f64, String> {
    if channels == 0
        || channel >= channels
        || !samples.len().is_multiple_of(channels)
        || count == 0
        || start
            .checked_add(count)
            .is_none_or(|end| end > samples.len() / channels)
    {
        return Err("invalid RMS geometry".to_string());
    }
    let mut sum = Sum::default();
    for frame in start..start + count {
        let value = samples[frame * channels + channel];
        if !value.is_finite() {
            return Err("non-finite RMS input".to_string());
        }
        sum.add(value * value);
    }
    Ok((sum.value / count as f64).sqrt())
}

#[cfg(test)]
#[allow(unused_imports)] // Custom-harness benches also compile with cfg(test).
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    #[test]
    fn rational_oscillator_preserves_phase_at_large_frame_indices() {
        let index = usize::MAX - 96_000;
        assert_eq!(
            tone_sample(index, 48_000, 997, 0.5, 0.0).to_bits(),
            tone_sample(index + 48_000, 48_000, 997, 0.5, 0.0).to_bits()
        );
    }

    #[test]
    fn analyzer_resolves_known_distortion_gain_phase_and_dc() {
        for ratio in [1.0e-6, 1.0e-10, 1.0e-12] {
            let samples = (0..48_000)
                .map(|n| {
                    0.125
                        + tone_sample(n, 48_000, 997, 0.5, 0.31)
                        + tone_sample(n, 48_000, 1994, 0.5 * ratio, 0.73)
                })
                .collect::<Vec<_>>();
            let fit = fit_components(&samples, 1, 0, 48_000, &[997], 0, 48_000).unwrap();
            assert!((fit.components[0].amplitude - 0.5).abs() < 1.0e-14);
            assert!((fit.components[0].phase_radians.unwrap() - 0.31).abs() < 1.0e-14);
            let measured = db_ratio(fit.residual_rms, fit.components[0].rms).unwrap();
            assert!(
                (measured - 20.0 * ratio.log10()).abs() < 0.03,
                "ratio {ratio}: {measured}"
            );
        }
    }

    #[test]
    fn joint_image_fit_rejects_fundamental_leakage_and_handles_nyquist() {
        for image_hz in [45_097, 48_000] {
            let samples = (0..9600)
                .map(|n| {
                    tone_sample(n, 96_000, 997, 0.5, 0.37)
                        + tone_sample(n, 96_000, image_hz, 1.0e-10, FRAC_PI_2)
                })
                .collect::<Vec<_>>();
            let fit = fit_components(&samples, 1, 0, 96_000, &[997, image_hz], 257, 9000).unwrap();
            assert!((fit.components[1].amplitude - 1.0e-10).abs() < 1.0e-15);
        }
    }

    #[test]
    fn quadrature_energy_does_not_disappear_at_dc_or_nyquist() {
        for hz in [0, 24_000] {
            for phase in [0.0, 0.37, 1.12] {
                let energies = [phase, phase + FRAC_PI_2].map(|p| {
                    let samples = (0..1000)
                        .map(|n| tone_sample(n, 48_000, hz, 0.5, p))
                        .collect::<Vec<_>>();
                    channel_rms(&samples, 1, 0, 0, samples.len())
                        .unwrap()
                        .powi(2)
                });
                assert!((energies.into_iter().sum::<f64>().sqrt() / 0.5 - 1.0).abs() < 1.0e-14);
            }
        }
    }

    #[test]
    fn channel_selection_is_independent() {
        let samples = (0..2048)
            .flat_map(|n| [0.0, tone_sample(n, 48_000, 997, 0.5, 0.0)])
            .collect::<Vec<_>>();
        assert_eq!(channel_rms(&samples, 2, 0, 0, 2048).unwrap(), 0.0);
        let fit = fit_components(&samples, 2, 1, 48_000, &[997], 0, 2048).unwrap();
        assert!((fit.components[0].amplitude - 0.5).abs() < 1.0e-14);
    }
}
