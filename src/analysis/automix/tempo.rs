//! Constant-tempo inference from an onset-strength curve. Offline only.
//!
//! The prior/DP formulation follows Ellis (2007); the grid is an unrounded
//! least-squares fit. Candidate/contrast corrections use separate real-music
//! development data; synthetic fixtures constrain phase and jitter behavior.
//! See docs/automix-accuracy.md for the evaluation policy.

use super::{check_cancel, AutomixError};
use crate::decoder::DecodeCancelToken;

const MIN_BPM: f64 = 55.0;
const MAX_BPM: f64 = 200.0;
const PRIOR_CENTER_BPM: f64 = 120.0;
const PRIOR_OCTAVES: f64 = 1.5;
const DP_TIGHTNESS: f64 = 100.0;
const MIN_SALIENCE: f64 = 0.15;
const MIN_GRID_CONFIDENCE: f64 = 0.35;
const MIN_GRID_STABILITY: f64 = 0.80;
// At the .80 usability bar this caps RMS phase error at 25 ms, including
// half-time grids whose beat-relative residual alone understates the error.
const MAX_GRID_RESIDUAL_SEC: f64 = 0.125;
const MIN_FIT_BEATS: usize = 6;
const CANCEL_CHUNK: usize = 2_048;

#[derive(Clone, Copy, Debug)]
pub(super) struct BeatGrid {
    /// The fitted period, never rounded to public BPM precision.
    pub period_sec: f64,
    /// First beat at or after the head origin. This is not a downbeat.
    pub first_beat_sec: f64,
    pub stability: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TempoEstimate {
    pub grid: Option<BeatGrid>,
    pub confidence: Option<f64>,
}

impl TempoEstimate {
    pub fn bpm(self) -> Option<f64> {
        self.grid
            .map(|grid| ((60.0 / grid.period_sec).clamp(MIN_BPM, MAX_BPM) * 100.0).round() / 100.0)
    }

