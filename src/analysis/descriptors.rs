//! Linear-bin spectral descriptors, without display pooling or ballistics.

use super::signal::SignalAccumulator;
use super::spectrum::MAX_KAISER_BETA;
use super::{SignalMeasurements, SpectrumAnalyzer, WindowFunction};
use crate::processor::traits::{validate_sample_rate_hz, ProcessError};
use realfft::{num_complex::Complex, RealFftPlanner, RealToComplex};
use std::ops::Range;
use std::sync::Arc;

/// Geometry and numerical conventions for [`DescriptorAnalyzer`].
#[derive(Debug, Clone)]
pub struct DescriptorConfig {
    /// Power-of-two window length, at least 4 samples; default 4096.
    pub fft_size: usize,
    /// Samples between frames, in `1..=fft_size`; default 1024.
    pub hop_size: usize,
    /// Shared spectrum window definition; default periodic Hann.
    pub window: WindowFunction,
    /// Cumulative-power fraction in `(0, 1]`; default 0.85.
    pub rolloff_fraction: f64,
    /// Positive finite power floor for flatness, in squared input units.
    ///
    /// Both means use `max(|FFT(x * window) / N|^2, floor)`; default
    /// `1e-20`. DC is removed before windowing. The floor affects quiet
    /// frames, but never turns a zero-energy frame into a defined result.
    pub flatness_floor: f64,
    /// Optional explicit contrast band edges in Hz, strictly increasing,
    /// between `sample_rate / fft_size` and Nyquist inclusive.
    ///
    /// Empty selects edges at the first positive bin, then Nyquist times
    /// `1/32, 1/16, 1/8, 1/4, 1/2, 1`, keeping only edges above that bin.
    /// Non-empty needs at least two edges. Bands include their lower edge
    /// and exclude their upper edge, except the last includes its upper edge.
    /// Bands containing fewer than two bins have undefined contrast.
    pub contrast_band_edges_hz: Vec<f64>,
    /// Optional power/rise band edges in Hz. Empty reuses the documented
    /// Nyquist-relative default used by `contrast_band_edges_hz`.
    pub band_edges_hz: Vec<f64>,
    /// Additive power floor used by band `rise_db`, in squared input units.
    pub band_rise_floor: f64,
    /// Positive finite clipping threshold in absolute input units; default 1.
    pub clipping_threshold: f64,
}

impl Default for DescriptorConfig {
    fn default() -> Self {
        Self {
            fft_size: 4096,
            hop_size: 1024,
            window: WindowFunction::Hann,
            rolloff_fraction: 0.85,
            flatness_floor: 1e-20,
            contrast_band_edges_hz: Vec::new(),
            band_edges_hz: Vec::new(),
            band_rise_floor: 1e-10,
            clipping_threshold: 1.0,
        }
    }
}

/// Latest full-window spectral measurements over bins `1..=N/2`.
///
/// The unwindowed frame mean is subtracted before applying the window; DC
/// and negative-frequency bins are excluded, Nyquist is included without
/// doubling any bin. Weights are linear power `P[k] = |FFT[k] / N|^2`.
/// No display tilt, clamping, rebanding, smoothing or peak hold is applied.
/// Silence, constant DC, and frames containing non-finite input have `None`
/// in every descriptor. Band geometry may make contrast undefined separately.
#[derive(Debug, Clone, PartialEq)]
pub struct SpectralDescriptors {
    /// Exclusive frame end in input samples since reset, saturating at `u64::MAX`.
    pub end_sample: u64,
    /// Power-weighted mean frequency `sum(f*P) / sum(P)`, in Hz.
    pub centroid_hz: Option<f64>,
    /// Spectral spread `sqrt(sum((f-centroid)^2*P) / sum(P))`, in Hz.
    pub bandwidth_hz: Option<f64>,
    /// Smallest bin frequency reaching the configured cumulative power fraction, in Hz.
    pub rolloff_hz: Option<f64>,
    /// Geometric/arithmetic mean of floored power, dimensionless in `[0, 1]`.
    ///
    /// Equal power gives 1. A single white-noise periodogram approaches
    /// `exp(-EulerGamma) ~= 0.561`, not 1; averaging power spectra first is
    /// a different estimator. No temporal averaging is performed here.
    pub flatness: Option<f64>,
    /// Per-band `10*log10(mean(top)/mean(bottom))`, in dB.
    ///
    /// Each region has `ceil(0.2 * bin_count)` bins, sorted by power.
    /// No floor is applied. Fewer than two bins or a zero valley mean yields
    /// `None`; equal nonzero powers give 0 dB. Region means are arithmetic.
    pub contrast_db: Vec<Option<f64>>,
}

