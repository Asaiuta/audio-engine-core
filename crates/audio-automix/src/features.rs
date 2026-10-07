//! Per-segment envelopes and spectral onset features.
use crate::{check_cancel, AutomixError, ENVELOPE_RATE, WINDOW_SIZE_MS};
use audio_engine_core::analysis::LoudnessMeter;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Keep roughly 46 ms of spectral context across common music sample rates.
/// Bound the power-of-two plan so extreme metadata cannot size its buffers.
pub(super) fn spectral_frame_size(sample_rate: u32) -> usize {
    let exponent = (f64::from(sample_rate.max(1)) * 1024.0 / 22_050.0)
        .log2()
        .round()
        .clamp(10.0, 13.0) as u32;
    1 << exponent
}

pub(super) fn spectral_hop_size(sample_rate: u32) -> usize {
    (sample_rate as usize / 200).clamp(1, 512)
}

pub(super) fn spectral_observation_offset_sec(sample_rate: u32) -> f64 {
    // Positive flux peaks as a transient crosses the Hann window's rising
    // slope, one quarter-window before its center. A frame difference spans
    // two starts, so reference its midpoint rather than the later start.
    (0.75 * (spectral_frame_size(sample_rate) - 1) as f64
        - 0.5 * spectral_hop_size(sample_rate) as f64)
        / f64::from(sample_rate.max(1))
}

pub(super) fn envelope_window_size(sample_rate: u32) -> usize {
    (sample_rate as usize * WINDOW_SIZE_MS / 1000).max(1)
}

#[derive(Default)]
pub(super) struct AnalysisSegment {
    pub(super) start_time: f64,
    pub(super) frames_analyzed: u64,
    /// The last selected frame is the end of the input: the window ends at the
    /// known track end, or the decoder reached end of stream before filling it.
    /// A window that stops at the analysis cap leaves this false.
    pub(super) reaches_input_end: bool,
    /// Physical clock of the selected PCM; zero only for envelope-only fixtures.
    pub(super) sample_rate: u32,
    pub(super) envelope: Vec<f32>,
    /// RMS of the unfinished envelope bin. It never counts as a complete bin,
    /// but must be considered before confirming silence at end of input.
    pub(super) partial_envelope: Option<f32>,
    pub(super) low_envelope: Vec<f32>,
    pub(super) spectral_flux: Vec<f32>,
    pub(super) spectral_flux_channels: [Vec<f32>; 3],
}

impl AnalysisSegment {
    pub(super) fn at(start_time: f64) -> Self {
        Self {
            start_time,
            ..Self::default()
        }
    }

    pub(super) fn envelope_rate(&self) -> f64 {
        if self.sample_rate == 0 {
            ENVELOPE_RATE
        } else {
            f64::from(self.sample_rate) / envelope_window_size(self.sample_rate) as f64
        }
    }

    pub(super) fn end_time(&self) -> f64 {
        self.start_time
            + if self.sample_rate == 0 {
                self.envelope.len() as f64 / ENVELOPE_RATE
            } else {
                self.frames_analyzed as f64 / f64::from(self.sample_rate)
            }
    }
}

struct EnvelopeAccumulator {
    sum_sq: f32,
    count: usize,
    window_size: usize,
}

impl EnvelopeAccumulator {
    fn new(window_size: usize) -> Self {
        Self {
            sum_sq: 0.0,
            count: 0,
            window_size: window_size.max(1),
        }
    }

    fn process(&mut self, sample: f32) -> Option<f32> {
        self.sum_sq += sample * sample;
        self.count += 1;
        if self.count >= self.window_size {
            let rms = (self.sum_sq / self.window_size as f32).sqrt();
            self.sum_sq = 0.0;
            self.count = 0;
            Some(rms)
        } else {
            None
        }
    }

    fn partial_rms(&self) -> Option<f32> {
        (self.count > 0).then(|| (self.sum_sq / self.count as f32).sqrt())
    }
}

struct FirstOrderFilter {
    prev_y: f32,
    alpha: f32,
}

