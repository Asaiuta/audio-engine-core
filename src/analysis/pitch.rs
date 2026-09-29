//! Frame-local YIN fundamental-frequency (F0) measurement and pYIN stage 1.
//!
//! [`PitchAnalyzer`] implements steps 1-5 of YIN (de Cheveigne and Kawahara,
//! 2002) on an end-anchored, rectangular integration window. It is a
//! measurement of periodicity: acceptance evidence is analytic synthetic
//! signals only, and no accuracy is claimed on real recordings. It is not
//! numerically equivalent to librosa's `yin`/`pyin`, which use a different
//! difference function and interpolate on the normalised curve.

use crate::processor::traits::{validate_sample_rate_hz, ProcessError};
use realfft::{num_complex::Complex, ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

mod probability;
use probability::{candidate_probabilities, threshold_prior};

/// Configuration for a streaming YIN pitch analyzer.
///
/// With `fs` the sample rate, the geometry is derived rather than exposed:
/// `tau_max = ceil(fs / fmin_hz)`, integration window `W = tau_max`, buffer
/// `N = W + tau_max + 1` and `tau_min = max(2, floor(fs / fmax_hz))`. At the
/// defaults and 44.1 kHz that is `W = 802`, `N = 1605` (36.4 ms) and
/// `tau_min = 25`.
#[derive(Debug, Clone)]
pub struct PitchConfig {
    /// Lowest measurable F0 in hertz; finite and positive. Sets `tau_max`, so
    /// the window duration is the same at every sample rate.
    pub fmin_hz: f64,
    /// Highest measurable F0 in hertz; finite, above `fmin_hz` and below
    /// `fs/2`, with `tau_min <= tau_max - 2`. YIN's parabolic interpolation is
    /// accurate only up to about `fs/4`; that is recommended, not enforced.
    pub fmax_hz: f64,
    /// Input samples between published frames, in `1..=N`. A hop in samples is
    /// a different duration at each sample rate (256 is 5.8 ms at 44.1 kHz).
    pub hop_size: usize,
    /// YIN absolute threshold on the normalised difference, in `(0, 1]`: the
    /// first dip below it is taken, and a frame is voiced when its
    /// aperiodicity is below it.
    pub threshold: f64,
    /// Alpha of the Beta threshold prior used by pYIN stage 1; finite and
    /// positive. Construction rejects a prior that cannot be evaluated in
    /// finite precision (before allocating any streaming state).
    pub prior_alpha: f64,
    /// Beta of the Beta threshold prior used by pYIN stage 1; finite and
    /// positive.
    pub prior_beta: f64,
    /// pYIN stage-1 weight of the absolute-minimum fallback, in `[0, 1]`.
    pub absolute_min_weight: f64,
}

impl Default for PitchConfig {
    fn default() -> Self {
        Self {
            fmin_hz: 55.0,
            fmax_hz: 1760.0,
            hop_size: 256,
            threshold: 0.1,
            prior_alpha: 2.0,
            prior_beta: 18.0,
            absolute_min_weight: 0.01,
        }
    }
}

/// Latest YIN frame.
///
/// For the buffer `b[0..N)` (`b[N-1]` newest) and lag `tau`,
/// `d(tau) = sum_{j<W} (b[N-W+j] - b[N-W+j-tau])^2` and the cumulative mean
/// normalised difference is `d'(0) = 1`,
/// `d'(tau) = d(tau) * tau / sum_{j=1..=tau} d(j)`. The chosen dip is the
/// smallest lag in `[tau_min, tau_max]` that is a local minimum of `d'` with a
/// parabolically interpolated value below `threshold`, else the dip with the
/// lowest interpolated value. Its period is the parabola vertex on `d`.
///
/// The estimate integrates over the lags' windows, so it refers to about
/// `end_sample - (W + period_samples) / 2`, 9-18 ms before the frame end at
/// the default range. Every pitch quantity is `None` (and `voiced` false) only when
/// it cannot be defined: digital silence or a constant buffer, a non-finite
/// sample in the buffer, or no dip in the lag range. A frame that fails the
/// threshold keeps its best guess and is merely not voiced. No energy gate is
/// applied: a very quiet periodic signal is as voiced as a loud one.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct PitchFrame {
    /// Exclusive end of the analysed buffer in input samples since reset.
    pub end_sample: u64,
    /// Estimated period in samples (fractional, from the parabola vertex).
    pub period_samples: Option<f64>,
    /// Estimated fundamental frequency `fs / period_samples` in hertz.
    pub f0_hz: Option<f64>,
    /// Interpolated `d'` at the chosen dip: dimensionless, 0 for a perfectly
    /// periodic window, about `1 / (1 + SNR)` for a tone in white noise. It is
    /// a measurement, not a calibrated probability or confidence.
    pub aperiodicity: Option<f64>,
    /// `aperiodicity < threshold`; false whenever the estimate is undefined.
    pub voiced: bool,
    /// Sum of the candidate probabilities under the configured threshold
    /// prior. This is not a calibrated probability on real recordings.
    /// Undefined when there is no pitch estimate.
    pub voicing_probability: Option<f64>,
    /// Distinct positive-mass candidates in increasing integer-lag order.
    /// At most 101; storage is allocated at construction and reused in place.
    /// Periods use YIN's raw-difference interpolation, not librosa's rule.
    pub candidates: Vec<PitchCandidate>,
    /// Unweighted RMS of the newest integration window (W input samples),
    /// before DC removal, in input amplitude units. Silence is `Some(0.0)`;
    /// a buffer containing any non-finite sample gives `None`. It does not
    /// gate the pitch or voicing decision.
    pub rms: Option<f64>,
}