/// Latest linear power and dB-rise measurements for the configured frequency
/// bands. Power is the ascending sum of the absolute raw-bin powers; it is not
/// a PSD and has no window or equivalent-noise-bandwidth compensation.
#[derive(Debug, Clone, PartialEq)]
pub struct BandMeasurements {
    /// Exclusive frame end in input samples since reset.
    pub end_sample: u64,
    /// Sum of raw power bins in each band. Every band is `None` when the frame
    /// was invalid or any band sum was not representable in `f64`; a band
    /// without bins is always `None` in all three fields.
    pub power: Vec<Option<f64>>,
    /// `10*log10(power)` for positive power, otherwise `None`.
    pub level_db: Vec<Option<f64>>,
    /// Positive frame-to-frame rise in dB,
    /// `max(0, 10*log10(power + floor) - 10*log10(previous + floor))` with the
    /// configured `band_rise_floor`; a band that did not rise reports `+0.0`.
    /// The first frame after construction, reset or an invalid frame has no
    /// predecessor and therefore reports `None`, as does a band whose
    /// `power + floor` is not representable in `f64` in either frame. This is
    /// a band-energy measurement, not AutoMix's tempo onset function.
    pub rise_db: Vec<Option<f64>>,
}

impl BandMeasurements {
    fn new(count: usize) -> Self {
        Self {
            end_sample: 0,
            power: vec![None; count],
            level_db: vec![None; count],
            rise_db: vec![None; count],
        }
    }

    fn clear(&mut self) {
        self.power.fill(None);
        self.level_db.fill(None);
        self.rise_db.fill(None);
    }
}

impl SpectralDescriptors {
    fn clear(&mut self) {
        self.centroid_hz = None;
        self.bandwidth_hz = None;
        self.rolloff_hz = None;
        self.flatness = None;
        self.contrast_db.fill(None);
    }
}

/// Streaming mono spectral and cumulative time-domain descriptor accumulator.
///
/// Construction owns all allocations. [`push`](Self::push), both borrowed
/// accessors, and [`reset`](Self::reset) allocate nothing. Feed from an
/// analysis worker: allocation freedom does not promise a callback deadline.
/// Callers own downmixing. The first spectral frame ends after `fft_size`
/// samples, later frames every `hop_size`; partial tails are not padded.
/// Chunk boundaries cannot change accumulation order or FFT positions.
///
/// Non-finite samples invalidate the containing spectral frames; spectral
/// output recovers when they leave the window. Cumulative signal measurements
/// remain undefined until reset, rather than silently omitting bad samples.
pub struct DescriptorAnalyzer {
    config: DescriptorConfig,
    bin_hz: f64,
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
    power: Vec<f64>,
    raw_power: Vec<f64>,
    raw_power_valid: bool,
    sorted: Vec<f64>,
    bands: Vec<Range<usize>>,
    band_ranges: Vec<Range<usize>>,
    band_measurements: BandMeasurements,
    previous_band_power: Vec<f64>,
    current_band_power: Vec<f64>,
    previous_band_valid: bool,
    bands_published: bool,
    result: SpectralDescriptors,
    published: bool,
    signal: SignalAccumulator,
    measurements: SignalMeasurements,
}

fn invalid_parameter(parameter: &'static str, message: &'static str) -> ProcessError {
    ProcessError::InvalidParameter {
        processor: "DescriptorAnalyzer",
        parameter,
        message,
    }
}

fn valid_edges(edges: &[f64], bin_hz: f64, nyquist: f64) -> bool {
    (edges.is_empty() || edges.len() >= 2)
        && edges
            .iter()
            .all(|edge| edge.is_finite() && *edge >= bin_hz && *edge <= nyquist)
        && edges.windows(2).all(|pair| pair[0] < pair[1])
}

fn resolve_edges(edges: &[f64], bin_hz: f64, nyquist: f64) -> Vec<f64> {
    if !edges.is_empty() {
        return edges.to_vec();
    }
    let mut resolved = vec![bin_hz];
    for divisor in [32.0, 16.0, 8.0, 4.0, 2.0, 1.0] {
        let edge = nyquist / divisor;
        if edge > bin_hz {
            resolved.push(edge);
        }
    }
    resolved
}

