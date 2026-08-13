//! AutoMix offline audio analysis.
//!
//! This module is intentionally pure/backend-side. It decodes bounded head/tail
//! windows off the realtime callback path and returns a stable DTO for later
//! transition planning.

use crate::decoder::{
    DecodeCancelToken, DecoderError, HttpCredentials, MediaLocation, StreamingDecoder,
};
use crate::processor::{LoudnessMeter, ProcessError};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use thiserror::Error;

const ANALYSIS_VERSION: u32 = 3;
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
const MIN_TEMPO_BPM: f64 = 55.0;
const MAX_TEMPO_BPM: f64 = 200.0;
// Rounded observation grids weaken a true non-integer beat period while an
// integer multiple can align exactly. Sixty percent retains the fundamental
// for those grids without accepting the low background autocorrelation floor.
const HARMONIC_PEAK_RATIO: f32 = 0.6;
const FFT_SIZE: usize = 1024;
const SPECTRAL_HOP_SIZE: usize = FFT_SIZE / 2;

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
    pub duration: f64,
    /// Duration of the analyzed head section in seconds.
    pub analyze_window: f64,
    /// Estimated tempo in BPM, when the beat tracker converged.
    pub bpm: Option<f64>,
    /// Confidence of the BPM estimate.
    pub bpm_confidence: Option<f64>,
    /// Position of the first detected beat in seconds.
    pub first_beat_pos: Option<f64>,
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
    /// Time budget cap for analysis in seconds; non-finite values reset to
    /// the built-in default.
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

struct SpectralFluxAccumulator {
    /// Windowed time-domain frame. `realfft` mutates its input, so this doubles
    /// as transform scratch.
    frame: Vec<f32>,
    /// Half-spectrum: `FFT_SIZE / 2 + 1` bins. Only the first `FFT_SIZE / 2`
    /// are read, matching the original bin selection.
    spectrum: Vec<Complex<f32>>,
    /// Workspace for `process_with_scratch`. The plain `process` allocates on
    /// every call, which this hop loop runs once per 512 samples of the track.
    fft_scratch: Vec<Complex<f32>>,
    previous_magnitudes: Vec<f32>,
    scratch: Vec<f32>,
    /// Precomputed Hann window.
    ///
    /// The window is a constant of `FFT_SIZE`, but it used to be rebuilt with
    /// 1,024 `cos()` calls on every hop — which measured more expensive than the
    /// transform it feeds. Caching it cut this accumulator by 72.5% per hop
    /// (7,840 ns to 2,158 ns) while staying bit-identical, because each
    /// coefficient is the same `f32` either way. `SpectrumAnalyzer` already
    /// stored its window this way.
    window: Vec<f32>,
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
            spectral: SpectralFluxAccumulator::new(),
        }
    }

    fn process(
        &mut self,
        samples: &[f64],
        meter: &mut LoudnessMeter,
        segment: &mut AnalysisSegment,
    ) -> Result<(), ProcessError> {
        meter.process(samples)?;

        for frame in samples.chunks_exact(self.channels) {
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
            }
            segment.frames_analyzed += 1;
        }
        Ok(())
    }
}

/// The periodic-denominator Hann window used by the spectral-flux accumulator.
///
/// The expression is kept character-for-character identical to the one this
/// replaced (`0.5 - 0.5 * cos(2*PI*i / (FFT_SIZE - 1))`, evaluated in `f32`),
/// so each cached coefficient is the same bit pattern the inline version
/// produced and the flux output is unchanged. `legacy_spectral_flux` in the
/// tests still evaluates the window inline and is the independent check on that.
fn hann_window() -> Vec<f32> {
    (0..FFT_SIZE)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
        .collect()
}