/// A pYIN stage-1 candidate under the configured, uncalibrated threshold prior.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct PitchCandidate {
    /// Raw-difference parabola vertex in samples.
    pub period_samples: f64,
    /// Sample rate divided by `period_samples`, in hertz.
    pub f0_hz: f64,
    /// Interpolated normalized-difference ordinate, dimensionless.
    pub aperiodicity: f64,
    /// Mass assigned by the first-dip rule, including any minimum fallback.
    /// This is not calibrated confidence.
    pub probability: f64,
}

/// Streaming causal YIN analyzer (steps 1-5) and pYIN threshold candidates.
///
/// The first frame is published once `N` samples have arrived
/// ([`buffer_samples`](Self::buffer_samples)), then one frame every
/// `hop_size` samples; only the latest frame is kept. Each frame peak-scales
/// the buffer and removes its mean (conditioning only; `d` is unchanged in
/// exact arithmetic), computes `d` from an FFT cross-correlation plus prefix
/// sums of squares (clamped at zero), then applies the pure YIN decision.
/// `period_samples` and `aperiodicity` do not depend on the sample rate;
/// `f0_hz` scales with it. Construction allocates everything; `push`, the
/// accessors and `reset` allocate nothing. Run it on an analysis worker, not
/// under an audio-callback deadline.
///
/// pYIN stage 1 uses 100 thresholds `i/100`, CDF-difference Beta prior masses,
/// and the paper's first-dip/fallback rule. There is no HMM/Viterbi tracker.
/// Acceptance covers synthetic analytic signals only, not real recordings.
///
/// ```
/// use audio_engine_core::analysis::{PitchAnalyzer, PitchConfig};
/// let mut pitch = PitchAnalyzer::new(&PitchConfig::default(), 44_100)?;
/// let samples = vec![0.0; pitch.buffer_samples()];
/// assert_eq!(pitch.push(&samples), 1);
/// assert_eq!(pitch.frame().unwrap().f0_hz, None);
/// assert_eq!(pitch.frame().unwrap().rms, Some(0.0));
/// # Ok::<(), audio_engine_core::processor::traits::ProcessError>(())
/// ```
pub struct PitchAnalyzer {
    config: PitchConfig,
    sample_rate_hz: u32,
    tau_min: usize,
    tau_max: usize,
    window: usize,
    buffer_len: usize,
    ring: Vec<f64>,
    ring_pos: usize,
    filled: usize,
    since_hop: usize,
    samples_seen: u64,
    conditioned: Vec<f64>,
    prefix: Vec<f64>,
    forward: Arc<dyn RealToComplex<f64>>,
    inverse: Arc<dyn ComplexToReal<f64>>,
    block_input: Vec<f64>,
    buffer_input: Vec<f64>,
    block_spectrum: Vec<Complex<f64>>,
    buffer_spectrum: Vec<Complex<f64>>,
    forward_scratch: Vec<Complex<f64>>,
    inverse_scratch: Vec<Complex<f64>>,
    correlation: Vec<f64>,
    difference: Vec<f64>,
    normalized: Vec<f64>,
    prior: [f64; 100],
    frame: PitchFrame,
    published: bool,
}