fn ranges_from_edges(edges: &[f64], bin_hz: f64, bins: usize) -> Vec<Range<usize>> {
    edges
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            // Compare bin centres directly so exact edge inclusion does not
            // depend on rounding a division back to an integer.
            let start = (1..bins)
                .find(|&k| k as f64 * bin_hz >= pair[0])
                .unwrap_or(bins);
            let end = (start..bins)
                .find(|&k| {
                    if index + 2 == edges.len() {
                        k as f64 * bin_hz > pair[1]
                    } else {
                        k as f64 * bin_hz >= pair[1]
                    }
                })
                .unwrap_or(bins);
            start.saturating_sub(1)..end.saturating_sub(1)
        })
        .collect()
}

impl DescriptorAnalyzer {
    /// Validate configuration, then allocate FFT, ring, window and scratch state.
    ///
    /// # Errors
    ///
    /// Before any allocation: zero sample rate returns `InvalidSampleRate`;
    /// FFT sizes below 4, non-powers-of-two, unrepresentable buffer sizes, or
    /// hops outside `1..=fft_size` return `InvalidGeometry`. Invalid fractions,
    /// floors, clipping thresholds, band edges, or Kaiser beta (outside finite
    /// `0..=50`) return `InvalidParameter`. All nonzero sample rates are valid.
    pub fn new(config: &DescriptorConfig, sample_rate: u32) -> Result<Self, ProcessError> {
        Self::validate(config, sample_rate)?;
        let bin_hz = sample_rate as f64 / config.fft_size as f64;
        let nyquist = sample_rate as f64 / 2.0;
        let contrast_edges = resolve_edges(&config.contrast_band_edges_hz, bin_hz, nyquist);
        let band_edges = resolve_edges(&config.band_edges_hz, bin_hz, nyquist);
        let bins = config.fft_size / 2 + 1;
        let bands = ranges_from_edges(&contrast_edges, bin_hz, bins);
        let band_ranges = ranges_from_edges(&band_edges, bin_hz, bins);
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(config.fft_size);
        let result = SpectralDescriptors {
            end_sample: 0,
            centroid_hz: None,
            bandwidth_hz: None,
            rolloff_hz: None,
            flatness: None,
            contrast_db: vec![None; bands.len()],
        };
        Ok(Self {
            bin_hz,
            ring: vec![0.0; config.fft_size],
            ring_pos: 0,
            filled: 0,
            since_hop: 0,
            samples_seen: 0,
            window: SpectrumAnalyzer::window_values(config.window, config.fft_size),
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            power: vec![0.0; bins],
            raw_power: vec![0.0; bins - 1],
            raw_power_valid: false,
            sorted: vec![0.0; bins],
            bands,
            band_ranges: band_ranges.clone(),
            band_measurements: BandMeasurements::new(band_ranges.len()),
            previous_band_power: vec![0.0; band_ranges.len()],
            current_band_power: vec![0.0; band_ranges.len()],
            previous_band_valid: false,
            bands_published: false,
            fft,
            result,
            published: false,
            signal: SignalAccumulator::default(),
            measurements: SignalMeasurements::default(),
            config: config.clone(),
        })
    }

