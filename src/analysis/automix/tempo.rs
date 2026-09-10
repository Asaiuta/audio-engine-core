//! Constant-tempo inference from an onset-strength curve. Offline only.
//!
//! The prior/DP formulation follows Ellis (2007); the grid is an unrounded
//! least-squares fit. Candidate/contrast corrections use separate real-music
//! development data; synthetic fixtures constrain phase and jitter behavior.
//! When spectral channel views are available, a channel-mean ACF can replace
//! the all-band candidate only under a conservative signal-derived gate.
//! See docs/automix-accuracy.md for the evaluation policy.

use super::{check_cancel, AutomixError};
use crate::decoder::DecodeCancelToken;
use realfft::{num_complex::Complex, ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

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
const CHANNEL_PERIOD_TOLERANCE: f64 = 0.01;
const CHANNEL_MARGIN_ADVANTAGE: f64 = 0.05;

#[derive(Clone, Copy)]
struct TempoCandidate {
    period: f64,
    score: f64,
    salience: f64,
}

#[derive(Clone, Copy)]
struct RankedEstimate {
    candidate: TempoCandidate,
    estimate: TempoEstimate,
    margin: f64,
}

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
    estimate_with_channels(values, None, rate, observation_offset_sec, cancel)
}

pub(super) fn estimate_with_channels(
    values: &[f32],
    channels: Option<&[Vec<f32>; 3]>,
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
    let correlation_lags = (3 * max_lag + 2).min(onsets.len() - 1);
    let mut correlation = Autocorrelation::new(onsets.len(), correlation_lags);
    let periodicity = smooth_periodicity(&onsets, rate, cancel)?;
    let correlations = correlation.compute(&periodicity, correlation_lags, cancel)?;
    let candidates = rank_candidates(&correlations, rate, min_lag, max_lag, cancel)?;
    let Some(all_band) =
        fit_ranked_candidates(&onsets, &candidates, rate, observation_offset_sec, cancel)?
    else {
        return Ok(TempoEstimate::default());
    };
    // Channel fusion cannot replace an invalid all-band grid, or improve a
    // margin already within the required advantage of its upper bound (1).
    if all_band.estimate.grid.is_none() || all_band.margin + CHANNEL_MARGIN_ADVANTAGE > 1.0 {
        return Ok(all_band.estimate);
    }

    let Some(channels) =
        channels.filter(|channels| channels.iter().all(|values| values.len() == onsets.len()))
    else {
        return Ok(all_band.estimate);
    };
    let Some(channel_correlations) =
        channel_mean_correlations(channels, rate, correlation_lags, &mut correlation, cancel)?
    else {
        return Ok(all_band.estimate);
    };
    let channel_candidates =
        rank_candidates(&channel_correlations, rate, min_lag, max_lag, cancel)?;
    // The margin is independent of which of the first two fits succeeds.
    // Check it before spending time on a DP fit that cannot be selected.
    if candidate_margin(&channel_candidates) < all_band.margin + CHANNEL_MARGIN_ADVANTAGE {
        return Ok(all_band.estimate);
    }
    let Some(channel_mean) = fit_ranked_candidates(
        &onsets,
        &channel_candidates,
        rate,
        observation_offset_sec,
        cancel,
    )?
    else {
        return Ok(all_band.estimate);
    };
    if should_use_channel_mean(all_band, channel_mean) {
        Ok(channel_mean.estimate)
    } else {
        Ok(all_band.estimate)
    }
}