/// Lag geometry derived from a validated configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    tau_min: usize,
    tau_max: usize,
    buffer_len: usize,
    fft_len: usize,
}

fn invalid_parameter(parameter: &'static str, message: &'static str) -> ProcessError {
    ProcessError::InvalidParameter {
        processor: "PitchAnalyzer",
        parameter,
        message,
    }
}

fn invalid_geometry(message: &'static str) -> ProcessError {
    ProcessError::InvalidGeometry {
        processor: "PitchAnalyzer",
        operation: "new",
        message,
    }
}

fn validate_config(config: &PitchConfig, sample_rate_hz: u32) -> Result<Geometry, ProcessError> {
    validate_sample_rate_hz("PitchAnalyzer", sample_rate_hz)?;
    let rate = f64::from(sample_rate_hz);
    if !config.fmin_hz.is_finite() || config.fmin_hz <= 0.0 {
        return Err(invalid_parameter("fmin_hz", "must be finite and positive"));
    }
    if !config.fmax_hz.is_finite()
        || config.fmax_hz <= config.fmin_hz
        || config.fmax_hz >= rate / 2.0
    {
        return Err(invalid_parameter(
            "fmax_hz",
            "must be finite, above fmin_hz and below half the sample rate",
        ));
    }
    // Finite positive operands: the ratio is positive or +inf, never NaN.
    // Integers up to 2^52 convert exactly; larger lags could not be buffered.
    let tau_max = (rate / config.fmin_hz).ceil();
    if tau_max > (1_u64 << 52) as f64 {
        return Err(invalid_geometry(
            "tau_max = ceil(fs/fmin) is not representable",
        ));
    }
    let tau_max = tau_max as usize;
    let buffer_len = tau_max
        .checked_mul(2)
        .and_then(|value| value.checked_add(1));
    let fft_len = buffer_len.and_then(usize::checked_next_power_of_two);
    let (Some(buffer_len), Some(fft_len)) = (buffer_len, fft_len) else {
        return Err(invalid_geometry(
            "buffer or FFT length is not representable",
        ));
    };
    if fft_len
        .checked_mul(std::mem::size_of::<Complex<f64>>())
        .is_none_or(|bytes| bytes > isize::MAX as usize)
    {
        return Err(invalid_geometry("FFT storage is not representable"));
    }
    let tau_min = ((rate / config.fmax_hz).floor() as usize).max(2);
    if tau_min.saturating_add(2) > tau_max {
        return Err(invalid_parameter(
            "fmax_hz",
            "must leave tau_min = floor(fs/fmax) at most ceil(fs/fmin) - 2",
        ));
    }
    if config.hop_size == 0 || config.hop_size > buffer_len {
        return Err(invalid_geometry("hop must be in 1..=buffer length"));
    }
    if !config.threshold.is_finite() || config.threshold <= 0.0 || config.threshold > 1.0 {
        return Err(invalid_parameter(
            "threshold",
            "must be finite and in (0, 1]",
        ));
    }
    if !config.prior_alpha.is_finite() || config.prior_alpha <= 0.0 {
        return Err(invalid_parameter(
            "prior_alpha",
            "must be finite and positive",
        ));
    }
    if !config.prior_beta.is_finite() || config.prior_beta <= 0.0 {
        return Err(invalid_parameter(
            "prior_beta",
            "must be finite and positive",
        ));
    }
    if !(0.0..=1.0).contains(&config.absolute_min_weight) {
        return Err(invalid_parameter(
            "absolute_min_weight",
            "must be finite and in [0, 1]",
        ));
    }
    Ok(Geometry {
        tau_min,
        tau_max,
        buffer_len,
        fft_len,
    })
}

