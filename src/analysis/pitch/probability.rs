//! Threshold-prior discretization and the pYIN paper's first-dip rule.
//! Implemented from the Beta integral/continued fraction and paper equations;
//! no Vamp/aubio implementation is used.

use super::{interpolated_minimum, vertex_period, PitchCandidate};

/// log(Beta(a,b)), shifting small arguments into the Stirling region.
/// Ratio/log1p forms avoid subtracting three enormous log-gamma values.
fn log_beta(mut a: f64, mut b: f64) -> f64 {
    let mut shift = 0.0;
    while a < 8.0 {
        shift += (a + b).ln() - a.ln();
        a += 1.0;
    }
    while b < 8.0 {
        shift += (a + b).ln() - b.ln();
        b += 1.0;
    }
    let sum = a + b;
    fn correction(x: f64) -> f64 {
        let z = 1.0 / (x * x);
        (1.0 / 12.0
            + z * (-1.0 / 360.0
                + z * (1.0 / 1260.0
                    + z * (-1.0 / 1680.0
                        + z * (1.0 / 1188.0 + z * (-691.0 / 360360.0 + z / 156.0))))))
            / x
    }
    shift - (a - 0.5) * (b / a).ln_1p() - (b - 0.5) * (a / b).ln_1p() - 0.5 * sum.ln()
        + 0.5 * (2.0 * std::f64::consts::PI).ln()
        + correction(a)
        + correction(b)
        - correction(sum)
}

/// Modified Lentz evaluation of the incomplete-Beta continued fraction
/// (DLMF 8.17.22). Setup only; failure is a typed constructor error.
fn beta_fraction(a: f64, b: f64, x: f64) -> Option<f64> {
    fn nonzero(v: f64) -> f64 {
        if v.abs() < 1e-300 {
            1e-300_f64.copysign(v)
        } else {
            v
        }
    }
    let mut c = 1.0;
    let mut d = 1.0 / nonzero(1.0 - (a + b) / (a + 1.0) * x);
    let mut h = d;
    for m in 1..=512 {
        let m = f64::from(m);
        let twice = 2.0 * m;
        let even = (m / (a + twice)) * ((b - m) / (a - 1.0 + twice)) * x;
        let odd = -((a + m) / (a + twice)) * ((a + b + m) / (a + 1.0 + twice)) * x;
        let mut delta = 1.0;
        for coefficient in [even, odd] {
            d = 1.0 / nonzero(1.0 + coefficient * d);
            c = nonzero(1.0 + coefficient / c);
            delta = d * c;
            h *= delta;
        }
        if !h.is_finite() || h <= 0.0 {
            return None;
        }
        if (delta - 1.0).abs() <= 4.0 * f64::EPSILON {
            return Some(h);
        }
    }
    None
}

fn beta_cdf(x: f64, a: f64, b: f64, log_normalizer: f64) -> Option<f64> {
    if x == 0.0 {
        return Some(0.0);
    }
    if x == 1.0 {
        return Some(1.0);
    }
    // Exact elementary forms keep the default (2,18) and uniform prior
    // independent of a special-function approximation.
    if a == 1.0 {
        return Some(-(b * (-x).ln_1p()).exp_m1());
    }
    if b == 1.0 {
        return Some(x.powf(a));
    }
    if a == 2.0 {
        return Some(-(b * (-x).ln_1p() + (b * x).ln_1p()).exp_m1());
    }
    let factor = (a * x.ln() + b * (-x).ln_1p() - log_normalizer).exp();
    let result = if x < (a + 1.0) / (a + b + 2.0) {
        factor * beta_fraction(a, b, x)? / a
    } else {
        1.0 - factor * beta_fraction(b, a, 1.0 - x)? / b
    };
    result.is_finite().then_some(result)
}

/// P(s_i) = F(i/100) - F((i-1)/100), entirely on the stack.
pub(super) fn threshold_prior(a: f64, b: f64) -> Option<[f64; 100]> {
    if !a.is_finite() || !b.is_finite() || a <= 0.0 || b <= 0.0 || !(a + b).is_finite() {
        return None;
    }
    let log_normalizer = log_beta(a, b);
    if !log_normalizer.is_finite() {
        return None;
    }
    let mut masses = [0.0; 100];
    let mut previous = 0.0;
    for (i, mass) in masses.iter_mut().enumerate() {
        let cdf = beta_cdf((i + 1) as f64 / 100.0, a, b, log_normalizer)?;
        if !cdf.is_finite() || cdf < previous - 1e-12 || !(-1e-12..=1.0 + 1e-12).contains(&cdf) {
            return None;
        }
        let cdf = cdf.clamp(previous, 1.0);
        *mass = cdf - previous;
        previous = cdf;
    }
    Some(masses)
}

/// Scan troughs in lag order once. Only a new record-low trough can become
/// the first trough below a remaining threshold. Each of 100 threshold
/// masses is assigned once, then remaining mass is discounted at the global
/// minimum. At most 101 distinct candidates; caller preallocates storage.
pub(super) fn candidate_probabilities(
    difference: &[f64],
    normalized: &[f64],
    tau_min: usize,
    prior: &[f64; 100],
    fallback: f64,
    rate: f64,
    output: &mut Vec<PitchCandidate>,
) -> f64 {
    output.clear();
    let tau_max = normalized.len() - 2;
    let mut remaining = 100;
    let mut minimum = None;
    let mut last_written = None;
    for tau in tau_min..=tau_max {
        if normalized[tau - 1] <= normalized[tau] || normalized[tau] > normalized[tau + 1] {
            continue;
        }
        let depth = interpolated_minimum(normalized, tau);
        if minimum.is_none_or(|(_, lowest)| depth < lowest) {
            minimum = Some((tau, depth));
        }
        let mut mass = 0.0;
        while remaining > 0 && remaining as f64 / 100.0 > depth {
            mass += prior[remaining - 1];
            remaining -= 1;
        }
        if mass > 0.0 {
            let period = vertex_period(difference, tau, tau_max);
            output.push(PitchCandidate {
                period_samples: period,
                f0_hz: rate / period,
                aperiodicity: depth,
                probability: mass,
            });
            last_written = Some(tau);
        }
    }
    if let Some((tau, depth)) = minimum {
        let mass = prior[..remaining].iter().sum::<f64>() * fallback;
        if mass > 0.0 {
            if last_written == Some(tau) {
                if let Some(candidate) = output.last_mut() {
                    candidate.probability += mass;
                }
            } else {
                let period = vertex_period(difference, tau, tau_max);
                output.push(PitchCandidate {
                    period_samples: period,
                    f0_hz: rate / period,
                    aperiodicity: depth,
                    probability: mass,
                });
            }
        }
    }
    output
        .iter()
        .map(|candidate| candidate.probability)
        .sum::<f64>()
        .min(1.0)
}