impl FirstOrderFilter {
    fn new(sample_rate: u32, cutoff_hz: f32) -> Self {
        let dt = 1.0 / sample_rate.max(1) as f32;
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
        let alpha = dt / (rc + dt);
        Self { prev_y: 0.0, alpha }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.prev_y + self.alpha * (x - self.prev_y);
        self.prev_y = y;
        y
    }
}

/// Sparse, unnormalized triangle on the linear FFT bins.
struct OnsetBand {
    start_bin: usize,
    weights: Vec<f32>,
    channel: usize,
}

/// Quarter-tone spacing balances spectral evidence before log compression.
/// Merge edges that land in the same FFT bin; empty low-frequency triangles
/// would otherwise repeat evidence without adding frequency resolution.
fn onset_bands(sample_rate: u32, fft_size: usize) -> Vec<OnsetBand> {
    let highest = 16_000.0_f64.min(f64::from(sample_rate) / 2.0);
    if highest <= 27.5 {
        return Vec::new();
    }
    let steps = (24.0 * (highest / 27.5).log2()).ceil() as usize;
    let mut edges: Vec<_> = (0..=steps)
        .map(|step| {
            let frequency = (27.5 * 2.0_f64.powf(step as f64 / 24.0)).min(highest);
            ((frequency * fft_size as f64 / f64::from(sample_rate)).round() as usize)
                .min(fft_size / 2)
        })
        .collect();
    edges.dedup();
    edges
        .windows(3)
        .map(|edges| {
            let (start, center, end) = (edges[0], edges[1], edges[2]);
            OnsetBand {
                start_bin: start,
                weights: (start..end)
                    .map(|bin| {
                        if bin < center {
                            (bin - start) as f32 / (center - start) as f32
                        } else {
                            (end - bin) as f32 / (end - center) as f32
                        }
                    })
                    .collect(),
                channel: if center as f64 * f64::from(sample_rate) / (fft_size as f64) < 250.0 {
                    0
                } else if center as f64 * f64::from(sample_rate) / (fft_size as f64) < 2_000.0 {
                    1
                } else {
                    2
                },
            }
        })
        .collect()
}

struct SpectralFluxAccumulator {
    /// Windowed time-domain frame. `realfft` mutates its input, so this doubles
    /// as transform scratch.
    frame: Vec<f32>,
    /// Real half-spectrum; the Nyquist bin is omitted from onset magnitudes.
    spectrum: Vec<Complex<f32>>,
    /// Workspace for `process_with_scratch`. The plain `process` allocates on
    /// every call, which this hop loop runs at the observation cadence.
    fft_scratch: Vec<Complex<f32>>,
    magnitudes: Vec<f32>,
    bands: Vec<OnsetBand>,
    previous_magnitudes: Vec<f32>,
    channel_band_counts: [usize; 3],
    last_channel_flux: [f32; 3],
    scratch: Vec<f32>,
    /// Precomputed symmetric Hann coefficients avoid per-hop trigonometry.
    window: Vec<f32>,
    hop_size: usize,
    pos: usize,
    fft: std::sync::Arc<dyn RealToComplex<f32>>,
}

pub(super) struct SegmentAnalyzer {
    sample_rate: u32,
    channels: usize,
    env_acc: EnvelopeAccumulator,
    low_acc: EnvelopeAccumulator,
    low_filter: FirstOrderFilter,
    spectral: SpectralFluxAccumulator,
}

impl SegmentAnalyzer {
    pub(super) fn new(sample_rate: u32, channels: usize) -> Self {
        let window_size = envelope_window_size(sample_rate);
        Self {
            sample_rate,
            channels,
            env_acc: EnvelopeAccumulator::new(window_size),
            low_acc: EnvelopeAccumulator::new(window_size),
            low_filter: FirstOrderFilter::new(sample_rate, 150.0),
            spectral: SpectralFluxAccumulator::new(sample_rate),
        }
    }