/// YIN decision for one difference function.
#[derive(Debug, Clone, Copy, PartialEq)]
struct YinEstimate {
    /// Parabola vertex on `d` around the chosen dip, in samples.
    period: f64,
    /// Interpolated `d'` at the chosen dip.
    aperiodicity: f64,
}

/// Write the cumulative mean normalised difference of `difference` into
/// `normalized` (same length) and return whether any `d(tau)` is positive.
///
/// `d'(0) = 1`. While no difference has accumulated yet (a leading run of
/// exact zeros), `d'` is also 1, the value YIN gives lag 0, so the output never
/// contains `0/0`. When every `d` is zero (silence, or a constant buffer after
/// conditioning) the caller must treat the frame as undefined.
fn cumulative_mean_normalized(difference: &[f64], normalized: &mut [f64]) -> bool {
    let mut sum = 0.0;
    for (tau, (&value, output)) in difference.iter().zip(normalized.iter_mut()).enumerate() {
        if tau == 0 {
            *output = 1.0;
            continue;
        }
        sum += value;
        *output = if sum > 0.0 {
            value * tau as f64 / sum
        } else {
            1.0
        };
    }
    sum > 0.0
}

/// Ordinate of the vertex of the parabola through `values[tau - 1..=tau + 1]`,
/// or `values[tau]` when the curvature is not positive; clamped to `+0.0`.
fn interpolated_minimum(values: &[f64], tau: usize) -> f64 {
    let (left, centre, right) = (values[tau - 1], values[tau], values[tau + 1]);
    let curvature = left - 2.0 * centre + right;
    let vertex = if curvature > 0.0 {
        centre - (left - right) * (left - right) / (8.0 * curvature)
    } else {
        centre
    };
    if vertex > 0.0 {
        vertex
    } else {
        0.0
    }
}

/// Fractional period for the dip chosen at `tau`: take the smallest raw `d`
/// among `tau - 1`, `tau` and `tau + 1` (moving right only up to `tau_max`,
/// since `d(tau_max + 2)` is not computed), then the vertex of the parabola
/// through `d` there. The integer lag is kept when that lag is not a local
/// minimum of `d` or the curvature is not positive, so the vertex offset is
/// always within half a sample.
fn vertex_period(difference: &[f64], tau: usize, tau_max: usize) -> f64 {
    let (left, centre, right) = (difference[tau - 1], difference[tau], difference[tau + 1]);
    let lag = if left < centre && left <= right {
        tau - 1
    } else if right < centre && tau < tau_max {
        tau + 1
    } else {
        tau
    };
    let (left, centre, right) = (difference[lag - 1], difference[lag], difference[lag + 1]);
    let curvature = left - 2.0 * centre + right;
    if centre <= left && centre <= right && curvature > 0.0 {
        lag as f64 + (left - right) / (2.0 * curvature)
    } else {
        lag as f64
    }
}