fn rank_candidates(
    correlations: &[f64],
    rate: f64,
    min_lag: usize,
    max_lag: usize,
    cancel: Option<&DecodeCancelToken>,
) -> Result<Vec<TempoCandidate>, AutomixError> {
    let min_period = rate * 60.0 / MAX_BPM;
    let max_period = rate * 60.0 / MIN_BPM;
    if min_lag > max_lag {
        return Ok(Vec::new());
    }
    let mean_weighted = (min_lag..=max_lag)
        .map(|lag| correlations[lag] * prior(lag as f64, rate))
        .sum::<f64>()
        / (max_lag - min_lag + 1) as f64;

    let mut best: Option<TempoCandidate> = None;
    let mut second: Option<TempoCandidate> = None;
    // The boundary bins participate in interpolation too; a true 200 BPM
    // period can fall between floor(min_period) and the next observation.
    for lag in min_lag..=max_lag {
        check_cancel(cancel)?;
        if correlations[lag] < correlations[lag - 1] || correlations[lag] < correlations[lag + 1] {
            continue;
        }
        let period = lag as f64 + parabola(correlations, lag);
        if period < min_period - 0.75 || period > max_period + 0.75 {
            continue;
        }
        let period = harmonic_period(correlations, period);
        let base = interpolated(correlations, period);
        let harmonic = (base
            + 0.5 * interpolated(correlations, period * 2.0)
            + 0.25 * interpolated(correlations, period * 3.0))
            / 1.75;
        // A slower candidate must explain strong intervening onsets. Weak
        // subdivisions do not force a doubled tempo; equally strong pulses
        // prefer the shortest supported level rather than the prior's octave.
        // Sixth powers reserve that penalty for nearly equal subdivisions.
        // Scale it by the candidate's own evidence: an absolute subtraction
        // can demote a supported beat below unrelated weak periodicities.
        let subdivisions = 0.60 * interpolated(correlations, period / 2.0).powi(6)
            + 0.30 * interpolated(correlations, period / 3.0).powi(6);
        let score = base * harmonic * prior(period, rate) * (1.0 - subdivisions);
        let weighted_peak = base * prior(period, rate);
        let salience = ((weighted_peak - mean_weighted) / weighted_peak.max(0.01)).clamp(0.0, 1.0);
        let candidate = TempoCandidate {
            period,
            score,
            salience,
        };
        if best.is_none_or(|previous| score > previous.score) {
            second = best;
            best = Some(candidate);
        } else if second.is_none_or(|previous| score > previous.score) {
            second = Some(candidate);
        }
    }
    Ok([best, second].into_iter().flatten().collect())
}

fn fit_ranked_candidates(
    onsets: &[f64],
    candidates: &[TempoCandidate],
    rate: f64,
    observation_offset_sec: f64,
    cancel: Option<&DecodeCancelToken>,
) -> Result<Option<RankedEstimate>, AutomixError> {
    let margin = candidate_margin(candidates);
    let mut first_rejection = None;
    for &candidate in candidates.iter().take(2) {
        let estimate = fit_candidate(onsets, candidate, rate, observation_offset_sec, cancel)?;
        if estimate.grid.is_some() {
            return Ok(Some(RankedEstimate {
                candidate,
                estimate,
                margin,
            }));
        }
        if first_rejection.is_none() {
            first_rejection = Some(RankedEstimate {
                candidate,
                estimate,
                margin,
            });
        }
    }
    Ok(first_rejection)
}

fn channel_mean_correlations(
    channels: &[Vec<f32>; 3],
    rate: f64,
    max_lag: usize,
    correlation: &mut Autocorrelation,
    cancel: Option<&DecodeCancelToken>,
) -> Result<Option<Vec<f64>>, AutomixError> {
    let mut mean = vec![0.0; max_lag + 1];
    let mut count = 0usize;
    for values in channels {
        check_cancel(cancel)?;
        let Some(onsets) = whiten(values, rate, cancel)? else {
            continue;
        };
        let periodicity = smooth_periodicity(&onsets, rate, cancel)?;
        let correlations = correlation.compute(&periodicity, max_lag, cancel)?;
        for (sum, value) in mean.iter_mut().zip(correlations) {
            *sum += value;
        }
        count += 1;
    }
    if count == 0 {
        return Ok(None);
    }
    for value in &mut mean {
        *value /= count as f64;
    }
    Ok(Some(mean))
}

fn candidate_margin(candidates: &[TempoCandidate]) -> f64 {
    match candidates {
        [best, second, ..] if best.score > 0.0 => {
            ((best.score - second.score) / best.score).max(0.0)
        }
        _ => 0.0,
    }
}

fn should_use_channel_mean(all_band: RankedEstimate, channel_mean: RankedEstimate) -> bool {
    if all_band.estimate.grid.is_none() || channel_mean.estimate.grid.is_none() {
        return false;
    }
    channel_mean.margin >= all_band.margin + CHANNEL_MARGIN_ADVANTAGE
        && compatible_periods(
            all_band.candidate.period,
            channel_mean.candidate.period,
            CHANNEL_PERIOD_TOLERANCE,
        )
}

fn compatible_periods(left: f64, right: f64, tolerance: f64) -> bool {
    [0.5, 2.0 / 3.0, 1.0, 1.5, 2.0, 3.0]
        .into_iter()
        .any(|ratio| (left - right * ratio).abs() <= tolerance * right * ratio)
}

