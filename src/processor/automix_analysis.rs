//! AutoMix offline audio analysis.
//!
//! This module is intentionally pure/backend-side. It decodes bounded head/tail
//! windows off the realtime callback path and returns a stable DTO for later
//! transition planning.

use crate::decoder::{DecodeCancelToken, HttpCredentials, StreamingDecoder};
use crate::processor::LoudnessMeter;
use rustfft::{num_complex::Complex32, FftPlanner};
use serde::{Deserialize, Serialize};

const ANALYSIS_VERSION: u32 = 1;
const DEFAULT_MAX_ANALYZE_TIME_SEC: f64 = 60.0;
const MIN_ANALYZE_TIME_SEC: f64 = 5.0;
const MAX_ANALYZE_TIME_SEC: f64 = 300.0;
const ENVELOPE_RATE: f64 = 50.0;
const WINDOW_SIZE_MS: usize = 20;
const SILENCE_THRESHOLD_DB: f32 = -48.0;
const BPM_MIN_LAG: usize = 15;
const BPM_MAX_LAG: usize = 55;
const FFT_SIZE: usize = 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AutomixAnalysisMode {
    Head,
    #[default]
    Full,
}

impl AutomixAnalysisMode {
    pub fn includes_tail(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AutomixAnalysis {
    pub version: u32,
    pub mode: AutomixAnalysisMode,
    pub duration: f64,
    pub analyze_window: f64,
    pub bpm: Option<f64>,
    pub bpm_confidence: Option<f64>,
    pub first_beat_pos: Option<f64>,
    pub loudness: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub fade_in_pos: f64,
    pub fade_out_pos: f64,
    pub cut_in_pos: Option<f64>,
    pub cut_out_pos: Option<f64>,
    pub mix_center_pos: f64,
    pub mix_start_pos: f64,
    pub mix_end_pos: f64,
    pub energy_profile: Vec<f64>,
    pub drop_pos: Option<f64>,
    pub vocal_in_pos: Option<f64>,
    pub vocal_out_pos: Option<f64>,
    pub vocal_last_in_pos: Option<f64>,
    pub outro_energy_level: Option<f64>,
    pub key_root: Option<i32>,
    pub key_mode: Option<i32>,
    pub key_confidence: Option<f64>,
    pub camelot_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AutomixAnalysisOptions {
    pub mode: AutomixAnalysisMode,
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

#[derive(Default)]
struct AnalysisSegment {
    envelope: Vec<f32>,
    low_envelope: Vec<f32>,
    vocal_ratio: Vec<f32>,
    spectral_flux: Vec<f32>,
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
    frame: Vec<Complex32>,
    previous_magnitudes: Vec<f32>,
    scratch: Vec<f32>,
    pos: usize,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
}

impl SpectralFluxAccumulator {
    fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        Self {
            frame: vec![Complex32::new(0.0, 0.0); FFT_SIZE],
            previous_magnitudes: vec![0.0; FFT_SIZE / 2],
            scratch: vec![0.0; FFT_SIZE],
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
            let window =
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos();
            self.frame[i] = Complex32::new(self.scratch[i] * window, 0.0);
        }
        self.fft.process(&mut self.frame);

        let mut flux = 0.0;
        for i in 0..FFT_SIZE / 2 {
            let mag = self.frame[i].norm();
            flux += (mag - self.previous_magnitudes[i]).max(0.0);
            self.previous_magnitudes[i] = mag;
        }

        self.scratch.copy_within(FFT_SIZE / 2..FFT_SIZE, 0);
        self.pos = FFT_SIZE / 2;
        Some(flux / (FFT_SIZE / 2) as f32)
    }
}

pub fn analyze_automix(
    path: String,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
) -> Result<AutomixAnalysis, String> {
    analyze_automix_with_cancel(path, credentials, options, None)
}

pub fn analyze_automix_with_cancel(
    path: String,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<AutomixAnalysis, String> {
    let options = options.normalized();
    check_cancel(cancel_token.as_ref())?;
    let mut decoder = StreamingDecoder::open_with_credentials_and_cancel(
        &path,
        credentials.as_ref(),
        cancel_token.clone(),
    )
    .map_err(|e| format!("Failed to open file for AutoMix analysis: {}", e))?;

    let sample_rate = decoder.info.sample_rate;
    let channels = decoder.info.channels.max(1);
    let duration = decoder.info.duration_secs.unwrap_or(0.0);
    let mut meter = LoudnessMeter::new(channels, sample_rate);
    let mut head = AnalysisSegment::default();
    let mut tail = AnalysisSegment::default();

    decode_segment(
        &mut decoder,
        &mut meter,
        &mut head,
        options.max_analyze_time_sec,
        cancel_token.as_ref(),
    )?;

    if options.mode.includes_tail() && duration > options.max_analyze_time_sec * 2.0 {
        check_cancel(cancel_token.as_ref())?;
        decoder
            .seek((duration - options.max_analyze_time_sec).max(0.0))
            .map_err(|e| format!("Failed to seek tail for AutoMix analysis: {}", e))?;
        decode_segment(
            &mut decoder,
            &mut meter,
            &mut tail,
            options.max_analyze_time_sec,
            cancel_token.as_ref(),
        )?;
    }

    Ok(finalize_analysis(
        options.mode,
        options.max_analyze_time_sec,
        duration,
        &meter,
        &head,
        &tail,
    ))
}

fn decode_segment(
    decoder: &mut StreamingDecoder,
    meter: &mut LoudnessMeter,
    segment: &mut AnalysisSegment,
    max_time_sec: f64,
    cancel_token: Option<&DecodeCancelToken>,
) -> Result<(), String> {
    let sample_rate = decoder.info.sample_rate;
    let channels = decoder.info.channels.max(1);
    let max_frames = (sample_rate as f64 * max_time_sec).ceil() as usize;
    let window_size = (sample_rate as usize * WINDOW_SIZE_MS / 1000).max(1);
    let mut frames_processed = 0usize;
    let mut chunk = Vec::with_capacity(window_size * channels);
    let mut env_acc = EnvelopeAccumulator::new(window_size);
    let mut low_acc = EnvelopeAccumulator::new(window_size);
    let mut vocal_acc = EnvelopeAccumulator::new(window_size);
    let mut low_filter = FirstOrderFilter::new(sample_rate, 150.0, false);
    let mut vocal_lowpass = FirstOrderFilter::new(sample_rate, 3_000.0, false);
    let mut vocal_highpass = FirstOrderFilter::new(sample_rate, 200.0, true);
    let mut spectral = SpectralFluxAccumulator::new();

    while frames_processed < max_frames {
        check_cancel(cancel_token)?;
        chunk.clear();
        let Some(sample_count) = decoder
            .decode_next_into(&mut chunk)
            .map_err(|e| e.to_string())?
        else {
            break;
        };
        if sample_count == 0 {
            continue;
        }

        meter.process(&chunk);

        for frame in chunk.chunks_exact(channels) {
            if frames_processed >= max_frames {
                break;
            }
            let mono = (frame.iter().sum::<f64>() / channels as f64) as f32;
            let low = low_filter.process(mono);
            let vocal = vocal_lowpass.process(vocal_highpass.process(mono));

            if let Some(rms) = env_acc.process(mono) {
                segment.envelope.push(rms);
            }
            if let Some(rms) = low_acc.process(low) {
                segment.low_envelope.push(rms);
            }
            if let Some(rms) = vocal_acc.process(vocal) {
                let base = segment.envelope.last().copied().unwrap_or(1.0);
                segment
                    .vocal_ratio
                    .push(if base > 0.0001 { rms / base } else { 0.0 });
            }
            if let Some(flux) = spectral.process(mono) {
                segment.spectral_flux.push(flux);
            }
            frames_processed += 1;
        }
    }

    Ok(())
}

fn check_cancel(cancel_token: Option<&DecodeCancelToken>) -> Result<(), String> {
    if cancel_token.is_some_and(DecodeCancelToken::is_cancelled) {
        Err("Analysis task canceled".to_string())
    } else {
        Ok(())
    }
}

fn finalize_analysis(
    mode: AutomixAnalysisMode,
    analyze_window: f64,
    duration: f64,
    meter: &LoudnessMeter,
    head: &AnalysisSegment,
    tail: &AnalysisSegment,
) -> AutomixAnalysis {
    let effective_duration = if duration.is_finite() && duration > 0.0 {
        duration
    } else {
        head.envelope.len() as f64 / ENVELOPE_RATE
    };
    let (fade_in, fade_out) = detect_silence(
        &head.envelope,
        if mode.includes_tail() {
            &tail.envelope
        } else {
            &[]
        },
        effective_duration,
        ENVELOPE_RATE,
        SILENCE_THRESHOLD_DB,
    );
    let (bpm, bpm_confidence, first_beat) = detect_bpm(
        if head.spectral_flux.len() >= 100 {
            &head.spectral_flux
        } else {
            &head.envelope
        },
        ENVELOPE_RATE,
    );
    let drop_pos = detect_drop(&head.envelope, ENVELOPE_RATE);
    let (vocal_in, vocal_out, vocal_last_in) = detect_vocals(
        &head.envelope,
        &head.vocal_ratio,
        if mode.includes_tail() {
            &tail.envelope
        } else {
            &[]
        },
        if mode.includes_tail() {
            &tail.vocal_ratio
        } else {
            &[]
        },
        effective_duration,
        ENVELOPE_RATE,
        fade_in,
        fade_out,
    );
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
    let energy_profile = build_energy_profile(
        &head.envelope,
        if mode.includes_tail() {
            &tail.envelope
        } else {
            &[]
        },
        effective_duration,
    );
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
        vocal_out_pos: if mode.includes_tail() {
            vocal_out
        } else {
            None
        },
        vocal_last_in_pos: if mode.includes_tail() {
            vocal_last_in
        } else {
            None
        },
        outro_energy_level: if mode.includes_tail() {
            calculate_outro_energy(&tail.envelope, ENVELOPE_RATE)
        } else {
            None
        },
        key_root: None,
        key_mode: None,
        key_confidence: None,
        camelot_key: None,
    }
}

pub fn detect_silence(
    head: &[f32],
    tail: &[f32],
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
        let tail_duration = tail.len() as f64 / rate;
        let tail_start = (duration - tail_duration).max(0.0);
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

    let max_lag = BPM_MAX_LAG.min(flux.len().saturating_sub(1));
    let mut best_corr = 0.0_f32;
    let mut best_lag = 0usize;
    let mut corr_sum = 0.0_f32;
    let mut corr_count = 0usize;

    for lag in BPM_MIN_LAG..=max_lag {
        let mut sum = 0.0;
        for idx in 0..flux.len() - lag {
            sum += flux[idx] * flux[idx + lag];
        }
        let normalized = sum / (flux.len() - lag) as f32;
        corr_sum += normalized;
        corr_count += 1;
        if normalized > best_corr {
            best_corr = normalized;
            best_lag = lag;
        }
    }

    if best_lag == 0 || best_corr <= 1.0e-5 {
        return (None, None, None);
    }

    let average_corr = if corr_count == 0 {
        0.0
    } else {
        corr_sum / corr_count as f32
    };
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

#[allow(clippy::too_many_arguments)]
fn detect_vocals(
    head_env: &[f32],
    head_ratio: &[f32],
    tail_env: &[f32],
    tail_ratio: &[f32],
    duration: f64,
    rate: f64,
    fade_in: f64,
    fade_out: f64,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let is_vocal = |ratio: f32, env: f32| ratio > 0.4 && env > 0.02;
    let vocal_in = head_ratio
        .iter()
        .zip(head_env.iter())
        .enumerate()
        .skip((fade_in * rate) as usize)
        .find(|(_, (ratio, env))| is_vocal(**ratio, **env))
        .map(|(idx, _)| idx as f64 / rate);

    let (scan_env, scan_ratio, base_time) = if tail_env.is_empty() {
        (head_env, head_ratio, 0.0)
    } else {
        (
            tail_env,
            tail_ratio,
            (duration - tail_env.len() as f64 / rate).max(0.0),
        )
    };
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

fn build_energy_profile(head: &[f32], tail: &[f32], duration: f64) -> Vec<f64> {
    let profile_rate = 10.0;
    let len = ((duration * profile_rate).ceil() as usize).max(1);
    let mut profile = vec![0.0; len];
    fill_energy_profile(&mut profile, head, 0.0, ENVELOPE_RATE, profile_rate);
    if !tail.is_empty() {
        let tail_start = (duration - tail.len() as f64 / ENVELOPE_RATE).max(0.0);
        fill_energy_profile(&mut profile, tail, tail_start, ENVELOPE_RATE, profile_rate);
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
}