/// YIN steps 3-5 on `difference = d(0..=tau_max + 1)`, writing `d'` into
/// `normalized` (same length). Dips are lags `tau` in `[tau_min, tau_max]`
/// with `d'(tau - 1) > d'(tau) <= d'(tau + 1)` (the first sample of a
/// plateau). The smallest dip whose interpolated ordinate is below
/// `threshold` wins; otherwise the dip with the lowest ordinate (first on
/// ties). `None` when every `d` is zero or there is no dip.
fn yin_kernel(
    difference: &[f64],
    normalized: &mut [f64],
    tau_min: usize,
    tau_max: usize,
    threshold: f64,
) -> Option<YinEstimate> {
    debug_assert!(2 <= tau_min && tau_min < tau_max);
    debug_assert!(difference.len() == tau_max + 2 && normalized.len() == tau_max + 2);
    if !cumulative_mean_normalized(difference, normalized) {
        return None;
    }
    let mut chosen: Option<(usize, f64)> = None;
    for tau in tau_min..=tau_max {
        if normalized[tau - 1] > normalized[tau] && normalized[tau] <= normalized[tau + 1] {
            let ordinate = interpolated_minimum(normalized, tau);
            if ordinate < threshold {
                chosen = Some((tau, ordinate));
                break;
            }
            if chosen.is_none_or(|(_, lowest)| ordinate < lowest) {
                chosen = Some((tau, ordinate));
            }
        }
    }
    let (tau, aperiodicity) = chosen?;
    Some(YinEstimate {
        period: vertex_period(difference, tau, tau_max),
        aperiodicity,
    })
}

/// Direct form of `d(tau)` for `tau = 0..difference.len()` over the newest
/// `window` samples of `buffer`: the test oracle for the FFT form. Neumaier
/// compensated summation keeps each lag within about an ulp of its exact sum,
/// so lags whose exact sums agree (`T - 1` and `T + 1` of a tiled period) agree
/// to rounding.
#[cfg(test)]
fn direct_difference(buffer: &[f64], window: usize, difference: &mut [f64]) {
    let start = buffer.len() - window;
    for (tau, output) in difference.iter_mut().enumerate() {
        let mut sum = 0.0_f64;
        let mut compensation = 0.0_f64;
        for index in start..buffer.len() {
            let delta = buffer[index] - buffer[index - tau];
            let term = delta * delta;
            let next = sum + term;
            compensation += if sum.abs() >= term.abs() {
                (sum - next) + term
            } else {
                (term - next) + sum
            };
            sum = next;
        }
        *output = sum + compensation;
    }
}

impl PitchAnalyzer {
    /// Validate the configuration and allocate the complete streaming state.
    pub fn new(config: &PitchConfig, sample_rate_hz: u32) -> Result<Self, ProcessError> {
        let geometry = validate_config(config, sample_rate_hz)?;
        let prior = threshold_prior(config.prior_alpha, config.prior_beta).ok_or_else(|| {
            invalid_parameter(
                "prior",
                "Beta threshold masses are not numerically representable",
            )
        })?;
        let mut planner = RealFftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(geometry.fft_len);
        let inverse = planner.plan_fft_inverse(geometry.fft_len);
        let lags = geometry.tau_max + 2;
        Ok(Self {
            config: config.clone(),
            sample_rate_hz,
            tau_min: geometry.tau_min,
            tau_max: geometry.tau_max,
            window: geometry.tau_max,
            buffer_len: geometry.buffer_len,
            ring: vec![0.0; geometry.buffer_len],
            ring_pos: 0,
            filled: 0,
            since_hop: 0,
            samples_seen: 0,
            conditioned: vec![0.0; geometry.buffer_len],
            prefix: vec![0.0; geometry.buffer_len + 1],
            block_input: forward.make_input_vec(),
            buffer_input: forward.make_input_vec(),
            block_spectrum: forward.make_output_vec(),
            buffer_spectrum: forward.make_output_vec(),
            forward_scratch: forward.make_scratch_vec(),
            inverse_scratch: inverse.make_scratch_vec(),
            correlation: inverse.make_output_vec(),
            difference: vec![0.0; lags],
            normalized: vec![0.0; lags],
            prior,
            frame: PitchFrame {
                end_sample: 0,
                period_samples: None,
                f0_hz: None,
                aperiodicity: None,
                voiced: false,
                voicing_probability: None,
                candidates: Vec::with_capacity(101),
                rms: None,
            },
            published: false,
            forward,
            inverse,
        })
    }

