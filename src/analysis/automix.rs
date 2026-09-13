//! AutoMix offline audio analysis.
//!
//! This module is intentionally pure/backend-side. It decodes bounded head/tail
//! windows off the realtime callback path and returns a stable DTO for later
//! transition planning.

mod tempo;

use crate::analysis::LoudnessMeter;
use crate::decoder::{
    DecodeCancelToken, DecoderError, HttpCredentials, MediaLocation, StreamingDecoder,
};
use crate::processor::traits::ProcessError;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use tempo::{BeatGrid, TempoEstimate};
use thiserror::Error;

// Unreleased v4: Structure's agreed semantics will join this schema before
// release. A commit is not a schema release; cached v3 results need recomputing.
const ANALYSIS_VERSION: u32 = 4;
const DEFAULT_MAX_ANALYZE_TIME_SEC: f64 = 60.0;
const MIN_ANALYZE_TIME_SEC: f64 = 5.0;
const MAX_ANALYZE_TIME_SEC: f64 = 300.0;
const ENVELOPE_RATE: f64 = 50.0;
/// Longest container-declared duration this analysis will treat as real.
///
/// The declared duration comes from untrusted container metadata but sizes the
/// whole-track [`AutomixAnalysis::energy_profile`], one slot per
/// [`ENERGY_PROFILE_RATE`]. Without a bound, a file declaring an absurd
/// duration would ask for an allocation proportional to it, which `vec!` cannot
/// fail gracefully. Twenty-four hours is far beyond any mixable track and still
/// caps the profile at well under ten megabytes.
const MAX_DECLARED_DURATION_SEC: f64 = 24.0 * 60.0 * 60.0;
/// Slots per second in the whole-track energy profile.
const ENERGY_PROFILE_RATE: f64 = 10.0;
const WINDOW_SIZE_MS: usize = 20;
const SILENCE_THRESHOLD_DB: f32 = -48.0;
const HEAD_BEAT_TOLERANCE_SEC: f64 = 0.010;

/// Keep roughly 46 ms of spectral context across common music sample rates.
/// Bound the power-of-two plan so extreme metadata cannot size its buffers.
fn spectral_frame_size(sample_rate: u32) -> usize {
    let exponent = (f64::from(sample_rate.max(1)) * 1024.0 / 22_050.0)
        .log2()
        .round()
        .clamp(10.0, 13.0) as u32;
    1 << exponent
}

fn spectral_hop_size(sample_rate: u32) -> usize {
    (sample_rate as usize / 200).clamp(1, 512)
}

fn spectral_observation_offset_sec(sample_rate: u32) -> f64 {
    // Positive flux peaks as a transient crosses the Hann window's rising
    // slope, one quarter-window before its center. A frame difference spans
    // two starts, so reference its midpoint rather than the later start.
    (0.75 * (spectral_frame_size(sample_rate) - 1) as f64
        - 0.5 * spectral_hop_size(sample_rate) as f64)
        / f64::from(sample_rate.max(1))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
/// Amount of a track covered by an AutoMix analysis pass.
pub enum AutomixAnalysisMode {
    /// Analyze only the head/tail windows needed for placement decisions.
    Head,
    /// Analyze the head window plus the trailing tail window.
    ///
    /// Only these two bounded windows are decoded: the track interior between
    /// them is never read, its `energy_profile` entries stay zero, and the
    /// reported loudness covers the analyzed windows rather than the whole
    /// track.
    #[default]
    Full,
}

impl AutomixAnalysisMode {
    /// Whether this mode covers the trailing tail window.
    pub fn includes_tail(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Clone, Debug, Serialize)]
/// Stable offline-analysis result used to plan an AutoMix transition.
pub struct AutomixAnalysis {
    /// Analysis algorithm version; consumers may gate on it.
    pub version: u32,
    /// Analysis mode that produced this result.
    pub mode: AutomixAnalysisMode,
    /// Analyzed track duration in seconds.
    ///
    /// This is the placement timeline, not the decoded-coverage duration: it
    /// is taken from container metadata when plausible, else derived from the
    /// total frame count, and falls back to the analyzed head span when
    /// neither is available. In [`AutomixAnalysisMode::Full`] the interior
    /// between the two analyzed windows is never decoded.
    pub duration: f64,
    /// Requested per-window analysis duration in seconds.
    ///
    /// This reports the configured cap from
    /// [`AutomixAnalysisOptions::max_analyze_time_sec`] rather than the
    /// realized head length; the head window is clamped to the track duration
    /// when the track is shorter than the cap.
    pub analyze_window: f64,
    /// Fitted constant tempo, rounded to 0.01 BPM only for reporting.
    ///
    /// A present tempo may summarize a drifting performance. Consult
    /// [`Self::beat_grid_stability`] before using its constant grid.
    pub bpm: Option<f64>,
    /// Prior-weighted autocorrelation salience times grid stability and the
    /// fraction of tracked beats supported by observed onsets, in 0..1.
    ///
    /// This is an evidence score, not a calibrated probability of correctness.
    /// Without a fitted grid it can report the available periodicity evidence;
    /// silence/insufficient input produces `None`. v3 thresholds do not apply.
    pub bpm_confidence: Option<f64>,
    /// First reported beat at or after the analyzed head origin, in absolute
    /// source seconds. A short isolated head transient can include a fitted
    /// beat up to 10 ms before the origin; that event is reported at the origin.
    /// Internal cut snapping and stability retain the unrounded fitted grid.
    /// This is not a downbeat or bar-start claim.
    pub first_beat_pos: Option<f64>,
    /// Constant-grid stability in 0..1, derived from RMS phase residual.
    ///
    /// Residuals include all supported tracked beats, including robust-fit
    /// outliers. The score is `1 - min(1, residual / min(period/4, 0.125))`,
    /// with times in seconds, so slow grids cannot hide large absolute errors.
    /// `None` (JSON null) means no grid was fitted. Cut snapping
    /// requires stability >=0.80 and [`Self::bpm_confidence`] >=0.35.
    pub beat_grid_stability: Option<f64>,
    /// Integrated loudness in LUFS, when measurable.
    pub loudness: Option<f64>,
    /// True-peak level in dBTP, when measurable.
    pub true_peak_dbtp: Option<f64>,
    /// Recommended fade-in position in seconds.
    pub fade_in_pos: f64,
    /// Recommended fade-out position in seconds.
    pub fade_out_pos: f64,
    /// Beat-aligned cut-in position in seconds, when found.
    pub cut_in_pos: Option<f64>,
    /// Beat-aligned cut-out position in seconds, when found.
    pub cut_out_pos: Option<f64>,
    /// Center of the mixable section in seconds.
    pub mix_center_pos: f64,
    /// Start of the mixable section in seconds.
    pub mix_start_pos: f64,
    /// End of the mixable section in seconds.
    pub mix_end_pos: f64,
    /// Energy envelope slots carry evidence; the interval between them stays
    /// zero. The length follows [`Self::duration`], bounded by an internal
    /// 24-hour `MAX_DECLARED_DURATION_SEC` cap, so a file declaring an absurd
    /// duration cannot size this vector.
    pub energy_profile: Vec<f64>,
    /// Drop (beat-matched break) position in seconds, when found.
    pub drop_pos: Option<f64>,
    /// First vocal entry position in seconds, when detected.
    pub vocal_in_pos: Option<f64>,
    /// Final vocal exit position in seconds, when detected.
    pub vocal_out_pos: Option<f64>,
    /// Last vocal entry position before the outro in seconds, when detected.
    pub vocal_last_in_pos: Option<f64>,
    /// RMS energy of the outro window, when measured.
    pub outro_energy_level: Option<f64>,
}

/// Failures produced by bounded offline AutoMix analysis.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AutomixError {
    /// Analysis was cooperatively canceled.
    #[error("AutoMix analysis canceled")]
    Canceled,
    /// Opening, seeking, or decoding the media source failed.
    #[error("AutoMix decoder operation failed")]
    Decoder(#[from] DecoderError),
    /// EBU R128 construction or ingestion failed during analysis.
    #[error("AutoMix loudness analysis failed")]
    Loudness(#[from] ProcessError),
    /// A coarse decoder seek landed after the requested tail boundary.
    #[error(
        "AutoMix tail seek landed after planned frame {planned_frame}: realized frame {realized_frame}"
    )]
    TailSeekPastStart {
        /// First frame the bounded tail analysis intended to decode.
        planned_frame: u64,
        /// Actual decoder position after the coarse seek.
        realized_frame: u64,
    },
}

