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
    sorted: Vec<f64>,
    bands: Vec<Range<usize>>,
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
        let edges = if config.contrast_band_edges_hz.is_empty() {
            let mut edges = vec![bin_hz];
            for divisor in [32.0, 16.0, 8.0, 4.0, 2.0, 1.0] {
                let edge = nyquist / divisor;
                if edge > bin_hz {
                    edges.push(edge);
                }
            }
            edges
        } else {
            config.contrast_band_edges_hz.clone()
        };
        let bins = config.fft_size / 2 + 1;
        let bands: Vec<_> = edges
            .windows(2)
            .enumerate()
            .map(|(index, pair)| {
                // Compare bin centres directly so exact edge inclusion does
                // not depend on rounding a division back to an integer.
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
                start..end
            })
            .collect();
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
            sorted: vec![0.0; bins],
            bands,
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
        if edges.len() == 1
            || edges
                .iter()
                .any(|edge| !edge.is_finite() || *edge < bin_hz || *edge > sample_rate as f64 / 2.0)
            || edges.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(invalid_parameter(
                "contrast_band_edges_hz",
                "need increasing edges between first bin and Nyquist",
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

    /// Forget stream history without reallocating; the next frame needs a full window.
    pub fn reset(&mut self) {
        self.ring_pos = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.samples_seen = 0;
        self.result.clear();
        self.result.end_sample = 0;
        self.published = false;
        self.signal = SignalAccumulator::default();
        self.measurements = SignalMeasurements::default();
    }

    fn compute_frame(&mut self) {
        self.result.clear();
        let mut scale: f64 = 0.0;
        for &sample in &self.ring {
            if !sample.is_finite() {
                return;
            }
            scale = scale.max(sample.abs());
        }
        if scale == 0.0 {
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
        let mut total = 0.0;
        let mut weighted = 0.0;
        for k in 1..self.power.len() {
            let power = (self.spectrum[k] / n as f64).norm_sqr();
            self.power[k] = power;
            total += power;
            weighted += k as f64 * self.bin_hz * power;
        }
        if total <= 0.0 || !total.is_finite() {
            return;
        }
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
            sorted.copy_from_slice(&self.power[range.clone()]);
            sorted.sort_unstable_by(f64::total_cmp);
            let region = sorted.len().div_ceil(5);
            let bottom: f64 = sorted[..region].iter().sum();
            let top: f64 = sorted[sorted.len() - region..].iter().sum();
            if bottom > 0.0 {
                *contrast = Some(10.0 * (top.log10() - bottom.log10()));
            }
        }
    }
}

#[cfg(test)]
mod tests;