    /// Consume mono samples and return the number of frames published.
    pub fn push(&mut self, samples: &[f64]) -> usize {
        let mut updates = 0;
        for &sample in samples {
            self.samples_seen = self.samples_seen.saturating_add(1);
            self.ring[self.ring_pos] = sample;
            self.ring_pos += 1;
            if self.ring_pos == self.buffer_len {
                self.ring_pos = 0;
            }
            if self.filled < self.buffer_len {
                self.filled += 1;
                if self.filled != self.buffer_len {
                    continue;
                }
            } else {
                self.since_hop += 1;
                if self.since_hop < self.config.hop_size {
                    continue;
                }
            }
            self.since_hop = 0;
            self.analyze();
            updates += 1;
        }
        updates
    }

    /// Borrow the latest frame, or `None` before `N` samples since reset.
    pub fn frame(&self) -> Option<&PitchFrame> {
        self.published.then_some(&self.frame)
    }

    /// Analysis buffer `N = 2 * ceil(fs / fmin_hz) + 1` in samples: the
    /// warm-up before the first frame. The integration window is
    /// `W = (N - 1) / 2`, and a frame's estimate refers to about
    /// `end_sample - (W + period_samples) / 2`.
    pub fn buffer_samples(&self) -> usize {
        self.buffer_len
    }

    /// Input sample rate selected at construction.
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Integration-window length W in samples. A pitch with period T refers
    /// approximately to `frame.end_sample - (W + T)/2`.
    pub fn integration_samples(&self) -> usize {
        self.window
    }

    /// Approximate reference position of the latest YIN estimate, in input
    /// samples since reset (fractional). Undefined with the estimate.
    pub fn reference_sample(&self) -> Option<f64> {
        let frame = self.frame()?;
        Some(frame.end_sample as f64 - (self.window as f64 + frame.period_samples?) / 2.0)
    }

    /// Forget all history and re-arm the warm-up without reallocating.
    pub fn reset(&mut self) {
        self.ring_pos = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.samples_seen = 0;
        self.published = false;
        self.frame.end_sample = 0;
        self.clear_estimate();
    }

    fn clear_estimate(&mut self) {
        self.frame.period_samples = None;
        self.frame.f0_hz = None;
        self.frame.aperiodicity = None;
        self.frame.voiced = false;
        self.frame.voicing_probability = None;
        self.frame.candidates.clear();
        self.frame.rms = None;
    }

    fn analyze(&mut self) {
        self.published = true;
        self.frame.end_sample = self.samples_seen;
        self.clear_estimate();
        if !self.compute_difference() {
            return;
        }
        let Some(estimate) = yin_kernel(
            &self.difference,
            &mut self.normalized,
            self.tau_min,
            self.tau_max,
            self.config.threshold,
        ) else {
            return;
        };
        self.frame.period_samples = Some(estimate.period);
        self.frame.f0_hz = Some(f64::from(self.sample_rate_hz) / estimate.period);
        self.frame.aperiodicity = Some(estimate.aperiodicity);
        self.frame.voiced = estimate.aperiodicity < self.config.threshold;
        self.frame.voicing_probability = Some(candidate_probabilities(
            &self.difference,
            &self.normalized,
            self.tau_min,
            &self.prior,
            self.config.absolute_min_weight,
            f64::from(self.sample_rate_hz),
            &mut self.frame.candidates,
        ));
    }
}