#[derive(Clone, Debug)]
/// Bounds and coverage mode for one AutoMix analysis pass.
pub struct AutomixAnalysisOptions {
    /// Which analysis mode to run.
    pub mode: AutomixAnalysisMode,
    /// Per-window source-audio duration cap in seconds.
    ///
    /// Each decoded window (the head, and the tail in
    /// [`AutomixAnalysisMode::Full`]) covers at most this many seconds of
    /// source audio; it is not a wall-clock compute-time budget. Non-finite
    /// values reset to the built-in default.
    pub max_analyze_time_sec: f64,
}

impl Default for AutomixAnalysisOptions {
    fn default() -> Self {
        Self {
            mode: AutomixAnalysisMode::Full,
            max_analyze_time_sec: DEFAULT_MAX_ANALYZE_TIME_SEC,
        }
    }
}

impl AutomixAnalysisOptions {
    /// Clamp analysis time to a finite in-range value.
    pub fn normalized(mut self) -> Self {
        if !self.max_analyze_time_sec.is_finite() {
            self.max_analyze_time_sec = DEFAULT_MAX_ANALYZE_TIME_SEC;
        }
        self.max_analyze_time_sec = self
            .max_analyze_time_sec
            .clamp(MIN_ANALYZE_TIME_SEC, MAX_ANALYZE_TIME_SEC);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameWindow {
    start: u64,
    end: u64,
}

impl FrameWindow {
    fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    fn start_time(self, sample_rate: u32) -> f64 {
        self.start as f64 / sample_rate.max(1) as f64
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AnalysisWindowPlan {
    head: FrameWindow,
    tail: Option<FrameWindow>,
}

impl AnalysisWindowPlan {
    fn new(mode: AutomixAnalysisMode, track_frames: Option<u64>, window_frames: u64) -> Self {
        let window_frames = window_frames.max(1);
        let head_end = track_frames.map_or(window_frames, |frames| frames.min(window_frames));
        let head = FrameWindow {
            start: 0,
            end: head_end,
        };
        let tail = track_frames.and_then(|frames| {
            (mode.includes_tail() && frames > head.end).then(|| FrameWindow {
                start: head.end.max(frames.saturating_sub(window_frames)),
                end: frames,
            })
        });

        Self { head, tail }
    }
}

#[derive(Default)]
struct AnalysisSegment {
    start_time: f64,
    frames_analyzed: u64,
    envelope: Vec<f32>,
    low_envelope: Vec<f32>,
    vocal_ratio: Vec<f32>,
    spectral_flux: Vec<f32>,
    spectral_flux_channels: [Vec<f32>; 3],
}

impl AnalysisSegment {
    fn at(start_time: f64) -> Self {
        Self {
            start_time,
            ..Self::default()
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
}

struct FirstOrderFilter {
    prev_x: f32,
    prev_y: f32,
    alpha: f32,
    high_pass: bool,
}

impl FirstOrderFilter {
    fn new(sample_rate: u32, cutoff_hz: f32, high_pass: bool) -> Self {
        let dt = 1.0 / sample_rate.max(1) as f32;
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
        let alpha = if high_pass {
            rc / (rc + dt)
        } else {
            dt / (rc + dt)
        };
        Self {
            prev_x: 0.0,
            prev_y: 0.0,
            alpha,
            high_pass,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = if self.high_pass {
            self.alpha * (self.prev_y + x - self.prev_x)
        } else {
            self.prev_y + self.alpha * (x - self.prev_y)
        };
        self.prev_x = x;
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

struct SegmentAnalyzer {
    channels: usize,
    env_acc: EnvelopeAccumulator,
    low_acc: EnvelopeAccumulator,
    vocal_acc: EnvelopeAccumulator,
    low_filter: FirstOrderFilter,
    vocal_lowpass: FirstOrderFilter,
    vocal_highpass: FirstOrderFilter,
    spectral: SpectralFluxAccumulator,
}

impl SegmentAnalyzer {
    fn new(sample_rate: u32, channels: usize) -> Self {
        let window_size = (sample_rate as usize * WINDOW_SIZE_MS / 1000).max(1);
        Self {
            channels,
            env_acc: EnvelopeAccumulator::new(window_size),
            low_acc: EnvelopeAccumulator::new(window_size),
            vocal_acc: EnvelopeAccumulator::new(window_size),
            low_filter: FirstOrderFilter::new(sample_rate, 150.0, false),
            vocal_lowpass: FirstOrderFilter::new(sample_rate, 3_000.0, false),
            vocal_highpass: FirstOrderFilter::new(sample_rate, 200.0, true),
            spectral: SpectralFluxAccumulator::new(sample_rate),
        }
    }

    fn process(
        &mut self,
        samples: &[f64],
        meter: &mut LoudnessMeter,
        segment: &mut AnalysisSegment,
        cancel: Option<&DecodeCancelToken>,
    ) -> Result<(), AutomixError> {
        meter.process(samples)?;

        for (index, frame) in samples.chunks_exact(self.channels).enumerate() {
            if index % 4_096 == 0 {
                check_cancel(cancel)?;
            }
            let mono = (frame.iter().sum::<f64>() / self.channels as f64) as f32;
            let low = self.low_filter.process(mono);
            let vocal = self
                .vocal_lowpass
                .process(self.vocal_highpass.process(mono));

            if let Some(rms) = self.env_acc.process(mono) {
                segment.envelope.push(rms);
            }
            if let Some(rms) = self.low_acc.process(low) {
                segment.low_envelope.push(rms);
            }
            if let Some(rms) = self.vocal_acc.process(vocal) {
                let base = segment.envelope.last().copied().unwrap_or(1.0);
                segment
                    .vocal_ratio
                    .push(if base > 0.0001 { rms / base } else { 0.0 });
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

/// Run bounded offline AutoMix analysis on a media location.
pub fn analyze_automix(
    location: MediaLocation,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
) -> Result<AutomixAnalysis, AutomixError> {
    analyze_automix_with_cancel(location, credentials, options, None)
}

/// Run bounded AutoMix analysis with a cooperative cancel token.
pub fn analyze_automix_with_cancel(
    location: MediaLocation,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<AutomixAnalysis, AutomixError> {
    let options = options.normalized();
    check_cancel(cancel_token.as_ref())?;
    let mut decoder = StreamingDecoder::open_with_credentials_and_cancel(
        location,
        credentials.as_ref(),
        cancel_token.clone(),
    )?;

    let sample_rate = decoder.info().sample_rate;
    let channels = decoder.info().channels.max(1);
    let declared_duration = decoder.info().duration_secs.filter(is_plausible_duration);
    let track_frames = decoder
        .info()
        .total_frames
        .filter(|frames| is_plausible_duration(&frames_to_seconds(*frames, sample_rate)))
        .or_else(|| {
            declared_duration.and_then(|duration| frames_for_duration(duration, sample_rate))
        });
    let duration = declared_duration
        .or_else(|| track_frames.map(|frames| frames_to_seconds(frames, sample_rate)))
        .unwrap_or(0.0);
    let window_frames = frames_for_duration(options.max_analyze_time_sec, sample_rate)
        .unwrap_or(1)
        .max(1);
    let plan = AnalysisWindowPlan::new(options.mode, track_frames, window_frames);
    let mut meter = LoudnessMeter::new(channels, sample_rate)?;
    let mut head = AnalysisSegment::at(plan.head.start_time(sample_rate));
    let mut tail = AnalysisSegment::default();

    decode_segment(
        &mut decoder,
        &mut meter,
        &mut head,
        0,
        plan.head.len(),
        cancel_token.as_ref(),
    )?;

    if let Some(tail_window) = plan.tail {
        check_cancel(cancel_token.as_ref())?;
        decoder.seek(tail_window.start_time(sample_rate))?;
        let realized_start = decoder.current_frame();
        let skip_frames = tail_window.start.checked_sub(realized_start).ok_or(
            AutomixError::TailSeekPastStart {
                planned_frame: tail_window.start,
                realized_frame: realized_start,
            },
        )?;
        tail = AnalysisSegment::at(tail_window.start_time(sample_rate));
        decode_segment(
            &mut decoder,
            &mut meter,
            &mut tail,
            skip_frames,
            tail_window.len(),
            cancel_token.as_ref(),
        )?;
    }

    finalize_analysis(
        &options,
        duration,
        sample_rate,
        &meter,
        &head,
        &tail,
        cancel_token.as_ref(),
    )
}

fn decode_segment(
    decoder: &mut StreamingDecoder,
    meter: &mut LoudnessMeter,
    segment: &mut AnalysisSegment,
    skip_frames: u64,
    take_frames: u64,
    cancel_token: Option<&DecodeCancelToken>,
) -> Result<(), AutomixError> {
    let sample_rate = decoder.info().sample_rate;
    let channels = decoder.info().channels.max(1);
    let window_size = (sample_rate as usize * WINDOW_SIZE_MS / 1000).max(1);
    let mut chunk = Vec::with_capacity(window_size * channels);
    let mut analyzer = SegmentAnalyzer::new(sample_rate, channels);
    let mut skip_remaining = skip_frames;
    let mut take_remaining = take_frames;

    while take_remaining > 0 {
        check_cancel(cancel_token)?;
        chunk.clear();
        let Some(sample_count) = decoder.decode_next_into(&mut chunk)? else {
            break;
        };
        if sample_count == 0 {
            continue;
        }
        let packet_frames = chunk.len() / channels;
        let Some(frame_range) =
            select_packet_frames(packet_frames, &mut skip_remaining, take_remaining)
        else {
            continue;
        };
        let sample_range = frame_range.start * channels..frame_range.end * channels;
        let selected_frames = (frame_range.end - frame_range.start) as u64;
        analyzer.process(&chunk[sample_range], meter, segment, cancel_token)?;
        take_remaining -= selected_frames;
    }

    Ok(())
}

fn select_packet_frames(
    packet_frames: usize,
    skip_remaining: &mut u64,
    take_remaining: u64,
) -> Option<Range<usize>> {
    let skipped = (*skip_remaining).min(packet_frames as u64) as usize;
    *skip_remaining -= skipped as u64;
    let available = packet_frames - skipped;
    let selected = take_remaining.min(available as u64) as usize;
    (selected > 0).then_some(skipped..skipped + selected)
}

fn frames_for_duration(duration: f64, sample_rate: u32) -> Option<u64> {
    if !duration.is_finite() || duration < 0.0 || sample_rate == 0 {
        return None;
    }
    let frames = duration * sample_rate as f64;
    (frames.is_finite() && frames <= u64::MAX as f64).then(|| frames.ceil() as u64)
}

fn frames_to_seconds(frames: u64, sample_rate: u32) -> f64 {
    frames as f64 / sample_rate.max(1) as f64
}

/// Whether a container-declared track duration may be used as the analysis
/// timeline.
///
/// An implausible value is discarded rather than clamped: clamping would report
/// a confident timeline the file never supported, whereas discarding falls back
/// to the duration actually measured from decoded head evidence.
fn is_plausible_duration(duration: &f64) -> bool {
    duration.is_finite() && *duration > 0.0 && *duration <= MAX_DECLARED_DURATION_SEC
}

fn check_cancel(cancel_token: Option<&DecodeCancelToken>) -> Result<(), AutomixError> {
    if cancel_token.is_some_and(DecodeCancelToken::is_cancelled) {
        Err(AutomixError::Canceled)
    } else {
        Ok(())
    }
}

fn finalize_analysis(
    options: &AutomixAnalysisOptions,
    duration: f64,
    sample_rate: u32,
    meter: &LoudnessMeter,
    head: &AnalysisSegment,
    tail: &AnalysisSegment,
    cancel: Option<&DecodeCancelToken>,
) -> Result<AutomixAnalysis, AutomixError> {
    check_cancel(cancel)?;
    let mode = options.mode;
    let effective_duration = if duration.is_finite() && duration > 0.0 {
        duration
    } else {
        head.start_time + head.frames_analyzed as f64 / sample_rate.max(1) as f64
    };
    let tail = (mode.includes_tail() && tail.frames_analyzed > 0).then_some(tail);
    let (fade_in, fade_out) = detect_silence_at(
        &head.envelope,
        tail.map_or(&[], |segment| segment.envelope.as_slice()),
        tail.map(|segment| segment.start_time),
        effective_duration,
        ENVELOPE_RATE,
        SILENCE_THRESHOLD_DB,
    );
    // Spectral flux is already differentiated. Only the RMS fallback needs
    // conversion to an onset curve, once, before local-mean removal.
    let fallback;
    let (tempo_values, tempo_rate, observation_offset) = if head.spectral_flux.len() >= 100 {
        (
            head.spectral_flux.as_slice(),
            sample_rate as f64 / spectral_hop_size(sample_rate) as f64,
            spectral_observation_offset_sec(sample_rate),
        )
    } else {
        fallback = head
            .envelope
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).max(0.0))
            .collect::<Vec<_>>();
        (fallback.as_slice(), ENVELOPE_RATE, 1.0 / ENVELOPE_RATE)
    };
    let channel_values = (head.spectral_flux_channels[0].len() == head.spectral_flux.len()
        && head.spectral_flux_channels[1].len() == head.spectral_flux.len()
        && head.spectral_flux_channels[2].len() == head.spectral_flux.len())
    .then_some(&head.spectral_flux_channels);
    let mut tempo = match channel_values {
        Some(channels) => tempo::estimate_with_channels(
            tempo_values,
            Some(channels),
            tempo_rate,
            observation_offset,
            cancel,
        )?,
        None => tempo::estimate(tempo_values, tempo_rate, observation_offset, cancel)?,
    };
    if let Some(grid) = &mut tempo.grid {
        grid.first_beat_sec += head.start_time;
    }
    let bpm = tempo.bpm();
    let bpm_confidence = tempo.confidence;
    let first_beat = tempo.grid.map(|grid| reported_first_beat(grid, head));
    let drop_pos = detect_drop(&head.envelope, ENVELOPE_RATE);
    let (vocal_in, vocal_out, vocal_last_in) =
        detect_vocals(head, tail, ENVELOPE_RATE, fade_in, fade_out);
    let cut_in = calculate_smart_cut_in(tempo, vocal_in.or(drop_pos), fade_in);
    let cut_out = if mode.includes_tail() {
        Some(calculate_smart_cut_out(
            tempo,
            vocal_out,
            fade_out,
            effective_duration,
        ))
    } else {
        None
    };
    let mix_center = cut_out.unwrap_or(fade_out).min(effective_duration);
    let mix_duration = bpm.map_or(20.0, |b| (240.0 / b * 8.0).clamp(15.0, 30.0));
    let mix_start = (mix_center - mix_duration / 2.0).max(0.0);
    let mix_end = (mix_center + mix_duration / 2.0).min(effective_duration);
    let energy_profile = build_energy_profile(head, tail, effective_duration);
    let loudness = finite_measurement(meter.integrated_loudness());
    let true_peak_dbtp = finite_measurement(meter.true_peak());

    check_cancel(cancel)?;
    Ok(AutomixAnalysis {
        version: ANALYSIS_VERSION,
        mode,
        duration: effective_duration,
        analyze_window: options.max_analyze_time_sec,
        bpm,
        bpm_confidence,
        first_beat_pos: first_beat,
        beat_grid_stability: tempo.grid.map(|grid| grid.stability),
        loudness,
        true_peak_dbtp,
        fade_in_pos: fade_in,
        fade_out_pos: if mode.includes_tail() {
            fade_out
        } else {
            effective_duration
        },
        cut_in_pos: Some(cut_in),
        cut_out_pos: cut_out,
        mix_center_pos: mix_center,
        mix_start_pos: mix_start,
        mix_end_pos: mix_end,
        energy_profile,
        drop_pos,
        vocal_in_pos: vocal_in,
        vocal_out_pos: tail.and(vocal_out),
        vocal_last_in_pos: tail.and(vocal_last_in),
        outro_energy_level: tail
            .and_then(|segment| calculate_outro_energy(&segment.envelope, ENVELOPE_RATE)),
    })
}

pub fn detect_silence(
    head: &[f32],
    tail: &[f32],
    duration: f64,
    rate: f64,
    db_thresh: f32,
) -> (f64, f64) {
    let tail_start = (!tail.is_empty()).then(|| (duration - tail.len() as f64 / rate).max(0.0));
    detect_silence_at(head, tail, tail_start, duration, rate, db_thresh)
}

fn reported_first_beat(grid: BeatGrid, head: &AnalysisSegment) -> f64 {
    let Some(initial_energy) = head.envelope.get(..2) else {
        return grid.first_beat_sec;
    };
    let silence = 10.0_f32.powf(SILENCE_THRESHOLD_DB / 20.0);
    // The first FFT has no predecessor. Use actual PCM energy to recognize
    // a short head transient followed by silence, without moving the fitted
    // grid used by cut snapping and residual statistics.
    if grid.first_beat_sec >= head.start_time + grid.period_sec - HEAD_BEAT_TOLERANCE_SEC
        && initial_energy[0] > silence
        && initial_energy[1] <= silence
    {
        head.start_time
    } else {
        grid.first_beat_sec
    }
}

fn detect_silence_at(
    head: &[f32],
    tail: &[f32],
    tail_start: Option<f64>,
    duration: f64,
    rate: f64,
    db_thresh: f32,
) -> (f64, f64) {
    let threshold = 10.0_f32.powf(db_thresh / 20.0);
    let fade_in = head
        .iter()
        .position(|value| *value > threshold)
        .map_or(0.0, |idx| idx as f64 / rate);

    let fade_out = if tail.is_empty() {
        head.iter()
            .rposition(|value| *value > threshold)
            .map_or(duration, |idx| (idx + 1) as f64 / rate)
            .min(duration)
    } else {
        let tail_start = tail_start.unwrap_or(0.0);
        tail.iter()
            .rposition(|value| *value > threshold)
            .map_or(duration, |idx| tail_start + (idx + 1) as f64 / rate)
            .min(duration)
    };

    (fade_in, fade_out)
}

fn detect_drop(envelope: &[f32], rate: f64) -> Option<f64> {
    let window_len = (2.0 * rate) as usize;
    let prev_len = (4.0 * rate) as usize;
    if envelope.len() < window_len + prev_len {
        return None;
    }

    let mut best_ratio = 0.0;
    let mut best_idx = 0usize;
    for idx in prev_len..envelope.len().saturating_sub(window_len) {
        let prev_avg = mean(&envelope[idx - prev_len..idx]);
        let next_avg = mean(&envelope[idx..idx + window_len]);
        if prev_avg > 0.001 {
            let ratio = next_avg / prev_avg;
            if ratio > best_ratio {
                best_ratio = ratio;
                best_idx = idx;
            }
        }
    }

    (best_ratio > 1.5).then_some(best_idx as f64 / rate)
}

fn detect_vocals(
    head: &AnalysisSegment,
    tail: Option<&AnalysisSegment>,
    rate: f64,
    fade_in: f64,
    fade_out: f64,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let is_vocal = |ratio: f32, env: f32| ratio > 0.4 && env > 0.02;
    let vocal_in = head
        .vocal_ratio
        .iter()
        .zip(head.envelope.iter())
        .enumerate()
        .skip((fade_in * rate) as usize)
        .find(|(_, (ratio, env))| is_vocal(**ratio, **env))
        .map(|(idx, _)| idx as f64 / rate);

    let (scan_env, scan_ratio, base_time) = tail
        .filter(|segment| !segment.envelope.is_empty())
        .map_or_else(
            || (head.envelope.as_slice(), head.vocal_ratio.as_slice(), 0.0),
            |segment| {
                (
                    segment.envelope.as_slice(),
                    segment.vocal_ratio.as_slice(),
                    segment.start_time,
                )
            },
        );
    let limit = ((fade_out - base_time) * rate).max(0.0) as usize;
    let vocal_out = scan_ratio
        .iter()
        .zip(scan_env.iter())
        .take(limit.min(scan_env.len()))
        .enumerate()
        .rfind(|(_, (ratio, env))| is_vocal(**ratio, **env))
        .map(|(idx, _)| base_time + idx as f64 / rate);

    let vocal_last_in = vocal_out.map(|value| (value - 5.0).max(fade_in));
    (vocal_in, vocal_out, vocal_last_in)
}

fn calculate_smart_cut_in(tempo: TempoEstimate, anchor: Option<f64>, fade_in: f64) -> f64 {
    let anchor = anchor.unwrap_or(fade_in);
    if let Some(grid) = tempo.usable_grid() {
        // These are transition durations, not claims about bar phase.
        for beats in [128.0, 64.0, 32.0] {
            let time = anchor - beats * grid.period_sec;
            if time > fade_in {
                return snap_to_beat(time, grid).max(fade_in);
            }
        }
    }
    fade_in
}

fn calculate_smart_cut_out(
    tempo: TempoEstimate,
    vocal_out: Option<f64>,
    fade_out: f64,
    duration: f64,
) -> f64 {
    let search_end = vocal_out.map_or(fade_out, |value| (value + 40.0).min(fade_out));
    if let Some(grid) = tempo.usable_grid() {
        let snapped = snap_to_beat(search_end, grid);
        if let Some(vocal_out) = vocal_out {
            if snapped < vocal_out + 2.0 {
                return snap_to_beat(vocal_out + 4.0, grid).min(duration);
            }
        }
        return snapped.min(duration);
    }
    search_end
}

fn snap_to_beat(time: f64, grid: BeatGrid) -> f64 {
    let units = ((time - grid.first_beat_sec) / grid.period_sec).round();
    (grid.first_beat_sec + units * grid.period_sec).max(0.0)
}

fn build_energy_profile(
    head: &AnalysisSegment,
    tail: Option<&AnalysisSegment>,
    duration: f64,
) -> Vec<f64> {
    let profile_rate = ENERGY_PROFILE_RATE;
    // The caller already discards an implausible declared duration, but this is
    // the allocation site, so it enforces the same ceiling itself rather than
    // trusting every present and future caller to have done so.
    let bounded_duration = duration.clamp(0.0, MAX_DECLARED_DURATION_SEC);
    let len = ((bounded_duration * profile_rate).ceil() as usize).max(1);
    let mut profile = vec![0.0; len];
    fill_energy_profile(
        &mut profile,
        &head.envelope,
        head.start_time,
        ENVELOPE_RATE,
        profile_rate,
    );
    if let Some(tail) = tail {
        fill_energy_profile(
            &mut profile,
            &tail.envelope,
            tail.start_time,
            ENVELOPE_RATE,
            profile_rate,
        );
    }
    profile
}

fn fill_energy_profile(
    profile: &mut [f64],
    envelope: &[f32],
    start_time: f64,
    env_rate: f64,
    profile_rate: f64,
) {
    for (idx, value) in envelope.iter().enumerate() {
        let profile_idx = ((start_time + idx as f64 / env_rate) * profile_rate) as usize;
        if let Some(slot) = profile.get_mut(profile_idx) {
            *slot = slot.max(f64::from(*value));
        }
    }
}

fn calculate_outro_energy(tail: &[f32], rate: f64) -> Option<f64> {
    if tail.is_empty() {
        return None;
    }
    let (_, local_out) = detect_silence(
        tail,
        &[],
        tail.len() as f64 / rate,
        rate,
        SILENCE_THRESHOLD_DB,
    );
    let end = (local_out * rate) as usize;
    let start = end.saturating_sub((10.0 * rate) as usize);
    if end <= start || end > tail.len() {
        return None;
    }
    let rms = mean_square(&tail[start..end]).sqrt();
    Some(if rms > 0.0 {
        f64::from(20.0 * rms.log10())
    } else {
        -70.0
    })
}

fn finite_measurement(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn mean(values: &[f32]) -> f32 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f32>() / values.len() as f32
    }
}

fn mean_square(values: &[f32]) -> f32 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().map(|value| value * value).sum::<f32>() / values.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn onset_bands_partition_resolved_bins_without_area_normalization() {
        for sample_rate in [8_000, 22_050, 44_100, 48_000, 96_000, 192_000] {
            let fft_size = spectral_frame_size(sample_rate);
            let bands = onset_bands(sample_rate, fft_size);
            assert!(bands.len() > 2);
            let last_bin = ((16_000.0_f64.min(f64::from(sample_rate) / 2.0) * fft_size as f64
                / f64::from(sample_rate))
            .round() as usize)
                .min(fft_size / 2);
            let mut summed_weights = vec![0.0; fft_size / 2];
            let mut centers = Vec::new();
            for band in &bands {
                assert!(band.start_bin + band.weights.len() <= last_bin);
                let center = band
                    .weights
                    .iter()
                    .position(|&weight| weight == 1.0)
                    .unwrap();
                centers.push(band.start_bin + center);
                // The discrete integral of a unit-height triangle equals
                // half its base width; wider bands must keep their area.
                let area = band.weights.iter().sum::<f32>();
                assert!((area - band.weights.len() as f32 / 2.0).abs() < 1e-5);
                for (offset, &weight) in band.weights.iter().enumerate() {
                    assert!((0.0..=1.0).contains(&weight));
                    summed_weights[band.start_bin + offset] += weight;
                }
            }
            for &weight in &summed_weights[centers[0]..=*centers.last().unwrap()] {
                assert!((weight - 1.0).abs() < 1e-6, "rate={sample_rate}, {weight}");
            }
        }
        assert!(onset_bands(0, spectral_frame_size(0)).is_empty());
        assert!(onset_bands(50, spectral_frame_size(50)).is_empty());
    }

    #[test]
    fn spectral_flux_clock_locates_an_isolated_impulse_across_sample_rates() {
        for sample_rate in [22_050, 44_100, 48_000, 96_000, 192_000] {
            let size = spectral_frame_size(sample_rate);
            let hop = spectral_hop_size(sample_rate);
            let impulse = 2 * size + hop / 3 + 37;
            let mut accumulator = SpectralFluxAccumulator::new(sample_rate);
            let flux: Vec<_> = (0..4 * size)
                .filter_map(|frame| accumulator.process(if frame == impulse { 1e-6 } else { 0.0 }))
                .collect();
            let peak = (1..flux.len() - 1)
                .max_by(|&left, &right| flux[left].total_cmp(&flux[right]))
                .unwrap();
            let (left, center, right) = (flux[peak - 1], flux[peak], flux[peak + 1]);
            let delta = 0.5 * (left - right) / (left - 2.0 * center + right);
            let observed = (peak as f64 + f64::from(delta)) * hop as f64 / f64::from(sample_rate)
                + spectral_observation_offset_sec(sample_rate);
            let expected = impulse as f64 / f64::from(sample_rate);
            assert!(
                (observed - expected).abs() < 0.002,
                "rate={sample_rate}, error={}",
                observed - expected
            );
        }
        assert_eq!(spectral_frame_size(0), 1024);
        assert_eq!(spectral_frame_size(u32::MAX), 8192);
    }

    /// Reference spectral-flux implementation using a full complex FFT — the
    /// formulation this module used before moving to `realfft`.
    ///
    /// Deliberately built on `rustfft` so it remains an independent oracle.
    fn reference_spectral_flux(samples: &[f32], sample_rate: u32, fft_size: usize) -> Vec<f32> {
        use rustfft::{num_complex::Complex32, FftPlanner};

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);
        let mut frame = vec![Complex32::new(0.0, 0.0); fft_size];
        let bands = onset_bands(sample_rate, fft_size);
        let mut previous = vec![0.0f32; bands.len()];
        let mut scratch = vec![0.0f32; fft_size];
        let mut pos = 0usize;
        let mut out = Vec::new();
        let hop = (sample_rate as usize / 200).clamp(1, 512);

        for &sample in samples {
            scratch[pos] = sample;
            pos += 1;
            if pos < fft_size {
                continue;
            }
            for i in 0..fft_size {
                let window = 0.5
                    - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (fft_size - 1) as f32).cos();
                frame[i] = Complex32::new(scratch[i] * window, 0.0);
            }
            fft.process(&mut frame);

            let mut flux = 0.0;
            for (band, previous) in bands.iter().zip(&mut previous) {
                let magnitude = band
                    .weights
                    .iter()
                    .enumerate()
                    .map(|(offset, weight)| frame[band.start_bin + offset].norm() * weight)
                    .sum::<f32>()
                    .ln_1p();
                flux += (magnitude - *previous).max(0.0);
                *previous = magnitude;
            }
            scratch.copy_within(hop..fft_size, 0);
            pos = fft_size - hop;
            out.push(flux / bands.len().max(1) as f32);
        }
        out
    }

    /// The real forward transform must reproduce the complex formulation's flux
    /// sequence. This matters beyond raw magnitudes: flux is differential and
    /// carries `previous_magnitudes` across hops, so a per-bin indexing mistake
    /// would accumulate rather than cancel.
    ///
    /// Accumulating band differences in f32 makes bit-exactness
    /// unrealistic; the tolerance is relative to the largest reference flux.
    #[test]
    fn cached_hann_window_is_bit_identical_to_evaluating_it_per_hop() {
        // The accumulator used to rebuild this window with 1,024 `cos()` calls on
        // every hop. Caching it is only a performance change if every cached
        // coefficient is the exact same `f32`, so compare bit patterns rather
        // than using a tolerance: a tolerance here would hide a real change in
        // the reported flux.
        for fft_size in [1024, 2048, 4096, 8192] {
            let cached = hann_window(fft_size);
            assert_eq!(cached.len(), fft_size);
            for (i, &coefficient) in cached.iter().enumerate() {
                let per_hop = 0.5
                    - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (fft_size - 1) as f32).cos();
                assert_eq!(
                    coefficient.to_bits(),
                    per_hop.to_bits(),
                    "size={fft_size}, window[{i}]: cached {coefficient} vs per-hop {per_hop}"
                );
            }
        }
    }

    #[test]
    fn spectral_flux_matches_complex_reference_formulation() {
        // Level and timbre both change over time so flux is genuinely non-zero:
        // a steady tone settles to ~0 flux after the first hop and would let an
        // indexing bug pass unnoticed.
        for (sample_rate, fft_size) in [
            (22_050, 1024),
            (44_100, 2048),
            (48_000, 2048),
            (96_000, 4096),
            (192_000, 8192),
        ] {
            let samples: Vec<f32> = (0..fft_size * 12)
                .map(|i| {
                    let t = i as f32 / sample_rate as f32;
                    let envelope = 0.2 + 0.8 * ((i / (fft_size * 3)) % 3) as f32 / 2.0;
                    let sweep = 220.0 + 400.0 * (i as f32 / (fft_size * 12) as f32);
                    envelope
                        * ((2.0 * std::f32::consts::PI * sweep * t).sin() * 0.6
                            + (2.0 * std::f32::consts::PI * 3.0 * sweep * t).sin() * 0.3)
                })
                .collect();

            let expected = reference_spectral_flux(&samples, sample_rate, fft_size);
            let mut accumulator = SpectralFluxAccumulator::new(sample_rate);
            let actual: Vec<f32> = samples
                .iter()
                .filter_map(|&sample| accumulator.process(sample))
                .collect();

            assert_eq!(actual.len(), expected.len());
            assert!(expected.len() >= 8, "fixture must produce several hops");

            let peak = expected.iter().fold(0.0f32, |acc, f| acc.max(f.abs()));
            assert!(peak > 0.0, "reference flux must not be all zeros");
            let tolerance = peak * 1e-4;

            for (hop, (got, want)) in actual.iter().zip(&expected).enumerate() {
                assert!(
                    (got - want).abs() <= tolerance,
                    "hop {hop}: {got} vs {want} (diff {:.3e} > tol {:.3e})",
                    (got - want).abs(),
                    tolerance
                );
            }
        }
    }

    static TEMP_AUDIO_COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempAudio {
        path: PathBuf,
    }

    impl TempAudio {
        fn wav(bytes: &[u8]) -> Self {
            let id = TEMP_AUDIO_COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "aec_automix_test_{}_{}.wav",
                std::process::id(),
                id
            ));
            let mut file = std::fs::File::create(&path).expect("create AutoMix fixture");
            file.write_all(bytes).expect("write AutoMix fixture");
            file.flush().expect("flush AutoMix fixture");
            Self { path }
        }

        fn path_string(&self) -> String {
            self.path
                .to_str()
                .expect("UTF-8 AutoMix fixture path")
                .to_owned()
        }
    }

    impl Drop for TempAudio {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn synth_wav<F: Fn(u64) -> f64>(sample_rate: u32, frames: u64, sample: F) -> Vec<u8> {
        let channels = 1_u16;
        let bits_per_sample = 16_u16;
        let block_align = channels * (bits_per_sample / 8);
        let byte_rate = sample_rate * u32::from(block_align);
        let data_len = frames as usize * usize::from(block_align);
        let mut bytes = Vec::with_capacity(44 + data_len);

        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
        for frame in 0..frames {
            let value = sample(frame).clamp(-1.0, 1.0);
            bytes.extend_from_slice(&((value * i16::MAX as f64).round() as i16).to_le_bytes());
        }

        bytes
    }

    fn analyze_tail_fixture(duration_secs: u64) -> AutomixAnalysis {
        let sample_rate = 8_000_u32;
        let frames = duration_secs * u64::from(sample_rate);
        let active_end = frames - u64::from(sample_rate) * 2 / 5;
        let wav = synth_wav(sample_rate, frames, |frame| {
            if frame < active_end {
                0.25
            } else {
                0.0
            }
        });
        let fixture = TempAudio::wav(&wav);

        analyze_automix(
            MediaLocation::local(fixture.path_string()),
            None,
            AutomixAnalysisOptions {
                mode: AutomixAnalysisMode::Full,
                max_analyze_time_sec: MIN_ANALYZE_TIME_SEC,
            },
        )
        .expect("analyze tail fixture")
    }

    fn pulse_train(rate: f64, bpm: f64, duration_seconds: f64) -> Vec<f32> {
        let len = (rate * duration_seconds).ceil() as usize;
        let period = rate * 60.0 / bpm;
        let mut values = vec![0.0; len];
        let mut position = 0.0_f64;
        while (position.round() as usize) < values.len() {
            values[position.round() as usize] = 1.0;
            position += period;
        }
        values
    }

    fn empty_analysis_with_flux(sample_rate: u32, flux: Vec<f32>) -> AutomixAnalysis {
        let meter = LoudnessMeter::new(2, sample_rate).unwrap();
        let head = AnalysisSegment {
            spectral_flux: flux,
            ..AnalysisSegment::default()
        };
        finalize_analysis(
            &AutomixAnalysisOptions {
                mode: AutomixAnalysisMode::Head,
                ..AutomixAnalysisOptions::default()
            },
            12.0,
            sample_rate,
            &meter,
            &head,
            &AnalysisSegment::default(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn window_plan_keeps_head_and_tail_disjoint_at_boundaries() {
        let window = 60;
        let cases = [
            (60, None),
            (61, Some(FrameWindow { start: 60, end: 61 })),
            (
                120,
                Some(FrameWindow {
                    start: 60,
                    end: 120,
                }),
            ),
            (
                121,
                Some(FrameWindow {
                    start: 61,
                    end: 121,
                }),
            ),
        ];

        for (track_frames, expected_tail) in cases {
            let plan =
                AnalysisWindowPlan::new(AutomixAnalysisMode::Full, Some(track_frames), window);
            assert_eq!(plan.head, FrameWindow { start: 0, end: 60 });
            assert_eq!(plan.tail, expected_tail);
            if let Some(tail) = plan.tail {
                assert!(plan.head.end <= tail.start);
            }
        }

        assert_eq!(
            AnalysisWindowPlan::new(AutomixAnalysisMode::Head, Some(121), window).tail,
            None
        );
        assert_eq!(
            AnalysisWindowPlan::new(AutomixAnalysisMode::Full, None, window),
            AnalysisWindowPlan {
                head: FrameWindow {
                    start: 0,
                    end: window,
                },
                tail: None,
            }
        );
    }

    #[test]
    fn packet_selection_slices_once_before_all_metric_consumers() {
        let sample_rate = 8_000;
        let channels = 2;
        let packet_frames = 1_400;
        let mut packet = Vec::with_capacity(packet_frames * channels);
        for frame in 0..packet_frames {
            let value = frame as f64 / packet_frames as f64 * 0.5;
            packet.extend_from_slice(&[value, value]);
        }
        let mut skip_remaining = 128;
        let frame_range = select_packet_frames(packet_frames, &mut skip_remaining, 1_024)
            .expect("packet should contain selected frames");

        assert_eq!(frame_range, 128..1_152);
        assert_eq!(skip_remaining, 0);
        let sample_range = frame_range.start * channels..frame_range.end * channels;
        let selected = &packet[sample_range];
        assert_eq!(selected.len(), 1_024 * channels);
        assert_eq!(selected[0], packet[128 * channels]);
        assert_eq!(selected[selected.len() - 1], packet[1_152 * channels - 1]);

        let mut meter = LoudnessMeter::new(channels, sample_rate).unwrap();
        let mut segment = AnalysisSegment::default();
        SegmentAnalyzer::new(sample_rate, channels)
            .process(selected, &mut meter, &mut segment, None)
            .unwrap();

        assert_eq!(meter.frames_processed(), 1_024);
        assert_eq!(segment.frames_analyzed, 1_024);
        assert_eq!(segment.envelope.len(), 6);
        assert_eq!(segment.low_envelope.len(), 6);
        assert_eq!(segment.vocal_ratio.len(), 6);
        assert_eq!(segment.spectral_flux.len(), 1);
    }

    #[test]
    fn segment_start_time_is_the_single_tail_timeline_origin() {
        let sample_rate = 8_000;
        let meter = LoudnessMeter::new(1, sample_rate).unwrap();
        let head = AnalysisSegment {
            frames_analyzed: 5 * u64::from(sample_rate),
            envelope: vec![0.25; 250],
            ..AnalysisSegment::default()
        };
        let tail = AnalysisSegment {
            start_time: 12.0,
            frames_analyzed: 2 * u64::from(sample_rate),
            envelope: vec![0.25; 100],
            ..AnalysisSegment::default()
        };

        let analysis = finalize_analysis(
            &AutomixAnalysisOptions {
                mode: AutomixAnalysisMode::Full,
                max_analyze_time_sec: 5.0,
            },
            20.0,
            sample_rate,
            &meter,
            &head,
            &tail,
            None,
        )
        .unwrap();

        assert!((analysis.fade_out_pos - 14.0).abs() < 0.001);
        assert!(analysis.energy_profile[120] > 0.0);
        assert_eq!(analysis.energy_profile[180], 0.0);
    }

    #[test]
    fn an_absurd_declared_duration_cannot_size_the_energy_profile() {
        // A container may declare any duration. Before the ceiling, this asked
        // for `1e12 * ENERGY_PROFILE_RATE` slots and aborted the process.
        let sample_rate = 8_000_u32;
        let head = AnalysisSegment {
            frames_analyzed: 5 * u64::from(sample_rate),
            envelope: vec![0.25; 250],
            ..AnalysisSegment::default()
        };

        let profile = build_energy_profile(&head, None, 1.0e12);

        assert_eq!(
            profile.len(),
            (MAX_DECLARED_DURATION_SEC * ENERGY_PROFILE_RATE) as usize
        );
        assert!(profile[0] > 0.0, "head evidence still lands at its origin");
    }

    #[test]
    fn an_implausible_declared_duration_falls_back_to_measured_head_evidence() {
        // Discarded rather than clamped: the analysis reports the five seconds
        // it actually decoded, not a confident 24-hour timeline it never saw.
        assert!(!is_plausible_duration(&(MAX_DECLARED_DURATION_SEC + 1.0)));
        assert!(!is_plausible_duration(&f64::INFINITY));
        assert!(!is_plausible_duration(&0.0));
        assert!(is_plausible_duration(&MAX_DECLARED_DURATION_SEC));

        let sample_rate = 8_000;
        let meter = LoudnessMeter::new(1, sample_rate).unwrap();
        let head = AnalysisSegment {
            frames_analyzed: 5 * u64::from(sample_rate),
            envelope: vec![0.25; 250],
            ..AnalysisSegment::default()
        };

        // `duration = 0.0` is what the caller passes once it rejects the
        // declared value, so `finalize_analysis` derives the timeline itself.
        let analysis = finalize_analysis(
            &AutomixAnalysisOptions {
                mode: AutomixAnalysisMode::Head,
                max_analyze_time_sec: 5.0,
            },
            0.0,
            sample_rate,
            &meter,
            &head,
            &AnalysisSegment::default(),
            None,
        )
        .unwrap();

        assert!((analysis.duration - 5.0).abs() < 0.001);
        assert_eq!(
            analysis.energy_profile.len(),
            (5.0 * ENERGY_PROFILE_RATE) as usize
        );
    }

    #[test]
    fn full_analysis_uses_absolute_tail_positions_at_window_boundaries() {
        for duration_secs in [6_u64, 10, 11] {
            let analysis = analyze_tail_fixture(duration_secs);
            let expected_end = duration_secs as f64 - 0.4;
            assert_eq!(
                analysis.bpm, None,
                "fixture must not exercise beat snapping"
            );
            for (name, actual) in [
                ("fade_out", analysis.fade_out_pos),
                ("cut_out", analysis.cut_out_pos.expect("Full mode cut-out")),
                ("mix_center", analysis.mix_center_pos),
            ] {
                assert!(
                    (actual - expected_end).abs() <= 0.05,
                    "{duration_secs}s {name}: expected {expected_end:.3}s, got {actual:.3}s"
                );
            }
        }
    }

    #[test]
    fn silence_detection_uses_head_and_tail_windows() {
        let mut head = vec![0.0; 50];
        head.extend(vec![0.02; 100]);
        let mut tail = vec![0.02; 100];
        tail.extend(vec![0.0; 50]);

        let (fade_in, fade_out) = detect_silence(&head, &tail, 20.0, 50.0, -48.0);

        assert!((fade_in - 1.0).abs() < 0.001);
        assert!((fade_out - 19.0).abs() < 0.001);
    }

    #[test]
    fn bpm_detection_returns_structured_low_confidence_for_flat_signal() {
        let values = vec![0.01; 160];
        let estimate = tempo::estimate(&values, 50.0, 0.0, None).unwrap();

        assert!(estimate.grid.is_none());
        assert!(estimate.confidence.is_none());
    }

    #[test]
    fn bpm_detection_rejects_invalid_rate_and_short_input() {
        let values = pulse_train(50.0, 120.0, 12.0);
        for rate in [0.0, -50.0, f64::NAN, f64::INFINITY] {
            assert!(tempo::estimate(&values, rate, 0.0, None)
                .unwrap()
                .grid
                .is_none());
        }
        assert!(tempo::estimate(&values[..99], 50.0, 0.0, None)
            .unwrap()
            .grid
            .is_none());
        assert!(tempo::estimate(&vec![f32::NAN; 500], 50.0, 0.0, None)
            .unwrap()
            .grid
            .is_none());
    }

    #[test]
    fn bpm_detection_finds_regular_pulse_train() {
        let mut values = vec![0.0; 300];
        for idx in (0..values.len()).step_by(25) {
            values[idx] = 1.0;
        }

        let estimate = tempo::estimate(&values, 50.0, 0.0, None).unwrap();

        assert!(estimate
            .bpm()
            .is_some_and(|value| (value - 120.0).abs() <= 0.05));
        assert!(estimate.usable_grid().is_some());
    }

    #[test]
    fn sub_frame_grid_regression_preserves_precision_and_unrounded_drift() {
        // This is an ODF-only test. The integration suite separately drives
        // the complete native-rate PCM -> decode -> FFT -> public DTO path.
        for rate in [50.0, 44_100.0 / 220.0, 200.0] {
            for bpm in [60.0, 127.3, 174.6, 200.0] {
                let values = pulse_train(rate, bpm, 60.0);
                let estimate = tempo::estimate(&values, rate, 0.0, None).unwrap();
                let detected = estimate.bpm().unwrap_or_else(|| {
                    panic!("expected {bpm} BPM to be detected at observation rate {rate}")
                });
                assert!(
                    (detected - bpm).abs() <= 0.05,
                    "expected {bpm} BPM at {rate} Hz, got {detected}"
                );
                let grid = estimate.grid.unwrap();
                let true_period = 60.0 / bpm;
                let phase_error = (grid.first_beat_sec + true_period / 2.0).rem_euclid(true_period)
                    - true_period / 2.0;
                assert!(
                    phase_error.abs() <= 0.010,
                    "{rate} Hz, {bpm} BPM phase: {phase_error}"
                );
                let last = (60.0 / true_period).floor();
                for period in [grid.period_sec, 60.0 / detected] {
                    assert!(
                        (phase_error + last * (period - true_period)).abs() <= 0.020,
                        "{rate} Hz, {bpm} BPM drift"
                    );
                }
            }
        }
    }

    #[test]
    fn finalize_analysis_uses_spectral_flux_cadence() {
        let sample_rate = 44_100;
        let flux_rate = sample_rate as f64 / spectral_hop_size(sample_rate) as f64;
        let analysis = empty_analysis_with_flux(sample_rate, pulse_train(flux_rate, 120.0, 12.0));

        assert!(
            analysis
                .bpm
                .is_some_and(|value| (value - 120.0).abs() <= 0.05),
            "spectral-flux BPM used the wrong cadence: {:?}",
            analysis.bpm
        );
    }

    #[test]
    fn cuts_snap_to_individual_beats_only_with_usable_evidence() {
        let grid = BeatGrid {
            period_sec: 0.5,
            first_beat_sec: 0.217,
            stability: 0.95,
        };
        let tempo = TempoEstimate {
            grid: Some(grid),
            confidence: Some(0.9),
        };
        assert!((snap_to_beat(1.3, grid) - 1.217).abs() < 1e-12);
        assert!((calculate_smart_cut_in(tempo, Some(70.1), 0.1) - 6.217).abs() < 1e-12);
        assert!((calculate_smart_cut_out(tempo, None, 10.1, 11.0) - 10.217).abs() < 1e-12);
        assert!(calculate_smart_cut_out(tempo, Some(10.0), 10.1, 11.0) <= 11.0);
        for (confidence, stability) in [(0.34, 0.95), (0.9, 0.79)] {
            let uncertain = TempoEstimate {
                grid: Some(BeatGrid { stability, ..grid }),
                confidence: Some(confidence),
            };
            assert_eq!(calculate_smart_cut_in(uncertain, Some(70.1), 0.1), 0.1);
            assert_eq!(calculate_smart_cut_out(uncertain, None, 10.1, 11.0), 10.1);
        }
    }

    #[test]
    fn boundary_beat_reporting_requires_isolated_pcm_energy_and_keeps_the_grid() {
        let grid = BeatGrid {
            period_sec: 0.5,
            first_beat_sec: 0.495,
            stability: 0.95,
        };
        let head = AnalysisSegment {
            envelope: vec![0.1, 0.0],
            ..AnalysisSegment::at(0.0)
        };
        assert_eq!(reported_first_beat(grid, &head), 0.0);
        let sustained = AnalysisSegment {
            envelope: vec![0.1, 0.1],
            ..AnalysisSegment::at(0.0)
        };
        assert_eq!(reported_first_beat(grid, &sustained), grid.first_beat_sec);
        assert_eq!(snap_to_beat(0.27, grid), 0.495);
    }

    #[test]
    fn boundary_beat_reporting_preserves_origin_and_requires_both_energy_windows() {
        let silence = 10.0_f32.powf(SILENCE_THRESHOLD_DB / 20.0);
        for origin in [0.0, 17.25] {
            let grid = BeatGrid {
                period_sec: 0.5,
                first_beat_sec: origin + 0.495,
                stability: 0.95,
            };
            for envelope in [
                vec![],
                vec![0.1],
                vec![0.0, 0.0],
                vec![silence, 0.0],
                vec![0.1, 0.1],
            ] {
                let head = AnalysisSegment {
                    envelope,
                    ..AnalysisSegment::at(origin)
                };
                assert_eq!(reported_first_beat(grid, &head), grid.first_beat_sec);
            }
            let head = AnalysisSegment {
                envelope: vec![0.1, silence],
                ..AnalysisSegment::at(origin)
            };
            assert_eq!(reported_first_beat(grid, &head), origin);
            let outside_boundary = BeatGrid {
                first_beat_sec: origin + 0.48,
                ..grid
            };
            assert_eq!(
                reported_first_beat(outside_boundary, &head),
                outside_boundary.first_beat_sec
            );
        }
    }

    #[test]
    fn serialized_v4_analysis_pins_null_grid_and_omits_key_placeholders() {
        let analysis = empty_analysis_with_flux(48_000, Vec::new());
        let json = serde_json::to_value(&analysis).expect("analysis should serialize");

        assert_eq!(json["version"], 4);
        assert_eq!(json["mode"], "head");
        assert!(json["beat_grid_stability"].is_null());
        let expected_keys: std::collections::BTreeSet<_> = [
            "version",
            "mode",
            "duration",
            "analyze_window",
            "bpm",
            "bpm_confidence",
            "first_beat_pos",
            "beat_grid_stability",
            "loudness",
            "true_peak_dbtp",
            "fade_in_pos",
            "fade_out_pos",
            "cut_in_pos",
            "cut_out_pos",
            "mix_center_pos",
            "mix_start_pos",
            "mix_end_pos",
            "energy_profile",
            "drop_pos",
            "vocal_in_pos",
            "vocal_out_pos",
            "vocal_last_in_pos",
            "outro_energy_level",
        ]
        .into_iter()
        .collect();
        let actual_keys: std::collections::BTreeSet<_> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(actual_keys, expected_keys);
        for field in [
            "key_status",
            "key_root",
            "key_pitch_class",
            "key_mode",
            "key_confidence",
            "camelot_key",
        ] {
            assert!(json.get(field).is_none(), "{field} must not be reserved");
        }
    }

    #[test]
    fn analysis_reports_cancellation_as_a_typed_variant() {
        let token = DecodeCancelToken::new();
        token.cancel();

        let error = analyze_automix_with_cancel(
            MediaLocation::local("unused.wav"),
            None,
            AutomixAnalysisOptions::default(),
            Some(token),
        )
        .expect_err("pre-canceled analysis must stop before opening the source");

        assert!(matches!(error, AutomixError::Canceled));
    }

    #[test]
    fn analysis_preserves_decoder_error_source() {
        let id = TEMP_AUDIO_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aec_automix_missing_{}_{}.wav",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        let error = analyze_automix(
            MediaLocation::local(path),
            None,
            AutomixAnalysisOptions::default(),
        )
        .expect_err("missing source must fail");

        assert!(matches!(
            &error,
            AutomixError::Decoder(DecoderError::FileOpen(_))
        ));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn analysis_preserves_loudness_process_error_source() {
        let mut meter = LoudnessMeter::new(2, 48_000).unwrap();
        let mut segment = AnalysisSegment::default();
        let error = SegmentAnalyzer::new(48_000, 2)
            .process(&[0.25, -0.25, 0.5], &mut meter, &mut segment, None)
            .expect_err("incomplete interleaved frame must fail");

        assert!(matches!(
            error,
            AutomixError::Loudness(ProcessError::InvalidBlock(
                crate::audio_block::AudioBlockError::IncompleteFrame {
                    samples: 3,
                    channels: 2,
                }
            ))
        ));
        assert_eq!(meter.frames_processed(), 0);
        assert_eq!(segment.frames_analyzed, 0);
    }
}