impl SpectralFluxAccumulator {
    fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        Self {
            frame: vec![0.0; FFT_SIZE],
            spectrum: vec![Complex::new(0.0, 0.0); fft.complex_len()],
            fft_scratch: vec![Complex::new(0.0, 0.0); fft.get_scratch_len()],
            previous_magnitudes: vec![0.0; FFT_SIZE / 2],
            scratch: vec![0.0; FFT_SIZE],
            window: hann_window(),
            pos: 0,
            fft,
        }
    }

    fn process(&mut self, sample: f32) -> Option<f32> {
        self.scratch[self.pos] = sample;
        self.pos += 1;
        if self.pos < FFT_SIZE {
            return None;
        }

        for i in 0..FFT_SIZE {
            self.frame[i] = self.scratch[i] * self.window[i];
        }
        // Lengths are fixed at construction to exactly what the plan requires,
        // so these checks cannot fail; a violated invariant would be a bug
        // here. Reuse the previous spectrum rather than panicking mid-analysis.
        debug_assert_eq!(self.frame.len(), FFT_SIZE);
        debug_assert_eq!(self.spectrum.len(), self.fft.complex_len());
        let _ = self.fft.process_with_scratch(
            &mut self.frame,
            &mut self.spectrum,
            &mut self.fft_scratch,
        );

        let mut flux = 0.0;
        for i in 0..FFT_SIZE / 2 {
            let mag = self.spectrum[i].norm();
            flux += (mag - self.previous_magnitudes[i]).max(0.0);
            self.previous_magnitudes[i] = mag;
        }

        self.scratch.copy_within(SPECTRAL_HOP_SIZE..FFT_SIZE, 0);
        self.pos = SPECTRAL_HOP_SIZE;
        Some(flux / (FFT_SIZE / 2) as f32)
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

    Ok(finalize_analysis(
        options.mode,
        options.max_analyze_time_sec,
        duration,
        sample_rate,
        &meter,
        &head,
        &tail,
    ))
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
        analyzer.process(&chunk[sample_range], meter, segment)?;
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
    mode: AutomixAnalysisMode,
    analyze_window: f64,
    duration: f64,
    sample_rate: u32,
    meter: &LoudnessMeter,
    head: &AnalysisSegment,
    tail: &AnalysisSegment,
) -> AutomixAnalysis {
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
    let (tempo_values, tempo_rate) = if head.spectral_flux.len() >= 100 {
        (
            head.spectral_flux.as_slice(),
            sample_rate as f64 / SPECTRAL_HOP_SIZE as f64,
        )
    } else {
        (head.envelope.as_slice(), ENVELOPE_RATE)
    };
    let (bpm, bpm_confidence, first_beat) = detect_bpm(tempo_values, tempo_rate);
    let drop_pos = detect_drop(&head.envelope, ENVELOPE_RATE);
    let (vocal_in, vocal_out, vocal_last_in) =
        detect_vocals(head, tail, ENVELOPE_RATE, fade_in, fade_out);
    let cut_in = calculate_smart_cut_in(
        bpm,
        first_beat,
        bpm_confidence,
        vocal_in.or(drop_pos),
        fade_in,
    );
    let cut_out = if mode.includes_tail() {
        Some(calculate_smart_cut_out(
            bpm,
            first_beat,
            bpm_confidence,
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

    AutomixAnalysis {
        version: ANALYSIS_VERSION,
        mode,
        duration: effective_duration,
        analyze_window,
        bpm,
        bpm_confidence,
        first_beat_pos: first_beat,
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
    }
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

pub fn detect_bpm(values: &[f32], rate: f64) -> (Option<f64>, Option<f64>, Option<f64>) {
    if values.len() < 110 || !rate.is_finite() || rate <= 0.0 {
        return (None, None, None);
    }

    let flux: Vec<f32> = values
        .windows(2)
        .map(|window| (window[1] - window[0]).max(0.0))
        .collect();
    let flux_energy = flux.iter().map(|value| value * value).sum::<f32>();
    if flux_energy <= 1.0e-6 {
        return (None, None, None);
    }

    let min_lag = (rate * 60.0 / MAX_TEMPO_BPM).floor().max(1.0) as usize;
    let max_lag = ((rate * 60.0 / MIN_TEMPO_BPM).ceil() as usize).min(flux.len().saturating_sub(1));
    if min_lag > max_lag {
        return (None, None, None);
    }

    let mut correlations = Vec::with_capacity(max_lag - min_lag + 1);
    let mut corr_sum = 0.0_f32;

    for lag in min_lag..=max_lag {
        let mut sum = 0.0;
        let mut left_energy = 0.0;
        let mut right_energy = 0.0;
        for idx in 0..flux.len() - lag {
            let left = flux[idx];
            let right = flux[idx + lag];
            sum += left * right;
            left_energy += left * left;
            right_energy += right * right;
        }
        let denominator = (left_energy * right_energy).sqrt();
        let normalized = if denominator > 0.0 {
            sum / denominator
        } else {
            0.0
        };
        corr_sum += normalized;
        correlations.push((lag, normalized));
    }

    let strongest_corr = correlations
        .iter()
        .map(|(_, correlation)| *correlation)
        .max_by(f32::total_cmp)
        .unwrap_or(0.0);
    if strongest_corr <= 1.0e-5 {
        return (None, None, None);
    }

    // A periodic onset train produces equally valid autocorrelation peaks at
    // integer multiples of its fundamental period. Prefer the shortest peak
    // that is effectively as strong as the global maximum so 120/180 BPM are
    // not folded to 60 BPM solely because a later multiple aligns exactly.
    let peak_floor = strongest_corr * HARMONIC_PEAK_RATIO;
    let (best_lag, best_corr) = correlations
        .iter()
        .enumerate()
        .find(|(index, (_, correlation))| {
            let previous = index
                .checked_sub(1)
                .and_then(|previous| correlations.get(previous))
                .map_or(f32::NEG_INFINITY, |(_, value)| *value);
            let next = correlations
                .get(index + 1)
                .map_or(f32::NEG_INFINITY, |(_, value)| *value);
            *correlation >= peak_floor && *correlation >= previous && *correlation >= next
        })
        .map(|(_, value)| *value)
        .unwrap_or_else(|| {
            correlations
                .iter()
                .copied()
                .max_by(|left, right| left.1.total_cmp(&right.1))
                .unwrap_or((0, 0.0))
        });

    let average_corr = corr_sum / correlations.len() as f32;
    let confidence = ((best_corr - average_corr).max(0.0) / best_corr.max(1.0e-6)).clamp(0.0, 1.0);
    if confidence < 0.12 {
        return (None, Some(confidence as f64), None);
    }

    let first_beat = (0..best_lag)
        .max_by(|a, b| {
            phase_energy(&flux, *a, best_lag)
                .partial_cmp(&phase_energy(&flux, *b, best_lag))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|phase| phase as f64 / rate);

    (
        Some(60.0 / (best_lag as f64 / rate)),
        Some(confidence as f64),
        first_beat,
    )
}

fn phase_energy(flux: &[f32], phase: usize, lag: usize) -> f32 {
    let mut energy = 0.0;
    let mut idx = phase;
    while idx < flux.len() {
        energy += flux[idx];
        idx += lag;
    }
    energy
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

fn calculate_smart_cut_in(
    bpm: Option<f64>,
    first_beat: Option<f64>,
    confidence: Option<f64>,
    anchor: Option<f64>,
    fade_in: f64,
) -> f64 {
    let anchor = anchor.unwrap_or(fade_in);
    if let (Some(bpm), Some(first_beat)) = (bpm, first_beat) {
        if confidence.unwrap_or(0.0) > 0.4 {
            let sec_per_bar = 240.0 / bpm;
            for bars in [32.0_f64, 16.0, 8.0] {
                let time = anchor - bars * sec_per_bar;
                if time > fade_in {
                    return snap_time(time, bpm, first_beat, 4.0);
                }
            }
        }
    }
    fade_in
}

fn calculate_smart_cut_out(
    bpm: Option<f64>,
    first_beat: Option<f64>,
    confidence: Option<f64>,
    vocal_out: Option<f64>,
    fade_out: f64,
    duration: f64,
) -> f64 {
    let search_end = vocal_out.map_or(fade_out, |value| (value + 40.0).min(fade_out));
    if let (Some(bpm), Some(first_beat)) = (bpm, first_beat) {
        if confidence.unwrap_or(0.0) > 0.4 {
            let snapped = snap_time(search_end, bpm, first_beat, 4.0);
            if let Some(vocal_out) = vocal_out {
                if snapped < vocal_out + 2.0 {
                    return snap_time(vocal_out + 4.0, bpm, first_beat, 4.0).min(duration);
                }
            }
            return snapped.min(duration);
        }
    }
    search_end
}

fn snap_time(time: f64, bpm: f64, first_beat: f64, grid: f64) -> f64 {
    let grid_sec = 60.0 / bpm * grid;
    if grid_sec <= 0.0 {
        return time;
    }
    let units = ((time - first_beat) / grid_sec).round();
    (first_beat + units * grid_sec).max(0.0)
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

    /// Reference spectral-flux implementation using a full complex FFT — the
    /// formulation this module used before moving to `realfft`.
    ///
    /// Deliberately built on `rustfft` so it remains an independent oracle.
    fn legacy_spectral_flux(samples: &[f32]) -> Vec<f32> {
        use rustfft::{num_complex::Complex32, FftPlanner};

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let mut frame = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
        let mut previous = vec![0.0f32; FFT_SIZE / 2];
        let mut scratch = vec![0.0f32; FFT_SIZE];
        let mut pos = 0usize;
        let mut out = Vec::new();

        for &sample in samples {
            scratch[pos] = sample;
            pos += 1;
            if pos < FFT_SIZE {
                continue;
            }
            for i in 0..FFT_SIZE {
                let window = 0.5
                    - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos();
                frame[i] = Complex32::new(scratch[i] * window, 0.0);
            }
            fft.process(&mut frame);

            let mut flux = 0.0;
            for i in 0..FFT_SIZE / 2 {
                let mag = frame[i].norm();
                flux += (mag - previous[i]).max(0.0);
                previous[i] = mag;
            }
            scratch.copy_within(SPECTRAL_HOP_SIZE..FFT_SIZE, 0);
            pos = SPECTRAL_HOP_SIZE;
            out.push(flux / (FFT_SIZE / 2) as f32);
        }
        out
    }

    /// The real forward transform must reproduce the complex formulation's flux
    /// sequence. This matters beyond raw magnitudes: flux is differential and
    /// carries `previous_magnitudes` across hops, so a per-bin indexing mistake
    /// would accumulate rather than cancel.
    ///
    /// `f32` with 512 accumulated bin differences per hop makes bit-exactness
    /// unrealistic; the tolerance is relative to the largest reference flux.
    #[test]
    fn cached_hann_window_is_bit_identical_to_evaluating_it_per_hop() {
        // The accumulator used to rebuild this window with 1,024 `cos()` calls on
        // every hop. Caching it is only a performance change if every cached
        // coefficient is the exact same `f32`, so compare bit patterns rather
        // than using a tolerance: a tolerance here would hide a real change in
        // the reported flux.
        let cached = hann_window();
        assert_eq!(cached.len(), FFT_SIZE);
        for (i, &coefficient) in cached.iter().enumerate() {
            let per_hop =
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos();
            assert_eq!(
                coefficient.to_bits(),
                per_hop.to_bits(),
                "window[{i}]: cached {coefficient} vs per-hop {per_hop}"
            );
        }
    }

    #[test]
    fn spectral_flux_matches_complex_reference_formulation() {
        // Level and timbre both change over time so flux is genuinely non-zero:
        // a steady tone settles to ~0 flux after the first hop and would let an
        // indexing bug pass unnoticed.
        let samples: Vec<f32> = (0..FFT_SIZE * 12)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                let envelope = 0.2 + 0.8 * ((i / (FFT_SIZE * 3)) % 3) as f32 / 2.0;
                let sweep = 220.0 + 400.0 * (i as f32 / (FFT_SIZE * 12) as f32);
                envelope
                    * ((2.0 * std::f32::consts::PI * sweep * t).sin() * 0.6
                        + (2.0 * std::f32::consts::PI * 3.0 * sweep * t).sin() * 0.3)
            })
            .collect();

        let expected = legacy_spectral_flux(&samples);
        let mut accumulator = SpectralFluxAccumulator::new();
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
            AutomixAnalysisMode::Head,
            DEFAULT_MAX_ANALYZE_TIME_SEC,
            12.0,
            sample_rate,
            &meter,
            &head,
            &AnalysisSegment::default(),
        )
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
            .process(selected, &mut meter, &mut segment)
            .unwrap();

        assert_eq!(meter.samples_processed(), 1_024);
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
            AutomixAnalysisMode::Full,
            5.0,
            20.0,
            sample_rate,
            &meter,
            &head,
            &tail,
        );

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
            AutomixAnalysisMode::Head,
            5.0,
            0.0,
            sample_rate,
            &meter,
            &head,
            &AnalysisSegment::default(),
        );

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
        let (bpm, confidence, first_beat) = detect_bpm(&values, 50.0);

        assert!(bpm.is_none());
        assert!(confidence.is_none());
        assert!(first_beat.is_none());
    }

    #[test]
    fn bpm_detection_rejects_invalid_rate_and_short_input() {
        let values = pulse_train(50.0, 120.0, 12.0);
        for rate in [0.0, -50.0, f64::NAN, f64::INFINITY] {
            assert_eq!(detect_bpm(&values, rate), (None, None, None));
        }
        assert_eq!(detect_bpm(&values[..109], 50.0), (None, None, None));
    }

    #[test]
    fn bpm_detection_finds_regular_pulse_train() {
        let mut values = vec![0.0; 300];
        for idx in (0..values.len()).step_by(25) {
            values[idx] = 1.0;
        }

        let (bpm, confidence, first_beat) = detect_bpm(&values, 50.0);

        assert!(bpm.is_some_and(|value| (value - 120.0).abs() < 0.1));
        assert!(confidence.is_some_and(|value| value > 0.12));
        assert!(first_beat.is_some());
    }

    #[test]
    fn bpm_detection_uses_rate_derived_lag_bounds() {
        for rate in [50.0, 44_100.0 / 512.0, 48_000.0 / 512.0] {
            for bpm in [60.0, 120.0, 180.0] {
                let values = pulse_train(rate, bpm, 12.0);
                let (detected, _, _) = detect_bpm(&values, rate);
                let detected = detected.unwrap_or_else(|| {
                    panic!("expected {bpm} BPM to be detected at observation rate {rate}")
                });
                let relative_error = (detected - bpm).abs() / bpm;
                assert!(
                    relative_error <= 0.02,
                    "expected {bpm} BPM at {rate} Hz, got {detected} ({relative_error:.3} relative error)"
                );
            }
        }
    }

    #[test]
    fn finalize_analysis_uses_spectral_flux_cadence() {
        let sample_rate = 44_100;
        let flux_rate = sample_rate as f64 / 512.0;
        let analysis = empty_analysis_with_flux(sample_rate, pulse_train(flux_rate, 120.0, 12.0));

        assert!(
            analysis
                .bpm
                .is_some_and(|value| (value - 120.0).abs() / 120.0 <= 0.02),
            "spectral-flux BPM used the wrong cadence: {:?}",
            analysis.bpm
        );
    }

    #[test]
    fn serialized_analysis_omits_unimplemented_key_placeholders() {
        let analysis = empty_analysis_with_flux(48_000, Vec::new());
        let json = serde_json::to_value(&analysis).expect("analysis should serialize");

        assert_eq!(json["version"], 3);
        for field in [
            "key_status",
            "key_root",
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
            .process(&[0.25, -0.25, 0.5], &mut meter, &mut segment)
            .map_err(AutomixError::from)
            .expect_err("incomplete interleaved frame must fail");

        assert!(matches!(
            error,
            AutomixError::Loudness(ProcessError::InvalidBlock(
                crate::processor::traits::AudioBlockError::IncompleteFrame {
                    samples: 3,
                    channels: 2,
                }
            ))
        ));
        assert_eq!(meter.samples_processed(), 0);
        assert_eq!(segment.frames_analyzed, 0);
    }
}