    fn validate(config: &DescriptorConfig, sample_rate: u32) -> Result<(), ProcessError> {
        validate_sample_rate_hz("DescriptorAnalyzer", sample_rate)?;
        if config.fft_size < 4
            || !config.fft_size.is_power_of_two()
            || config.fft_size > isize::MAX as usize / std::mem::size_of::<Complex<f64>>()
            || config.hop_size == 0
            || config.hop_size > config.fft_size
        {
            return Err(ProcessError::InvalidGeometry {
                processor: "DescriptorAnalyzer",
                operation: "new",
                message: "FFT must be a representable power of two >= 4; hop must be in 1..=FFT",
            });
        }
        if !config.rolloff_fraction.is_finite()
            || config.rolloff_fraction <= 0.0
            || config.rolloff_fraction > 1.0
        {
            return Err(invalid_parameter(
                "rolloff_fraction",
                "must be finite and in (0, 1]",
            ));
        }
        for (parameter, value) in [
            ("flatness_floor", config.flatness_floor),
            ("clipping_threshold", config.clipping_threshold),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(invalid_parameter(parameter, "must be finite and positive"));
            }
        }
        if let WindowFunction::Kaiser { beta } = config.window {
            if !beta.is_finite() || !(0.0..=MAX_KAISER_BETA).contains(&beta) {
                return Err(invalid_parameter(
                    "window.beta",
                    "must be finite and in 0..=50",
                ));
            }
        }
        let edges = &config.contrast_band_edges_hz;
        let bin_hz = sample_rate as f64 / config.fft_size as f64;
        if !valid_edges(edges, bin_hz, sample_rate as f64 / 2.0) {
            return Err(invalid_parameter(
                "contrast_band_edges_hz",
                "need increasing edges between first bin and Nyquist",
            ));
        }
        if !valid_edges(&config.band_edges_hz, bin_hz, sample_rate as f64 / 2.0) {
            return Err(invalid_parameter(
                "band_edges_hz",
                "need increasing edges between first bin and Nyquist",
            ));
        }
        if !config.band_rise_floor.is_finite() || config.band_rise_floor <= 0.0 {
            return Err(invalid_parameter(
                "band_rise_floor",
                "must be finite and positive",
            ));
        }
        Ok(())
    }

    /// Consume mono samples and return the number of full spectral frames published.
    ///
    /// Only the last frame is retained; signal measurements include all supplied
    /// samples, including any partial spectral tail. Empty input is a no-op.
    pub fn push(&mut self, samples: &[f64]) -> usize {
        let mut updates = 0;
        for &sample in samples {
            self.signal.push(sample, self.config.clipping_threshold);
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
            self.result.end_sample = self.samples_seen;
            self.compute_frame();
            self.published = true;
            updates += 1;
        }
        self.measurements = self.signal.measurements();
        updates
    }

    /// Borrow the latest complete frame, or `None` during warm-up.
    pub fn spectral(&self) -> Option<&SpectralDescriptors> {
        self.published.then_some(&self.result)
    }

    /// Borrow cumulative unwindowed measurements since construction or reset.
    pub fn signal(&self) -> &SignalMeasurements {
        &self.measurements
    }

    /// Borrow the latest absolute linear power bins, indexed as `k = i + 1`.
    /// The values use the same frame, window and DC removal as `spectral()`;
    /// they are in squared input units, are not a PSD, and are not doubled at
    /// interior frequencies. The slice is absent during warm-up, for invalid
    /// frames, and when a non-zero bin cannot be represented in `f64`.
    pub fn power_spectrum(&self) -> Option<&[f64]> {
        self.raw_power_valid.then_some(self.raw_power.as_slice())
    }

    /// Frequency spacing of `power_spectrum()` bins in Hz,
    /// `sample_rate_hz / fft_size`.
    pub fn bin_width_hz(&self) -> f64 {
        self.bin_hz
    }

    /// Borrow the latest configured band measurements, or `None` before the
    /// first complete frame. `band_bins()` gives the corresponding bin ranges.
    pub fn bands(&self) -> Option<&BandMeasurements> {
        self.bands_published.then_some(&self.band_measurements)
    }

    /// Raw-bin ranges for each configured band. Ranges index
    /// `power_spectrum()` and therefore start at slice index zero (`k = 1`).
    pub fn band_bins(&self) -> &[Range<usize>] {
        &self.band_ranges
    }

    /// Forget stream history without reallocating; the next frame needs a full window.
    pub fn reset(&mut self) {
        self.ring_pos = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.samples_seen = 0;
        self.result.clear();
        self.result.end_sample = 0;
        self.published = false;
        self.raw_power.fill(0.0);
        self.raw_power_valid = false;
        self.band_measurements.clear();
        self.band_measurements.end_sample = 0;
        self.previous_band_power.fill(0.0);
        self.current_band_power.fill(0.0);
        self.previous_band_valid = false;
        self.bands_published = false;
        self.signal = SignalAccumulator::default();
        self.measurements = SignalMeasurements::default();
    }

    fn compute_frame(&mut self) {
        self.result.clear();
        self.raw_power.fill(0.0);
        self.raw_power_valid = false;
        // Every completed frame publishes band state, like `spectral()`: the
        // early returns below leave all band fields `None` for this frame.
        self.band_measurements.clear();
        self.band_measurements.end_sample = self.result.end_sample;
        self.bands_published = true;
        let mut scale: f64 = 0.0;
        for &sample in &self.ring {
            if !sample.is_finite() {
                self.previous_band_valid = false;
                return;
            }
            scale = scale.max(sample.abs());
        }
        if scale == 0.0 {
            self.raw_power_valid = true;
            self.publish_bands();
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
            self.previous_band_valid = false;
            return;
        }
        let mut total = 0.0;
        let mut weighted = 0.0;
        let mut absolute_valid = true;
        for k in 1..self.power.len() {
            let power = (self.spectrum[k] / n as f64).norm_sqr();
            self.power[k] = power;
            let scaled = power * scale;
            let absolute = scaled * scale;
            if !absolute.is_finite() || (power > 0.0 && absolute == 0.0) {
                absolute_valid = false;
            } else {
                self.raw_power[k - 1] = absolute;
            }
            total += power;
            weighted += k as f64 * self.bin_hz * power;
        }
        self.raw_power_valid = absolute_valid;
        if total <= 0.0 || !total.is_finite() {
            self.publish_bands();
            return;
        }
        self.publish_bands();
        let centroid = weighted / total;
        self.result.centroid_hz = Some(centroid);
        let mut variance = 0.0;
        let mut cumulative = 0.0;
        let mut max_log = f64::NEG_INFINITY;
        let log_scale = 2.0 * scale.ln();
        let log_floor = self.config.flatness_floor.ln();
        for k in 1..self.power.len() {
            let frequency = k as f64 * self.bin_hz;
            variance += (frequency - centroid).powi(2) * self.power[k];
            cumulative += self.power[k];
            if self.result.rolloff_hz.is_none()
                && cumulative >= self.config.rolloff_fraction * total
            {
                self.result.rolloff_hz = Some(frequency);
            }
            // Log-space floors avoid overflow/underflow in absolute power.
            let log_power = (self.power[k].ln() + log_scale).max(log_floor);
            self.sorted[k] = log_power;
            max_log = max_log.max(log_power);
        }
        self.result.bandwidth_hz = Some((variance / total).sqrt());
        let count = (self.power.len() - 1) as f64;
        let mut shifted_logs = 0.0;
        let mut shifted_power = 0.0;
        for &log_power in &self.sorted[1..] {
            shifted_logs += log_power - max_log;
            shifted_power += (log_power - max_log).exp();
        }
        self.result.flatness =
            Some(((shifted_logs / count).exp() / (shifted_power / count)).min(1.0));
        for (range, contrast) in self.bands.iter().zip(&mut self.result.contrast_db) {
            if range.len() < 2 {
                continue;
            }
            let sorted = &mut self.sorted[..range.len()];
            let power_range = (range.start + 1)..(range.end + 1);
            sorted.copy_from_slice(&self.power[power_range]);
            sorted.sort_unstable_by(f64::total_cmp);
            let region = sorted.len().div_ceil(5);
            let bottom: f64 = sorted[..region].iter().sum();
            let top: f64 = sorted[sorted.len() - region..].iter().sum();
            if bottom > 0.0 {
                *contrast = Some(10.0 * (top.log10() - bottom.log10()));
            }
        }
    }

    fn publish_bands(&mut self) {
        // Frame-level strict representability: if the raw bins or any band
        // sum are not representable, every band field of this frame stays
        // `None` (cleared by `compute_frame`) and the rise chain restarts.
        let mut current_valid = self.raw_power_valid;
        for (range, current) in self.band_ranges.iter().zip(&mut self.current_band_power) {
            if !current_valid {
                break;
            }
            // Ascending-bin order is part of the contract.
            let sum = self.raw_power[range.clone()]
                .iter()
                .fold(0.0, |sum, &value| sum + value);
            current_valid = sum.is_finite();
            *current = sum;
        }
        if !current_valid {
            self.previous_band_valid = false;
            return;
        }
        let floor = self.config.band_rise_floor;
        for (index, range) in self.band_ranges.iter().enumerate() {
            // A band without bins has no power, level or rise.
            if range.is_empty() {
                continue;
            }
            let power = self.current_band_power[index];
            self.band_measurements.power[index] = Some(power);
            self.band_measurements.level_db[index] = (power > 0.0).then(|| 10.0 * power.log10());
            if self.previous_band_valid {
                // A difference of logs, not a ratio: `(E + eps) / (E_prev + eps)`
                // overflows for a loud frame after a silent one. Only an
                // unrepresentable `E + eps` leaves the rise non-finite.
                let previous = self.previous_band_power[index];
                let rise = 10.0 * ((power + floor).log10() - (previous + floor).log10());
                // Explicit comparison so a zero rise is always `+0.0`.
                self.band_measurements.rise_db[index] =
                    rise.is_finite()
                        .then_some(if rise > 0.0 { rise } else { 0.0 });
            }
        }
        self.previous_band_power
            .copy_from_slice(&self.current_band_power);
        self.previous_band_valid = true;
    }
}

#[cfg(test)]
mod tests;