fn fit_candidate(
    onsets: &[f64],
    candidate: TempoCandidate,
    rate: f64,
    observation_offset_sec: f64,
    cancel: Option<&DecodeCancelToken>,
) -> Result<TempoEstimate, AutomixError> {
    check_cancel(cancel)?;
    let TempoCandidate {
        period, salience, ..
    } = candidate;
    if salience < MIN_SALIENCE {
        return Ok(TempoEstimate {
            grid: None,
            confidence: Some(salience),
        });
    }
    let beats = track_beats(onsets, period, cancel)?;
    let Some((slope, intercept, rms, support)) = fit_grid(onsets, &beats, rate, cancel)? else {
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

/// One plan/buffer owner for all onset views in a single offline estimate.
struct Autocorrelation {
    forward: Arc<dyn RealToComplex<f64>>,
    inverse: Arc<dyn ComplexToReal<f64>>,
    buffer: Vec<f64>,
    spectrum: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    prefix_energy: Vec<f64>,
    suffix_energy: Vec<f64>,
}

impl Autocorrelation {
    fn new(length: usize, max_lag: usize) -> Self {
        // Padding by max_lag is sufficient: wraparound cannot contribute to
        // any requested lag. There is no need to compute all 2*N-1 lags.
        let size = (length + max_lag).next_power_of_two();
        let mut planner = RealFftPlanner::new();
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        let scratch_len = forward.get_scratch_len().max(inverse.get_scratch_len());
        Self {
            buffer: forward.make_input_vec(),
            spectrum: forward.make_output_vec(),
            scratch: vec![Complex::default(); scratch_len],
            prefix_energy: vec![0.0; length + 1],
            suffix_energy: vec![0.0; length + 1],
            forward,
            inverse,
        }
    }

    fn compute(
        &mut self,
        onsets: &[f64],
        max_lag: usize,
        cancel: Option<&DecodeCancelToken>,
    ) -> Result<Vec<f64>, AutomixError> {
        check_cancel(cancel)?;
        self.buffer.fill(0.0);
        self.buffer[..onsets.len()].copy_from_slice(onsets);
        for (index, &value) in onsets.iter().enumerate() {
            if index % CANCEL_CHUNK == 0 {
                check_cancel(cancel)?;
            }
            self.prefix_energy[index + 1] = self.prefix_energy[index] + value * value;
        }
        // A suffix sum avoids subtracting nearly equal totals for a quiet
        // tail following a large transient.
        for index in (0..onsets.len()).rev() {
            if index % CANCEL_CHUNK == 0 {
                check_cancel(cancel)?;
            }
            self.suffix_energy[index] =
                self.suffix_energy[index + 1] + onsets[index] * onsets[index];
        }
        if self
            .forward
            .process_with_scratch(&mut self.buffer, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            return direct_autocorrelation(onsets, max_lag, cancel);
        }
        check_cancel(cancel)?;
        for chunk in self.spectrum.chunks_mut(CANCEL_CHUNK) {
            check_cancel(cancel)?;
            for bin in chunk {
                *bin = Complex::new(bin.norm_sqr(), 0.0);
            }
        }
        if self
            .inverse
            .process_with_scratch(&mut self.spectrum, &mut self.buffer, &mut self.scratch)
            .is_err()
        {
            return direct_autocorrelation(onsets, max_lag, cancel);
        }
        check_cancel(cancel)?;
        let normalization = self.buffer.len() as f64;
        let total_energy = self.prefix_energy[onsets.len()];
        let mut result = Vec::with_capacity(max_lag + 1);
        result.push(1.0);
        for lag in 1..=max_lag {
            check_cancel(cancel)?;
            let denominator =
                (self.prefix_energy[onsets.len() - lag] * self.suffix_energy[lag]).sqrt();
            // FFT absolute roundoff can overwhelm very quiet overlap pairs.
            // Preserve the direct calculation for ill-conditioned lags; this
            // cutoff depends on numeric conditioning, never music labels.
            let value = if denominator < 1e-4 * total_energy {
                direct_correlation(onsets, lag, cancel)?
            } else if denominator > 0.0 {
                (self.buffer[lag] / normalization / denominator).clamp(0.0, 1.0)
            } else {
                0.0
            };
            result.push(value);
        }
        Ok(result)
    }
}

fn direct_autocorrelation(
    onsets: &[f64],
    max_lag: usize,
    cancel: Option<&DecodeCancelToken>,
) -> Result<Vec<f64>, AutomixError> {
    let mut result = vec![1.0];
    for lag in 1..=max_lag {
        result.push(direct_correlation(onsets, lag, cancel)?);
    }
    Ok(result)
}

fn direct_correlation(
    onsets: &[f64],
    lag: usize,
    cancel: Option<&DecodeCancelToken>,
) -> Result<f64, AutomixError> {
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
    Ok(if denominator > 0.0 {
        (dot / denominator).clamp(0.0, 1.0)
    } else {
        0.0
    })
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
    fn fft_correlations_match_direct_overlap_normalization_and_reuse() {
        for length in [73, 401, 6_013] {
            let max_lag = 659.min(length - 1);
            let mut workspace = Autocorrelation::new(length, max_lag);
            for shape in 0..4 {
                let values: Vec<_> = (0..length)
                    .map(|i| match shape {
                        0 => 0.0,
                        1 => 1.0 + 0.7 * (i as f64 * 0.127).sin(),
                        2 => {
                            if i == 0 {
                                1e6
                            } else {
                                1e-4 * (1.0 + (i as f64).sin())
                            }
                        }
                        _ => {
                            let phase = (i as f64 % 91.37) - 45.0;
                            (-0.5 * (phase / 2.3).powi(2)).exp()
                                + 0.13 * (i as f64 * 1.713).sin().max(0.0)
                        }
                    })
                    .collect();
                let expected = direct_autocorrelation(&values, max_lag, None).unwrap();
                let actual = workspace.compute(&values, max_lag, None).unwrap();
                for (lag, (&expected, &actual)) in expected.iter().zip(&actual).enumerate() {
                    assert!(
                        (actual - expected).abs() < 3e-10,
                        "length={length}, shape={shape}, lag={lag}: {actual} != {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn short_accented_pulses_use_the_level_with_enough_observed_beats() {
        let rate = 200.0;
        let period = 1.0 / 3.0;
        let mut onsets = vec![0.0; 600];
        for beat in 0..9 {
            let center = (0.217 + beat as f64 * period) * rate;
            let amplitude = if beat % 2 == 0 { 1.0 } else { 0.5 };
            for (index, onset) in onsets.iter_mut().enumerate() {
                *onset +=
                    (amplitude * (-0.5 * ((index as f64 - center) / 1.6).powi(2)).exp()) as f32;
            }
        }
        let result = estimate(&onsets, rate, 0.0, None).unwrap();
        let bpm = result.bpm().expect("nine pulses can support a fitted grid");
        assert!((bpm - 180.0).abs() <= 0.05, "{result:?}");
        let grid = result.grid.unwrap();
        assert!((grid.first_beat_sec - 0.217).abs() <= 0.010);
    }

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
    fn channel_fusion_keeps_identical_views_equivalent() {
        let rate = 200.0;
        let period = 60.0 / 120.0;
        let values: Vec<f32> = (0..6_000)
            .map(|index| {
                let time = index as f64 / rate;
                let phase = (time - 0.137 + period / 2.0).rem_euclid(period) - period / 2.0;
                (-0.5 * (phase / 0.008).powi(2)).exp() as f32
            })
            .collect();
        let channels = [values.clone(), values.clone(), values];
        let control = estimate(&channels[0], rate, 0.0, None).unwrap();
        let fused = estimate_with_channels(&channels[0], Some(&channels), rate, 0.0, None).unwrap();
        assert_eq!(fused.bpm(), control.bpm());
        assert_eq!(fused.confidence, control.confidence);
        assert_eq!(
            fused.grid.map(|grid| grid.period_sec),
            control.grid.map(|grid| grid.period_sec)
        );
    }

    #[test]
    fn channel_fusion_requires_margin_advantage_and_period_agreement() {
        let estimate = TempoEstimate {
            grid: Some(BeatGrid {
                period_sec: 100.0,
                first_beat_sec: 0.0,
                stability: 0.9,
            }),
            confidence: Some(0.9),
        };
        let all_band = RankedEstimate {
            candidate: TempoCandidate {
                period: 100.0,
                score: 0.5,
                salience: 0.8,
            },
            estimate,
            margin: 0.10,
        };
        let compatible = RankedEstimate {
            candidate: TempoCandidate {
                period: 200.5,
                score: 0.5,
                salience: 0.8,
            },
            estimate,
            margin: 0.16,
        };
        assert!(should_use_channel_mean(all_band, compatible));

        let weak_margin = RankedEstimate {
            margin: 0.14,
            ..compatible
        };
        assert!(!should_use_channel_mean(all_band, weak_margin));

        let incompatible = RankedEstimate {
            candidate: TempoCandidate {
                period: 130.0,
                ..compatible.candidate
            },
            ..compatible
        };
        assert!(!should_use_channel_mean(all_band, incompatible));
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
                Autocorrelation::new(onsets.len(), 1_000).compute(&onsets, 1_000, Some(&token)),
                Err(AutomixError::Canceled)
            ));
        });
    }
}
