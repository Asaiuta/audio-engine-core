//! Streaming spectral harmonic/percussive masks.
//!
//! [`HpssAnalyzer`] is intentionally a measurement API. It publishes soft
//! masks and energy fractions with a fixed centred-context look-ahead; it does
//! not emit separated audio and makes no musical-accuracy claim.
//!
//! Any rhythm use of the masks or fractions (onset, beat, or tempo evidence)
//! needs its own screen before it is relied on; nothing here validates it.

use super::{SpectrumAnalyzer, WindowFunction};
use crate::processor::traits::{validate_sample_rate_hz, ProcessError};
use realfft::{num_complex::Complex, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Configuration for a streaming spectral HPSS mask analyzer.
#[derive(Debug, Clone)]
pub struct HpssConfig {
    /// Power-of-two frame length, at least four samples.
    pub fft_size: usize,
    /// Samples between STFT frames.
    pub hop_size: usize,
    /// Shared analysis window.
    pub window: WindowFunction,
    /// Odd temporal median length in frames; default 17. The median spans
    /// `harmonic_median_frames * hop_size` input samples: with the defaults,
    /// 17 x 1024 samples, about 395 ms at 44.1 kHz.
    pub harmonic_median_frames: usize,
    /// Odd frequency median length in bins; default 17. The median spans
    /// `percussive_median_bins * sample_rate / fft_size` Hz: with the defaults,
    /// 17 x 10.77 Hz, about 183 Hz at 44.1 kHz with FFT size 4096.
    pub percussive_median_bins: usize,
    /// Positive exponent applied to median magnitudes; default 2.
    pub mask_exponent: f64,
    /// Margin `beta >= 1`; one gives complementary harmonic/percussive masks.
    pub margin: f64,
}

impl Default for HpssConfig {
    fn default() -> Self {
        Self {
            fft_size: 4096,
            hop_size: 1024,
            window: WindowFunction::Hann,
            harmonic_median_frames: 17,
            percussive_median_bins: 17,
            mask_exponent: 2.0,
            margin: 1.0,
        }
    }
}

/// Latest masked spectral frame. Bin vectors are indexed as `k = i + 1` and
/// use `None` when the frame/context is invalid or a mask is undefined.
///
/// Per bin, `H` is the median of `power` over the `harmonic_median_frames`
/// frames centred on this one and `V` the median over the
/// `percussive_median_bins` bins centred on `k` (edges reflected). With
/// `p = mask_exponent` and `b = margin^p`:
/// `harmonic = H^(p/2) / (H^(p/2) + b V^(p/2))`,
/// `percussive = V^(p/2) / (V^(p/2) + b H^(p/2))` and
/// `residual = 1 - harmonic - percussive`. Masks and fractions are
/// dimensionless; the sample rate enters only through `end_sample` and the
/// physical spans of the two medians.
#[derive(Debug, Clone, PartialEq)]
pub struct HpssFrame {
    /// Exclusive end of the masked centre frame in input samples.
    pub end_sample: u64,
    /// Absolute power `|FFT/N|^2` of the centre frame in squared input units,
    /// bit-identical to `DescriptorAnalyzer::power_spectrum` for that frame.
    /// Every bin is `None` when any frame of the temporal context is invalid.
    pub power: Vec<Option<f64>>,
    /// Harmonic soft mask in `[0, 1]`; `None` where `H = V = 0`, exactly
    /// `1.0` where `V = 0 < H`.
    pub harmonic_mask: Vec<Option<f64>>,
    /// Percussive soft mask in `[0, 1]`; `None` where `H = V = 0`, exactly
    /// `1.0` where `H = 0 < V`.
    pub percussive_mask: Vec<Option<f64>>,
    /// Residual mask `1 - harmonic - percussive` in `[0, 1]`, computed without
    /// cancellation. With margin one it is exactly `Some(0.0)` and
    /// `harmonic + percussive == 1.0` holds bitwise where masks are defined.
    pub residual_mask: Vec<Option<f64>>,
    /// `sum(harmonic[k] * power[k]) / sum(power[k])` over the centre frame;
    /// `None` when the frame is invalid or has zero power.
    pub harmonic_fraction: Option<f64>,
    /// As `harmonic_fraction`, weighted by the percussive mask.
    pub percussive_fraction: Option<f64>,
    /// As `harmonic_fraction`, weighted by the residual mask.
    pub residual_fraction: Option<f64>,
    /// Share of the centre-frame power in bins whose masks are undefined; the
    /// four fractions sum to one within rounding.
    pub unassigned_fraction: Option<f64>,
}

/// Streaming spectral HPSS analyzer with fixed centred-context latency.
pub struct HpssAnalyzer {
    config: HpssConfig,
    sample_rate_hz: u32,
    ring: Vec<f64>,
    ring_pos: usize,
    filled: usize,
    since_hop: usize,
    samples_seen: u64,
    window: Vec<f64>,
    fft: Arc<dyn RealToComplex<f64>>,
    input: Vec<f64>,
    spectrum: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    bins: usize,
    time_logs: Vec<f64>,
    time_powers: Vec<f64>,
    row_valid: Vec<bool>,
    median_scratch: Vec<f64>,
    stft_frames_seen: usize,
    frame: HpssFrame,
    published: bool,
}

fn invalid_parameter(parameter: &'static str, message: &'static str) -> ProcessError {
    ProcessError::InvalidParameter {
        processor: "HpssAnalyzer",
        parameter,
        message,
    }
}

fn reflect_index(index: isize, length: usize) -> usize {
    let length = length as isize;
    if index < 0 {
        (-index - 1) as usize
    } else if index >= length {
        (2 * length - index - 1) as usize
    } else {
        index as usize
    }
}

fn validate_config(config: &HpssConfig, sample_rate_hz: u32) -> Result<(), ProcessError> {
    validate_sample_rate_hz("HpssAnalyzer", sample_rate_hz)?;
    if config.fft_size < 4
        || !config.fft_size.is_power_of_two()
        || config.hop_size == 0
        || config.hop_size > config.fft_size
    {
        return Err(ProcessError::InvalidGeometry {
            processor: "HpssAnalyzer",
            operation: "new",
            message: "FFT must be a power of two >= 4 and hop must be in 1..=FFT",
        });
    }
    if config.harmonic_median_frames < 3 || config.harmonic_median_frames.is_multiple_of(2) {
        return Err(invalid_parameter(
            "harmonic_median_frames",
            "must be odd and at least 3",
        ));
    }
    let max_bins = config.fft_size / 2;
    if config.percussive_median_bins < 3
        || config.percussive_median_bins.is_multiple_of(2)
        || config.percussive_median_bins > max_bins
    {
        return Err(invalid_parameter(
            "percussive_median_bins",
            "must be odd, at least 3, and no larger than fft_size/2",
        ));
    }
    if !config.mask_exponent.is_finite() || config.mask_exponent <= 0.0 {
        return Err(invalid_parameter(
            "mask_exponent",
            "must be finite and positive",
        ));
    }
    if !config.margin.is_finite() || config.margin < 1.0 {
        return Err(invalid_parameter(
            "margin",
            "must be finite and at least one",
        ));
    }
    if let WindowFunction::Kaiser { beta } = config.window {
        if !beta.is_finite() || !(0.0..=50.0).contains(&beta) {
            return Err(invalid_parameter(
                "window.beta",
                "must be finite and in 0..=50",
            ));
        }
    }
    let rows = config.harmonic_median_frames;
    let bins = max_bins;
    if rows
        .checked_mul(bins)
        .and_then(|values| values.checked_mul(std::mem::size_of::<f64>()))
        .is_none()
    {
        return Err(ProcessError::InvalidGeometry {
            processor: "HpssAnalyzer",
            operation: "new",
            message: "HPSS context storage is not representable",
        });
    }
    Ok(())
}

impl HpssAnalyzer {
    /// Validate configuration and allocate the complete streaming state.
    pub fn new(config: &HpssConfig, sample_rate_hz: u32) -> Result<Self, ProcessError> {
        validate_config(config, sample_rate_hz)?;
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(config.fft_size);
        let bins = config.fft_size / 2;
        let rows = config.harmonic_median_frames;
        let vector = |value| vec![value; bins];
        Ok(Self {
            config: config.clone(),
            sample_rate_hz,
            ring: vec![0.0; config.fft_size],
            ring_pos: 0,
            filled: 0,
            since_hop: 0,
            samples_seen: 0,
            window: SpectrumAnalyzer::window_values(config.window, config.fft_size),
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            bins,
            time_logs: vec![f64::NEG_INFINITY; rows * bins],
            time_powers: vec![0.0; rows * bins],
            row_valid: vec![false; rows],
            median_scratch: vec![0.0; rows.max(config.percussive_median_bins)],
            stft_frames_seen: 0,
            frame: HpssFrame {
                end_sample: 0,
                power: vector(None),
                harmonic_mask: vector(None),
                percussive_mask: vector(None),
                residual_mask: vector(None),
                harmonic_fraction: None,
                percussive_fraction: None,
                residual_fraction: None,
                unassigned_fraction: None,
            },
            published: false,
            fft,
        })
    }

    /// Consume mono samples and return the number of HPSS frames published.
    pub fn push(&mut self, samples: &[f64]) -> usize {
        let mut updates = 0;
        for &sample in samples {
            self.samples_seen = self.samples_seen.saturating_add(1);
            self.ring[self.ring_pos] = sample;
            self.ring_pos = (self.ring_pos + 1) % self.config.fft_size;
            if self.filled < self.config.fft_size {
                self.filled += 1;
                if self.filled != self.config.fft_size {
                    continue;
                }
            } else {
                self.since_hop += 1;
                if self.since_hop < self.config.hop_size {
                    continue;
                }
            }
            self.since_hop = 0;
            self.compute_stft_row();
            if self.stft_frames_seen >= self.config.harmonic_median_frames - 1 {
                let centre = self.stft_frames_seen - self.config.harmonic_median_frames / 2;
                self.publish_centre(centre);
                updates += 1;
            }
            self.stft_frames_seen = self.stft_frames_seen.saturating_add(1);
        }
        updates
    }

    /// Borrow the latest frame, or `None` until a complete temporal context is
    /// available.
    pub fn frame(&self) -> Option<&HpssFrame> {
        self.published.then_some(&self.frame)
    }

    /// Centred temporal look-ahead in input samples.
    pub fn lookahead_samples(&self) -> usize {
        self.config.harmonic_median_frames / 2 * self.config.hop_size
    }

    /// Input sample rate selected at construction.
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Forget all context without reallocating.
    pub fn reset(&mut self) {
        self.ring_pos = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.samples_seen = 0;
        self.time_logs.fill(f64::NEG_INFINITY);
        self.time_powers.fill(0.0);
        self.row_valid.fill(false);
        self.stft_frames_seen = 0;
        self.clear_frame();
        self.published = false;
    }

    fn clear_frame(&mut self) {
        self.frame.end_sample = 0;
        self.frame.power.fill(None);
        self.frame.harmonic_mask.fill(None);
        self.frame.percussive_mask.fill(None);
        self.frame.residual_mask.fill(None);
        self.frame.harmonic_fraction = None;
        self.frame.percussive_fraction = None;
        self.frame.residual_fraction = None;
        self.frame.unassigned_fraction = None;
    }

    fn compute_stft_row(&mut self) {
        let slot = self.stft_frames_seen % self.config.harmonic_median_frames;
        let start = slot * self.bins;
        self.row_valid[slot] = false;
        self.time_logs[start..start + self.bins].fill(f64::NEG_INFINITY);
        self.time_powers[start..start + self.bins].fill(0.0);
        let mut scale: f64 = 0.0;
        for &sample in &self.ring {
            if !sample.is_finite() {
                return;
            }
            scale = scale.max(sample.abs());
        }
        if scale == 0.0 {
            self.row_valid[slot] = true;
            return;
        }
        let n = self.config.fft_size;
        let mut mean = 0.0;
        for (index, input) in self.input.iter_mut().enumerate() {
            *input = self.ring[(self.ring_pos + index) % n] / scale;
            mean += *input;
        }
        mean /= n as f64;
        for (input, window) in self.input.iter_mut().zip(&self.window) {
            *input = (*input - mean) * window;
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            return;
        }
        for k in 1..=self.bins {
            let power = (self.spectrum[k] / n as f64).norm_sqr();
            let absolute = (power * scale) * scale;
            if !absolute.is_finite() || (power > 0.0 && absolute == 0.0) {
                return;
            }
            self.time_powers[start + k - 1] = absolute;
            // Absolute log power: frames carry different peak scales, so the
            // temporal median must not compare per-frame normalised powers.
            self.time_logs[start + k - 1] = if power > 0.0 {
                power.ln() + 2.0 * scale.ln()
            } else {
                f64::NEG_INFINITY
            };
        }
        self.row_valid[slot] = true;
    }

    fn publish_centre(&mut self, centre: usize) {
        self.clear_frame();
        let end = (self.config.fft_size as u64)
            .saturating_add((centre as u64).saturating_mul(self.config.hop_size as u64));
        self.frame.end_sample = end;
        let rows = self.config.harmonic_median_frames;
        let half_rows = rows / 2;
        let centre_slot = centre % rows;
        let first = self.stft_frames_seen - (rows - 1);
        if !(0..rows).all(|offset| self.row_valid[(first + offset) % rows]) {
            self.published = true;
            return;
        }
        for bin in 0..self.bins {
            for offset in 0..rows {
                let row = (first + offset) % rows;
                self.median_scratch[offset] = self.time_logs[row * self.bins + bin];
            }
            self.median_scratch[..rows].sort_unstable_by(f64::total_cmp);
            let harmonic_log = self.median_scratch[half_rows];

            let half_bins = self.config.percussive_median_bins / 2;
            for offset in 0..=half_bins * 2 {
                let relative = bin as isize + offset as isize - half_bins as isize;
                let reflected = reflect_index(relative, self.bins);
                self.median_scratch[offset] = self.time_logs[centre_slot * self.bins + reflected];
            }
            let count = self.config.percussive_median_bins;
            self.median_scratch[..count].sort_unstable_by(f64::total_cmp);
            let percussive_log = self.median_scratch[count / 2];

            let (harmonic, percussive, residual) = self.masks(harmonic_log, percussive_log);
            self.frame.power[bin] = Some(self.time_powers[centre_slot * self.bins + bin]);
            self.frame.harmonic_mask[bin] = harmonic;
            self.frame.percussive_mask[bin] = percussive;
            self.frame.residual_mask[bin] = residual;
        }
        self.compute_fractions(centre_slot);
        self.published = true;
    }

    fn masks(
        &self,
        harmonic_log_power: f64,
        percussive_log_power: f64,
    ) -> (Option<f64>, Option<f64>, Option<f64>) {
        if harmonic_log_power == f64::NEG_INFINITY && percussive_log_power == f64::NEG_INFINITY {
            return (None, None, None);
        }
        if harmonic_log_power == f64::NEG_INFINITY {
            return (Some(0.0), Some(1.0), Some(0.0));
        }
        if percussive_log_power == f64::NEG_INFINITY {
            return (Some(1.0), Some(0.0), Some(0.0));
        }
        // `z = q (ln H - ln V)` with `q = p / 2`. `ratio = exp(-|z|)` in
        // [0, 1] is the minority-to-majority ratio of the powered medians, so
        // the smaller mask keeps its relative precision at extreme ratios.
        let z = self.config.mask_exponent * 0.5 * (harmonic_log_power - percussive_log_power);
        let ratio = (-z.abs()).exp();
        let (majority, minority, residual) = if self.config.margin == 1.0 {
            // Minority `sigma(-|z|)`; the majority `1 - s` partitions exactly.
            let minority = ratio / (1.0 + ratio);
            (1.0 - minority, minority, 0.0)
        } else {
            let margin_power = self.config.margin.powf(self.config.mask_exponent);
            if margin_power.is_infinite() {
                return (Some(0.0), Some(0.0), Some(1.0));
            }
            // Cancellation-free `R = r (b^2 - 1) / ((1 + b r)(r + b))` with
            // `b = beta^p`, factored so that `b^2` cannot overflow.
            let residual = ratio * (margin_power - 1.0) / (ratio + margin_power)
                * ((margin_power + 1.0) / (1.0 + margin_power * ratio));
            (
                1.0 / (1.0 + margin_power * ratio),
                ratio / (ratio + margin_power),
                residual,
            )
        };
        if z >= 0.0 {
            (Some(majority), Some(minority), Some(residual))
        } else {
            (Some(minority), Some(majority), Some(residual))
        }
    }

    fn compute_fractions(&mut self, centre_slot: usize) {
        let row = &self.time_powers[centre_slot * self.bins..(centre_slot + 1) * self.bins];
        // Sum powers relative to the row peak: the absolute total of a valid
        // frame can overflow even though every fraction lies in [0, 1].
        let peak = row.iter().fold(0.0_f64, |peak, &power| peak.max(power));
        if peak <= 0.0 {
            return;
        }
        let mut total = 0.0;
        let mut harmonic = 0.0;
        let mut percussive = 0.0;
        let mut residual = 0.0;
        let mut unassigned = 0.0;
        for (bin, &power) in row.iter().enumerate() {
            let power = power / peak;
            total += power;
            match (
                self.frame.harmonic_mask[bin],
                self.frame.percussive_mask[bin],
                self.frame.residual_mask[bin],
            ) {
                (Some(h), Some(p), Some(r)) => {
                    harmonic += h * power;
                    percussive += p * power;
                    residual += r * power;
                }
                _ => unassigned += power,
            }
        }
        self.frame.harmonic_fraction = Some(harmonic / total);
        self.frame.percussive_fraction = Some(percussive / total);
        self.frame.residual_fraction = Some(residual / total);
        self.frame.unassigned_fraction = Some(unassigned / total);
    }
}

#[cfg(test)]
mod tests;