    pub fn usable_grid(self) -> Option<BeatGrid> {
        self.grid.filter(|grid| {
            self.confidence.unwrap_or(0.0) >= MIN_GRID_CONFIDENCE
                && grid.stability >= MIN_GRID_STABILITY
        })
    }
}

pub(super) fn estimate(
    values: &[f32],
    rate: f64,
    observation_offset_sec: f64,
    cancel: Option<&DecodeCancelToken>,
) -> Result<TempoEstimate, AutomixError> {
    check_cancel(cancel)?;
    if !rate.is_finite()
        || rate <= 0.0
        || !observation_offset_sec.is_finite()
        || values.len() < (rate * 2.0).ceil() as usize
    {
        return Ok(TempoEstimate::default());
    }
    let Some(onsets) = whiten(values, rate, cancel)? else {
        return Ok(TempoEstimate::default());
    };
    let min_period = rate * 60.0 / MAX_BPM;
    let max_period = rate * 60.0 / MIN_BPM;
    let min_lag = (min_period.floor() as usize).max(2);
    let max_lag = (max_period.ceil() as usize).min(onsets.len() / 2);
    if min_lag > max_lag {
        return Ok(TempoEstimate::default());
    }
    let periodicity = smooth_periodicity(&onsets, rate, cancel)?;
    let correlations = autocorrelation(
        &periodicity,
        (3 * max_lag + 2).min(onsets.len() - 1),
        cancel,
    )?;
    let mean_weighted = (min_lag..=max_lag)
        .map(|lag| correlations[lag] * prior(lag as f64, rate))
        .sum::<f64>()
        / (max_lag - min_lag + 1) as f64;

    let mut best: Option<(f64, f64, f64)> = None;
    // The boundary bins participate in interpolation too; a true 200 BPM
    // period can fall between floor(min_period) and the next observation.
    for lag in min_lag..=max_lag {
        check_cancel(cancel)?;
        if correlations[lag] < correlations[lag - 1] || correlations[lag] < correlations[lag + 1] {
            continue;
        }
        let period = lag as f64 + parabola(&correlations, lag);
        if period < min_period - 0.75 || period > max_period + 0.75 {
            continue;
        }
        let period = harmonic_period(&correlations, period);
        let base = interpolated(&correlations, period);
        let harmonic = (base
            + 0.5 * interpolated(&correlations, period * 2.0)
            + 0.25 * interpolated(&correlations, period * 3.0))
            / 1.75;
        // A slower candidate must explain strong intervening onsets. Weak
        // subdivisions do not force a doubled tempo; equally strong pulses
        // prefer the shortest supported level rather than the prior's octave.
        // Fourth powers reserve that penalty for nearly equal subdivisions.
        // Scale it by the candidate's own evidence: an absolute subtraction
        // can demote a supported beat below unrelated weak periodicities.
        let subdivisions = 0.60 * interpolated(&correlations, period / 2.0).powi(4)
            + 0.30 * interpolated(&correlations, period / 3.0).powi(4);
        let score = base * harmonic * prior(period, rate) * (1.0 - subdivisions);
        let weighted_peak = base * prior(period, rate);
        let salience = ((weighted_peak - mean_weighted) / weighted_peak.max(0.01)).clamp(0.0, 1.0);
        if best.is_none_or(|(_, previous, _)| score > previous) {
            best = Some((period, score, salience));
        }
    }
    let Some((period, _, salience)) = best else {
        return Ok(TempoEstimate::default());
    };
    if salience < MIN_SALIENCE {
        return Ok(TempoEstimate {
            grid: None,
            confidence: Some(salience),
        });
    }
    let beats = track_beats(&onsets, period, cancel)?;
    let Some((slope, intercept, rms, support)) = fit_grid(&onsets, &beats, rate, cancel)? else {
        return Ok(TempoEstimate {
            grid: None,
            confidence: Some(salience),
        });
    };
    let period_sec = slope / rate;
    let fitted_bpm = 60.0 / period_sec;
    if !fitted_bpm.is_finite() || !(MIN_BPM - 0.1..=MAX_BPM + 0.1).contains(&fitted_bpm) {
        return Ok(TempoEstimate {
            grid: None,
            confidence: Some(0.0),
        });
    }
    let residual_scale = (slope / 4.0).min(MAX_GRID_RESIDUAL_SEC * rate);
    let stability = (1.0 - rms / residual_scale).clamp(0.0, 1.0);
    if stability == 0.0 {
        return Ok(TempoEstimate {
            grid: None,
            confidence: Some(0.0),
        });
    }
    let first_beat_sec = (intercept / rate + observation_offset_sec).rem_euclid(period_sec);
    Ok(TempoEstimate {
        grid: Some(BeatGrid {
            period_sec,
            first_beat_sec,
            stability,
        }),
        confidence: Some((salience * stability * support).clamp(0.0, 1.0)),
    })
}

fn whiten(
    values: &[f32],
    rate: f64,
    cancel: Option<&DecodeCancelToken>,
) -> Result<Option<Vec<f64>>, AutomixError> {
    let mut prefix = Vec::with_capacity(values.len() + 1);
    prefix.push(0.0);
    let mut sum = 0.0;
    for chunk in values.chunks(CANCEL_CHUNK) {
        check_cancel(cancel)?;
        for &value in chunk {
            if !value.is_finite() {
                return Ok(None);
            }
            sum += f64::from(value.max(0.0));
            prefix.push(sum);
        }
    }
    let radius = (rate * 0.10).round().max(1.0) as usize;
    let mut onsets = Vec::with_capacity(values.len());
    let mut energy = 0.0;
    for (index, &value) in values.iter().enumerate() {
        if index % CANCEL_CHUNK == 0 {
            check_cancel(cancel)?;
        }
        let start = index.saturating_sub(radius);
        let end = (index + radius + 1).min(values.len());
        let local_mean = (prefix[end] - prefix[start]) / (end - start) as f64;
        let onset = (f64::from(value) - local_mean).max(0.0);
        energy += onset * onset;
        onsets.push(onset);
    }
    let rms = (energy / values.len() as f64).sqrt();
    if rms <= 1e-10 {
        return Ok(None);
    }
    for chunk in onsets.chunks_mut(CANCEL_CHUNK) {
        check_cancel(cancel)?;
        for value in chunk {
            *value /= rms;
        }
    }
    Ok(Some(onsets))
}

// Narrow transients otherwise favor integer-aligned multiples over a
// fractional fundamental. Smooth only ACF evidence; track and fit raw onsets.
fn smooth_periodicity(
    onsets: &[f64],
    rate: f64,
    cancel: Option<&DecodeCancelToken>,
) -> Result<Vec<f64>, AutomixError> {
    let sigma = (rate * 0.010).max(1.0);
    let radius = (3.0 * sigma).ceil() as usize;
    let weights: Vec<_> = (0..=radius)
        .map(|offset| (-0.5 * (offset as f64 / sigma).powi(2)).exp())
        .collect();
    let normalizer = 2.0 * weights.iter().sum::<f64>() - weights[0];
    let mut smoothed = Vec::with_capacity(onsets.len());
    for index in 0..onsets.len() {
        if index % CANCEL_CHUNK == 0 {
            check_cancel(cancel)?;
        }
        let start = index.saturating_sub(radius);
        let end = (index + radius + 1).min(onsets.len());
        let sum = onsets[start..end]
            .iter()
            .enumerate()
            .map(|(offset, value)| value * weights[(start + offset).abs_diff(index)])
            .sum::<f64>();
        smoothed.push(sum / normalizer);
    }
    Ok(smoothed)
}

fn autocorrelation(
    onsets: &[f64],
    max_lag: usize,
    cancel: Option<&DecodeCancelToken>,
) -> Result<Vec<f64>, AutomixError> {
    let mut result = vec![1.0];
    for lag in 1..=max_lag {
        let (mut dot, mut left_energy, mut right_energy) = (0.0, 0.0, 0.0);
        for (left, right) in onsets[..onsets.len() - lag]
            .chunks(CANCEL_CHUNK)
            .zip(onsets[lag..].chunks(CANCEL_CHUNK))
        {
            check_cancel(cancel)?;
            for (&a, &b) in left.iter().zip(right) {
                dot += a * b;
                left_energy += a * a;
                right_energy += b * b;
            }
        }
        let denominator = (left_energy * right_energy).sqrt();
        result.push(if denominator > 0.0 {
            (dot / denominator).clamp(0.0, 1.0)
        } else {
            0.0
        });
    }
    Ok(result)
}

fn prior(period: f64, rate: f64) -> f64 {
    let octaves = ((60.0 * rate / period) / PRIOR_CENTER_BPM).log2() / PRIOR_OCTAVES;
    (-0.5 * octaves * octaves).exp()
}

fn parabola(values: &[f64], index: usize) -> f64 {
    if index == 0 || index + 1 >= values.len() {
        return 0.0;
    }
    let curvature = values[index - 1] - 2.0 * values[index] + values[index + 1];
    if curvature >= -1e-12 {
        return 0.0;
    }
    (0.5 * (values[index - 1] - values[index + 1]) / curvature).clamp(-0.5, 0.5)
}

fn interpolated(values: &[f64], position: f64) -> f64 {
    let index = position.floor() as usize;
    if index + 1 >= values.len() {
        return 0.0;
    }
    values[index] + (values[index + 1] - values[index]) * (position - index as f64)
}

fn harmonic_period(correlations: &[f64], period: f64) -> f64 {
    let (mut numerator, mut denominator) = (0.0, 0.0);
    for (multiple, weight) in [(1.0, 1.0), (2.0, 0.5), (3.0, 0.25)] {
        let center = (period * multiple).round() as usize;
        if center < 2 || center + 2 >= correlations.len() {
            continue;
        }
        let peak = (center - 1..=center + 1)
            .max_by(|&a, &b| correlations[a].total_cmp(&correlations[b]))
            .unwrap_or(center);
        if correlations[peak] < 0.1 {
            continue;
        }
        let lag = peak as f64 + parabola(correlations, peak);
        let weight = weight * correlations[peak];
        numerator += weight * multiple * lag;
        denominator += weight * multiple * multiple;
    }
    if denominator > 0.0 {
        numerator / denominator
    } else {
        period
    }
}

fn track_beats(
    onsets: &[f64],
    period: f64,
    cancel: Option<&DecodeCancelToken>,
) -> Result<Vec<usize>, AutomixError> {
    let first_lag = (period / 2.0).floor().max(1.0) as usize;
    let last_lag = (period * 2.0).ceil() as usize;
    let penalties: Vec<_> = (first_lag..=last_lag)
        .map(|lag| DP_TIGHTNESS * (lag as f64 / period).ln().powi(2))
        .collect();
    let mut cumulative = vec![0.0; onsets.len()];
    let mut predecessor = vec![None; onsets.len()];
    let mut end = 0;
    for index in 0..onsets.len() {
        if index % 256 == 0 {
            check_cancel(cancel)?;
        }
        let mut best = 0.0;
        for (offset, &penalty) in penalties
            .iter()
            .take(index.saturating_sub(first_lag) + usize::from(index >= first_lag))
            .enumerate()
        {
            let previous = index - first_lag - offset;
            let score = cumulative[previous] - penalty;
            if score > best {
                best = score;
                predecessor[index] = Some(previous);
            }
        }
        cumulative[index] = onsets[index] + best;
        if cumulative[index] > cumulative[end] {
            end = index;
        }
    }
    let mut beats = vec![end];
    while let Some(previous) = predecessor[end] {
        end = previous;
        beats.push(end);
    }
    beats.reverse();
    Ok(beats)
}

/// Fit supported observations while keeping their DP beat numbers (including
/// intervening missing beats). Virtual zero-onset beats cannot certify a grid.
fn fit_grid(
    onsets: &[f64],
    beats: &[usize],
    rate: f64,
    cancel: Option<&DecodeCancelToken>,
) -> Result<Option<(f64, f64, f64, f64)>, AutomixError> {
    let mut points = Vec::new();
    for (number, &index) in beats.iter().enumerate() {
        if number % CANCEL_CHUNK == 0 {
            check_cancel(cancel)?;
        }
        if onsets[index] >= 0.25 {
            points.push((number as f64, index as f64 + parabola(onsets, index)));
        }
    }
    if points.len() < MIN_FIT_BEATS {
        return Ok(None);
    }
    let (mut slope, mut intercept) = least_squares(&points);
    // One bounded robust refit removes isolated tracking errors. Stability is
    // still measured on ALL supported observations, never just the inliers.
    let residuals: Vec<_> = points
        .iter()
        .map(|(x, y)| (y - (intercept + slope * x)).abs())
        .collect();
    let mut ordered = residuals.clone();
    ordered.sort_by(f64::total_cmp);
    let cutoff = (3.0 * ordered[ordered.len() / 2]).max(0.035 * rate);
    let inliers: Vec<_> = points
        .iter()
        .zip(&residuals)
        .filter_map(|(&point, &residual)| (residual <= cutoff).then_some(point))
        .collect();
    if inliers.len() >= MIN_FIT_BEATS {
        (slope, intercept) = least_squares(&inliers);
    }
    let rms = (points
        .iter()
        .map(|(x, y)| (y - (intercept + slope * x)).powi(2))
        .sum::<f64>()
        / points.len() as f64)
        .sqrt();
    let support = points.len() as f64 / beats.len() as f64;
    Ok(Some((slope, intercept, rms, support)))
}

fn least_squares(points: &[(f64, f64)]) -> (f64, f64) {
    let count = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / count;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / count;
    let numerator = points
        .iter()
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>();
    let denominator = points
        .iter()
        .map(|(x, _)| (x - mean_x).powi(2))
        .sum::<f64>();
    let slope = numerator / denominator;
    (slope, mean_y - slope * mean_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_subdivisions_cannot_promote_a_weaker_unrelated_period() {
        let rate = 200.0;
        let bpm = 119.18;
        let period = 60.0 / bpm;
        // Strong sixteenth notes accompany the beat. A weaker independent
        // pulse train is a distractor, not an octave of the dominant rhythm.
        let onsets: Vec<f32> = (0..6_000)
            .map(|index| {
                let time = index as f64 / rate;
                let pulse = |period: f64, offset: f64| {
                    let phase = (time - offset + period / 2.0).rem_euclid(period) - period / 2.0;
                    (-0.5 * (phase / 0.008).powi(2)).exp()
                };
                (pulse(period, 0.217)
                    + pulse(period / 4.0, 0.217)
                    + 0.6 * pulse(period * 1.25, 0.297)) as f32
            })
            .collect();
        let estimate = estimate(&onsets, rate, 0.0, None).unwrap();
        let detected = estimate
            .bpm()
            .expect("dominant periodic onsets support a grid");
        assert!(
            (detected - bpm).abs() <= 0.05,
            "expected {bpm}, got {detected}"
        );
        let grid = estimate.grid.unwrap();
        let phase_error =
            (grid.first_beat_sec - 0.217 + period / 2.0).rem_euclid(period) - period / 2.0;
        assert!(phase_error.abs() <= 0.010);
        let final_error = phase_error + (30.0 / period).floor() * (grid.period_sec - period);
        assert!(final_error.abs() <= 0.020);
    }

    #[test]
    fn slow_grid_cannot_hide_absolute_onset_jitter() {
        let rate = 200.0;
        for bpm in [60.0, 70.0, 90.0, 127.3] {
            let period = 60.0 / bpm;
            let mut onsets = vec![0.0; 12_000];
            for beat in 0..(60.0 / period) as usize {
                let time = 0.217 + beat as f64 * period + 0.065 * (beat as f64 * 1.713).sin();
                let center = time * rate;
                let start = (center - 8.0).max(0.0) as usize;
                let end = ((center + 9.0) as usize).min(onsets.len());
                for (offset, onset) in onsets[start..end].iter_mut().enumerate() {
                    *onset += (-0.5 * ((start as f64 + offset as f64 - center) / 1.6).powi(2)).exp()
                        as f32;
                }
            }
            let result = estimate(&onsets, rate, 0.0, None).unwrap();
            assert!(result.usable_grid().is_none(), "{bpm}: {result:?}");
            assert!(
                result.grid.is_none_or(|grid| grid.stability < 0.80),
                "{bpm}: {result:?}"
            );
        }
    }

    #[test]
    fn large_tempo_search_observes_cancellation() {
        let onsets = vec![1.0; 1_000_000];
        let token = DecodeCancelToken::new();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(5));
                token.cancel();
            });
            assert!(matches!(
                autocorrelation(&onsets, 1_000, Some(&token)),
                Err(AutomixError::Canceled)
            ));
        });
    }
}