    pub(super) fn process(
        &mut self,
        samples: &[f64],
        meter: &mut LoudnessMeter,
        segment: &mut AnalysisSegment,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<(), AutomixError> {
        meter.process(samples)?;
        segment.sample_rate = self.sample_rate;

        for (index, frame) in samples.chunks_exact(self.channels).enumerate() {
            if index % 4_096 == 0 {
                check_cancel(cancel)?;
            }
            let mono = (frame.iter().sum::<f64>() / self.channels as f64) as f32;
            let low = self.low_filter.process(mono);

            if let Some(rms) = self.env_acc.process(mono) {
                segment.envelope.push(rms);
            }
            if let Some(rms) = self.low_acc.process(low) {
                segment.low_envelope.push(rms);
            }
            if let Some(flux) = self.spectral.process(mono) {
                segment.spectral_flux.push(flux);
                if let Some(channel_flux) = self.spectral.channel_flux() {
                    for (values, flux) in
                        segment.spectral_flux_channels.iter_mut().zip(channel_flux)
                    {
                        values.push(flux);
                    }
                }
            }
            segment.frames_analyzed += 1;
        }
        segment.partial_envelope = self.env_acc.partial_rms();
        Ok(())
    }
}

/// The symmetric Hann window used by the spectral-flux accumulator.
///
/// The complex-transform test oracle evaluates the same f32 expression inline.
fn hann_window(fft_size: usize) -> Vec<f32> {
    (0..fft_size)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (fft_size - 1) as f32).cos())
        .collect()
}

impl SpectralFluxAccumulator {
    fn new(sample_rate: u32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft_size = spectral_frame_size(sample_rate);
        let fft = planner.plan_fft_forward(fft_size);
        let bands = onset_bands(sample_rate, fft_size);
        let mut channel_band_counts = [0; 3];
        for band in &bands {
            channel_band_counts[band.channel] += 1;
        }
        Self {
            frame: vec![0.0; fft_size],
            spectrum: vec![Complex::new(0.0, 0.0); fft.complex_len()],
            fft_scratch: vec![Complex::new(0.0, 0.0); fft.get_scratch_len()],
            magnitudes: vec![0.0; fft_size / 2],
            previous_magnitudes: vec![0.0; bands.len()],
            channel_band_counts,
            last_channel_flux: [0.0; 3],
            bands,
            scratch: vec![0.0; fft_size],
            window: hann_window(fft_size),
            hop_size: spectral_hop_size(sample_rate),
            pos: 0,
            fft,
        }
    }

    fn process(&mut self, sample: f32) -> Option<f32> {
        let fft_size = self.frame.len();
        self.scratch[self.pos] = sample;
        self.pos += 1;
        if self.pos < fft_size {
            return None;
        }

        for i in 0..fft_size {
            self.frame[i] = self.scratch[i] * self.window[i];
        }
        // Lengths are fixed at construction to exactly what the plan requires,
        // so these checks cannot fail; a violated invariant would be a bug
        // here. Reuse the previous spectrum rather than panicking mid-analysis.
        debug_assert_eq!(self.frame.len(), self.fft.len());
        debug_assert_eq!(self.spectrum.len(), self.fft.complex_len());
        let _ = self.fft.process_with_scratch(
            &mut self.frame,
            &mut self.spectrum,
            &mut self.fft_scratch,
        );

        for (magnitude, bin) in self.magnitudes.iter_mut().zip(&self.spectrum) {
            *magnitude = bin.norm();
        }
        let mut flux = 0.0;
        let mut channel_flux = [0.0; 3];
        for (band, previous) in self.bands.iter().zip(&mut self.previous_magnitudes) {
            let magnitude = self.magnitudes[band.start_bin..band.start_bin + band.weights.len()]
                .iter()
                .zip(&band.weights)
                .map(|(magnitude, weight)| magnitude * weight)
                .sum::<f32>()
                .ln_1p();
            let difference = (magnitude - *previous).max(0.0);
            flux += difference;
            channel_flux[band.channel] += difference;
            *previous = magnitude;
        }
        for (flux, count) in channel_flux.iter_mut().zip(self.channel_band_counts) {
            *flux /= count.max(1) as f32;
        }
        self.last_channel_flux = channel_flux;

        self.scratch.copy_within(self.hop_size..fft_size, 0);
        self.pos = fft_size - self.hop_size;
        Some(flux / self.bands.len().max(1) as f32)
    }

    fn channel_flux(&self) -> Option<[f32; 3]> {
        self.channel_band_counts
            .iter()
            .all(|&count| count > 0)
            .then_some(self.last_channel_flux)
    }
}

#[cfg(test)]
mod tests;
