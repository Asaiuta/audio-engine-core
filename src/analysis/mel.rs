//! Frozen Slaney mel frontends and MFCCs.
//!
//! This module deliberately exposes two named geometries rather than a
//! generic hidden default. They are the two frontends already frozen in the
//! repository's tempo research: the 40-band N2 frontend and the 128-band H2
//! domain frontend. The analyzer is an unpadded streaming reader; resampling,
//! global standardisation and model-specific f16 conversion remain outside
//! this crate.

use super::{SpectrumAnalyzer, WindowFunction};
use crate::processor::traits::{validate_sample_rate_hz, ProcessError};
use realfft::{num_complex::Complex, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Mel filter normalization used by a [`MelConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MelNormalization {
    /// Multiply each triangle by `2 / (upper_hz - lower_hz)` (Slaney area).
    Area,
    /// Leave the triangular peak at one.
    None,
}

/// Amplitude scaling applied to the positive-frequency FFT magnitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MelSpectrumScale {
    /// Use `abs(rfft)` with no FFT-size scaling.
    RawMagnitude,
    /// Use `abs(rfft) / sqrt(fft_size)` (the frozen H2 convention).
    SqrtFftSize,
}

/// Optional log compression applied after the mel filterbank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MelLogCompression {
    /// Keep the linear mel energies.
    None,
    /// Apply `ln(1 + 1000*x)` (the frozen H2 convention).
    Log1pThousand,
}

/// A fully specified Slaney mel frontend geometry.
///
/// There is deliberately no `Default`: callers choose
/// [`MelConfig::neural_frontend`] or [`MelConfig::domain_128`], or spell out a
/// custom geometry, so a frozen frontend is never selected implicitly.
#[derive(Debug, Clone)]
pub struct MelConfig {
    /// FFT size, a power of two at least four samples.
    pub fft_size: usize,
    /// Samples between frames, in `1..=fft_size`.
    pub hop_size: usize,
    /// Shared periodic/symmetric window definition.
    pub window: WindowFunction,
    /// Number of triangular filters.
    pub bands: usize,
    /// Lower mel filter edge in Hz.
    pub fmin_hz: f64,
    /// Upper mel filter edge in Hz.
    pub fmax_hz: f64,
    /// Filter normalization.
    pub normalization: MelNormalization,
    /// FFT magnitude convention.
    pub spectrum_scale: MelSpectrumScale,
    /// Optional post-filter log compression.
    pub log_compression: MelLogCompression,
    /// If present, `new` requires this exact input rate. Named frozen
    /// constructors set this field; custom configurations may leave it `None`.
    pub expected_sample_rate_hz: Option<u32>,
}

impl MelConfig {
    /// Frozen N2 tempo frontend: 11025 Hz, 1024/512, 40 area-normalized
    /// Slaney bands from 20 to 5000 Hz, raw magnitude.
    pub fn neural_frontend() -> Self {
        Self {
            fft_size: 1024,
            hop_size: 512,
            window: WindowFunction::Hann,
            bands: 40,
            fmin_hz: 20.0,
            fmax_hz: 5000.0,
            normalization: MelNormalization::Area,
            spectrum_scale: MelSpectrumScale::RawMagnitude,
            log_compression: MelLogCompression::None,
            expected_sample_rate_hz: Some(11_025),
        }
    }

    /// Frozen H2 domain frontend: 22050 Hz, 1024/441, 128 raw Slaney bands
    /// from 30 to 11000 Hz and `ln(1 + 1000*x)` compression.
    pub fn domain_128() -> Self {
        Self {
            fft_size: 1024,
            hop_size: 441,
            window: WindowFunction::Hann,
            bands: 128,
            fmin_hz: 30.0,
            fmax_hz: 11_000.0,
            normalization: MelNormalization::None,
            spectrum_scale: MelSpectrumScale::SqrtFftSize,
            log_compression: MelLogCompression::Log1pThousand,
            expected_sample_rate_hz: Some(22_050),
        }
    }
}

/// Latest mel frame. Invalid frames retain the frame timing but set `valid`
/// false and zero the preallocated values.
#[derive(Debug, Clone, PartialEq)]
pub struct MelFrame {
    /// Exclusive end of the frame in input samples since reset.
    pub end_sample: u64,
    /// Whether all input samples and the FFT were finite and valid.
    pub valid: bool,
    /// One value per configured mel filter.
    pub values: Vec<f64>,
}

/// Streaming mel-spectrogram analyzer for one explicit frozen geometry.
pub struct MelAnalyzer {
    config: MelConfig,
    sample_rate_hz: u32,
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
    filters: Vec<f64>,
    frame: MelFrame,
    published: bool,
}

fn invalid_parameter(parameter: &'static str, message: &'static str) -> ProcessError {
    ProcessError::InvalidParameter {
        processor: "MelAnalyzer",
        parameter,
        message,
    }
}

fn hz_to_mel(hz: f64) -> f64 {
    if hz < 1000.0 {
        hz * 0.015
    } else {
        15.0 + 27.0 * (hz / 1000.0).ln() / 6.4_f64.ln()
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    if mel < 15.0 {
        mel / 0.015
    } else {
        1000.0 * (6.4_f64.powf((mel - 15.0) / 27.0))
    }
}

fn validate_config(config: &MelConfig, sample_rate_hz: u32) -> Result<(), ProcessError> {
    validate_sample_rate_hz("MelAnalyzer", sample_rate_hz)?;
    if let Some(expected) = config.expected_sample_rate_hz {
        if expected != sample_rate_hz {
            return Err(ProcessError::SampleRateMismatch {
                processor: "MelAnalyzer",
                expected_sample_rate_hz: expected,
                actual_sample_rate_hz: sample_rate_hz,
            });
        }
    }
    if config.fft_size < 4
        || !config.fft_size.is_power_of_two()
        || config.hop_size == 0
        || config.hop_size > config.fft_size
    {
        return Err(ProcessError::InvalidGeometry {
            processor: "MelAnalyzer",
            operation: "new",
            message: "FFT must be a power of two >= 4 and hop must be in 1..=FFT",
        });
    }
    if config.bands == 0 || config.bands > config.fft_size / 2 + 1 {
        return Err(invalid_parameter("bands", "must be in 1..=fft_size/2+1"));
    }
    let nyquist = sample_rate_hz as f64 / 2.0;
    if !config.fmin_hz.is_finite()
        || !config.fmax_hz.is_finite()
        || config.fmin_hz < 0.0
        || config.fmin_hz >= config.fmax_hz
        || config.fmax_hz > nyquist
    {
        return Err(invalid_parameter(
            "fmin_hz/fmax_hz",
            "must be finite, ordered, and inside 0..=Nyquist",
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
    Ok(())
}

fn filterbank(config: &MelConfig, sample_rate_hz: u32) -> Vec<f64> {
    let bins = config.fft_size / 2 + 1;
    let lower = hz_to_mel(config.fmin_hz);
    let upper = hz_to_mel(config.fmax_hz);
    let edges: Vec<f64> = (0..=config.bands + 1)
        .map(|i| mel_to_hz(lower + (upper - lower) * i as f64 / (config.bands + 1) as f64))
        .collect();
    let mut filters = vec![0.0; config.bands * bins];
    let bin_hz = sample_rate_hz as f64 / config.fft_size as f64;
    for band in 0..config.bands {
        let left = edges[band];
        let center = edges[band + 1];
        let right = edges[band + 2];
        let area_scale = match config.normalization {
            MelNormalization::Area => 2.0 / (right - left),
            MelNormalization::None => 1.0,
        };
        for k in 0..bins {
            let frequency = k as f64 * bin_hz;
            let weight = if frequency < left || frequency > right {
                0.0
            } else if frequency <= center {
                (frequency - left) / (center - left)
            } else {
                (right - frequency) / (right - center)
            };
            filters[band * bins + k] = weight.max(0.0) * area_scale;
        }
    }
    filters
}

impl MelAnalyzer {
    /// Validate the geometry and allocate the complete streaming state.
    pub fn new(config: &MelConfig, sample_rate_hz: u32) -> Result<Self, ProcessError> {
        validate_config(config, sample_rate_hz)?;
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(config.fft_size);
        let bands = config.bands;
        Ok(Self {
            config: config.clone(),
            sample_rate_hz,
            bin_hz: sample_rate_hz as f64 / config.fft_size as f64,
            ring: vec![0.0; config.fft_size],
            ring_pos: 0,
            filled: 0,
            since_hop: 0,
            samples_seen: 0,
            window: SpectrumAnalyzer::window_values(config.window, config.fft_size),
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            filters: filterbank(config, sample_rate_hz),
            frame: MelFrame {
                end_sample: 0,
                valid: false,
                values: vec![0.0; bands],
            },
            published: false,
            fft,
        })
    }

    /// Consume mono samples and return the number of complete frames published.
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
            self.frame.end_sample = self.samples_seen;
            self.compute_frame();
            self.published = true;
            updates += 1;
        }
        updates
    }

    /// Borrow the latest frame, including an invalid frame marker after a
    /// non-finite input window.
    pub fn frame(&self) -> Option<&MelFrame> {
        self.published.then_some(&self.frame)
    }

    /// Borrow the latest valid mel values only.
    pub fn mel(&self) -> Option<&[f64]> {
        self.published
            .then_some(&self.frame)
            .filter(|frame| frame.valid)
            .map(|frame| frame.values.as_slice())
    }

    /// Frequency spacing of the underlying FFT bins.
    pub fn bin_width_hz(&self) -> f64 {
        self.bin_hz
    }

    /// Input rate selected at construction.
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Configured mel filterbank rows, in band-major order. This is a borrowed
    /// diagnostic view and does not allocate.
    pub fn filters(&self) -> &[f64] {
        &self.filters
    }

    /// Forget history without reallocating.
    pub fn reset(&mut self) {
        self.ring_pos = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.samples_seen = 0;
        self.frame.end_sample = 0;
        self.frame.valid = false;
        self.frame.values.fill(0.0);
        self.published = false;
    }

    fn compute_frame(&mut self) {
        self.frame.valid = false;
        self.frame.values.fill(0.0);
        if self.ring.iter().any(|sample| !sample.is_finite()) {
            return;
        }
        for (index, input) in self.input.iter_mut().enumerate() {
            *input = self.ring[(self.ring_pos + index) % self.config.fft_size] * self.window[index];
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            return;
        }
        let scale = match self.config.spectrum_scale {
            MelSpectrumScale::RawMagnitude => 1.0,
            MelSpectrumScale::SqrtFftSize => (self.config.fft_size as f64).sqrt(),
        };
        let bins = self.config.fft_size / 2 + 1;
        for band in 0..self.config.bands {
            let mut value = 0.0;
            for k in 0..bins {
                value += self.filters[band * bins + k] * self.spectrum[k].norm() / scale;
            }
            self.frame.values[band] = match self.config.log_compression {
                MelLogCompression::None => value,
                MelLogCompression::Log1pThousand => (1.0 + 1000.0 * value).ln(),
            };
        }
        self.frame.valid = self.frame.values.iter().all(|value| value.is_finite());
        if !self.frame.valid {
            self.frame.values.fill(0.0);
        }
    }
}

/// MFCC configuration over an explicitly selected [`MelConfig`].
///
/// The coefficients are exactly the DCT-II with `norm = ortho`,
/// `c[j] = s_j * sum_n x[n] * cos(pi / M * (n + 0.5) * j)` with
/// `s_0 = sqrt(1 / M)`, `s_j = sqrt(2 / M)` for `j > 0` and `M = mel.bands`.
/// With `apply_log`, `x[n] = ln(max(mel[n], log_floor))` of the selected
/// geometry's magnitude mel; without it, `x[n]` is the geometry's own value,
/// which is the intended input only when the geometry log-compresses itself
/// (for example [`MelLogCompression::Log1pThousand`]). This is not librosa's
/// `power_to_db` MFCC and claims no numerical parity with it.
#[derive(Debug, Clone)]
pub struct MfccConfig {
    /// Mel frontend to analyze.
    pub mel: MelConfig,
    /// Number of DCT-II orthonormal coefficients, including coefficient zero.
    pub coefficients: usize,
    /// Apply `ln(max(mel, log_floor))` before DCT-II. Must be false when
    /// `mel.log_compression` is not [`MelLogCompression::None`];
    /// [`MfccConfig::from_mel`] derives it from the geometry.
    pub apply_log: bool,
    /// Positive floor used when `apply_log` is enabled.
    pub log_floor: f64,
}

impl MfccConfig {
    /// Construct an MFCC configuration over a selected mel geometry.
    /// `apply_log` is true exactly when the geometry has no log compression of
    /// its own; `log_floor` is `1e-12`.
    pub fn from_mel(mel: MelConfig, coefficients: usize) -> Self {
        let apply_log = mel.log_compression == MelLogCompression::None;
        Self {
            mel,
            coefficients,
            apply_log,
            log_floor: 1e-12,
        }
    }
}

/// Latest MFCC frame.
#[derive(Debug, Clone, PartialEq)]
pub struct MfccFrame {
    /// Exclusive frame end in input samples since reset.
    pub end_sample: u64,
    /// Whether the underlying mel frame was valid.
    pub valid: bool,
    /// DCT-II orthonormal coefficients.
    pub coefficients: Vec<f64>,
}

/// Streaming DCT-II (`norm = ortho`) MFCC analyzer.
///
/// Every published frame applies the transform defined on [`MfccConfig`] to
/// the latest mel frame: `ln(max(mel, log_floor))` of a magnitude mel
/// geometry, or directly the geometry's own log-compressed values. It is not
/// librosa's `power_to_db` MFCC and claims no parity with it.
pub struct MfccAnalyzer {
    config: MfccConfig,
    mel: MelAnalyzer,
    frame: MfccFrame,
    scratch: Vec<f64>,
    published: bool,
}

/// Orthonormal DCT-II of `input` into `output` without allocating:
/// `output[j] = s_j * sum_n input[n] * cos(pi / M * (n + 0.5) * j)` with
/// `M = input.len()`, `s_0 = sqrt(1 / M)` and `s_j = sqrt(2 / M)` for `j > 0`.
/// `output.len()` selects how many leading coefficients are computed.
fn dct2_ortho(input: &[f64], output: &mut [f64]) {
    let m = input.len() as f64;
    for (coefficient, out) in output.iter_mut().enumerate() {
        let mut sum = 0.0;
        for (index, &value) in input.iter().enumerate() {
            sum += value
                * (std::f64::consts::PI / m * (index as f64 + 0.5) * coefficient as f64).cos();
        }
        let scale = if coefficient == 0 {
            (1.0 / m).sqrt()
        } else {
            (2.0 / m).sqrt()
        };
        *out = sum * scale;
    }
}

impl MfccAnalyzer {
    /// Validate the mel geometry and coefficient policy, then allocate state.
    /// `apply_log` must be false when the mel geometry already log-compresses;
    /// every configuration error is reported before any allocation.
    pub fn new(config: &MfccConfig, sample_rate_hz: u32) -> Result<Self, ProcessError> {
        if config.coefficients == 0 || config.coefficients > config.mel.bands {
            return Err(ProcessError::InvalidParameter {
                processor: "MfccAnalyzer",
                parameter: "coefficients",
                message: "must be in 1..=mel.bands",
            });
        }
        if !config.log_floor.is_finite() || config.log_floor <= 0.0 {
            return Err(ProcessError::InvalidParameter {
                processor: "MfccAnalyzer",
                parameter: "log_floor",
                message: "must be finite and positive",
            });
        }
        if config.apply_log && config.mel.log_compression != MelLogCompression::None {
            return Err(ProcessError::InvalidParameter {
                processor: "MfccAnalyzer",
                parameter: "apply_log",
                message: "must be false when the mel geometry is already log-compressed",
            });
        }
        let mel = MelAnalyzer::new(&config.mel, sample_rate_hz)?;
        Ok(Self {
            config: config.clone(),
            mel,
            frame: MfccFrame {
                end_sample: 0,
                valid: false,
                coefficients: vec![0.0; config.coefficients],
            },
            scratch: vec![0.0; config.mel.bands],
            published: false,
        })
    }

    /// Consume mono samples and return the number of MFCC frames published.
    pub fn push(&mut self, samples: &[f64]) -> usize {
        let count = self.mel.push(samples);
        if count == 0 {
            return 0;
        }
        let Some(mel_frame) = self.mel.frame() else {
            return 0;
        };
        self.frame.end_sample = mel_frame.end_sample;
        self.frame.valid = mel_frame.valid;
        self.frame.coefficients.fill(0.0);
        if !mel_frame.valid {
            self.published = true;
            return count;
        }
        for (index, value) in mel_frame.values.iter().enumerate() {
            self.scratch[index] = if self.config.apply_log {
                value.max(self.config.log_floor).ln()
            } else {
                *value
            };
        }
        dct2_ortho(&self.scratch, &mut self.frame.coefficients);
        self.frame.valid = self
            .frame
            .coefficients
            .iter()
            .all(|value| value.is_finite());
        self.published = true;
        count
    }

    /// Borrow the latest frame, including an invalid marker.
    pub fn frame(&self) -> Option<&MfccFrame> {
        self.published.then_some(&self.frame)
    }

    /// Borrow the latest valid coefficients only.
    pub fn coefficients(&self) -> Option<&[f64]> {
        self.frame
            .valid
            .then_some(self.frame.coefficients.as_slice())
    }

    /// Borrow the underlying mel analyzer's latest frame.
    pub fn mel(&self) -> Option<&MelFrame> {
        self.mel.frame()
    }

    /// Forget history without reallocating.
    pub fn reset(&mut self) {
        self.mel.reset();
        self.frame.end_sample = 0;
        self.frame.valid = false;
        self.frame.coefficients.fill(0.0);
        self.published = false;
    }
}

#[cfg(test)]
mod tests;