impl PitchAnalyzer {
    /// Condition the buffer (peak scale, then mean removal) and fill
    /// `difference` with `d(0..=tau_max + 1)` from eq. (7):
    /// `d(tau) = sum a^2 + E(tau) - 2 c(tau)`, with the block energies from
    /// prefix sums of squares and `c(tau) = xcorr[N - W - tau]`, where
    /// `xcorr[m] = sum_j a[j] b[j + m] = IFFT(conj(FFT(a)) FFT(b))[m] / L` has
    /// no circular wrap for `m <= N - W`. Returns false for a non-finite
    /// sample, digital silence or an FFT error.
    fn compute_difference(&mut self) -> bool {
        let (n, w) = (self.buffer_len, self.window);
        let mut scale = 0.0_f64;
        for &sample in &self.ring {
            if !sample.is_finite() {
                return false;
            }
            scale = scale.max(sample.abs());
        }
        if scale == 0.0 {
            self.frame.rms = Some(0.0);
            return false;
        }
        // The write position holds the oldest sample once the ring is full.
        let (head, tail) = self.ring.split_at(self.ring_pos);
        let mut mean = 0.0;
        for (output, &sample) in self.conditioned.iter_mut().zip(tail.iter().chain(head)) {
            *output = sample / scale;
            mean += *output;
        }
        mean /= n as f64;
        // RMS uses its own window peak: an older loud sample must not erase
        // a quiet but representable integration-window measurement.
        let raw = tail.iter().chain(head).skip(n - w);
        let window_scale = raw.clone().fold(0.0_f64, |peak, x| peak.max(x.abs()));
        self.frame.rms = Some(if window_scale == 0.0 {
            0.0
        } else {
            let mean_square = raw.map(|x| (x / window_scale).powi(2)).sum::<f64>() / w as f64;
            mean_square.min(1.0).sqrt() * window_scale
        });
        let mut energy = 0.0;
        self.prefix[0] = 0.0;
        for (value, prefix) in self.conditioned.iter_mut().zip(&mut self.prefix[1..]) {
            *value -= mean;
            energy += *value * *value;
            *prefix = energy;
        }
        self.block_input[..w].copy_from_slice(&self.conditioned[n - w..]);
        self.block_input[w..].fill(0.0);
        self.buffer_input[..n].copy_from_slice(&self.conditioned);
        self.buffer_input[n..].fill(0.0);
        if self
            .forward
            .process_with_scratch(
                &mut self.block_input,
                &mut self.block_spectrum,
                &mut self.forward_scratch,
            )
            .is_err()
            || self
                .forward
                .process_with_scratch(
                    &mut self.buffer_input,
                    &mut self.buffer_spectrum,
                    &mut self.forward_scratch,
                )
                .is_err()
        {
            return false;
        }
        for (block, buffer) in self.block_spectrum.iter_mut().zip(&self.buffer_spectrum) {
            *block = block.conj() * buffer;
        }
        // DC and Nyquist products of real spectra are real; drop rounding signs.
        if let Some(first) = self.block_spectrum.first_mut() {
            first.im = 0.0;
        }
        if let Some(last) = self.block_spectrum.last_mut() {
            last.im = 0.0;
        }
        if self
            .inverse
            .process_with_scratch(
                &mut self.block_spectrum,
                &mut self.correlation,
                &mut self.inverse_scratch,
            )
            .is_err()
        {
            return false;
        }
        let fft_len = self.correlation.len() as f64;
        let block_energy = self.prefix[n] - self.prefix[n - w];
        self.difference[0] = 0.0;
        for (tau, output) in self.difference.iter_mut().enumerate().skip(1) {
            let lagged_energy = self.prefix[n - tau] - self.prefix[n - w - tau];
            let cross = self.correlation[n - w - tau] / fft_len;
            let value = block_energy + lagged_energy - 2.0 * cross;
            *output = if value > 0.0 { value } else { 0.0 };
        }
        true
    }
}

#[cfg(test)]
mod tests;
