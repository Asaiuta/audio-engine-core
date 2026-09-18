//! Music evidence, cut placement, and stable result assembly.
use crate::features::{spectral_hop_size, spectral_observation_offset_sec, AnalysisSegment};
use crate::tempo::{self, BeatGrid, TempoEstimate};
use crate::{
    check_cancel, AutomixAnalysis, AutomixAnalysisOptions, AutomixError, ANALYSIS_VERSION,
    ENERGY_PROFILE_RATE, ENVELOPE_RATE, HEAD_BEAT_TOLERANCE_SEC, MAX_DECLARED_DURATION_SEC,
    SILENCE_THRESHOLD_DB,
};
use audio_engine_core::analysis::LoudnessMeter;

pub(super) fn finalize_analysis(
    options: &AutomixAnalysisOptions,
    duration: f64,
    sample_rate: u32,
    meter: &LoudnessMeter,
    head: &AnalysisSegment,
    tail: &AnalysisSegment,
    cancel: Option<&dyn Fn() -> bool>,
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

pub(super) fn detect_silence(
    head: &[f32],
    tail: &[f32],
    duration: f64,
    rate: f64,
    db_thresh: f32,
) -> (f64, f64) {
    let tail_start = (!tail.is_empty()).then(|| (duration - tail.len() as f64 / rate).max(0.0));
    detect_silence_at(head, tail, tail_start, duration, rate, db_thresh)
}

pub(super) fn reported_first_beat(grid: BeatGrid, head: &AnalysisSegment) -> f64 {
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

pub(super) fn calculate_smart_cut_in(
    tempo: TempoEstimate,
    anchor: Option<f64>,
    fade_in: f64,
) -> f64 {
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

pub(super) fn calculate_smart_cut_out(
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

pub(super) fn snap_to_beat(time: f64, grid: BeatGrid) -> f64 {
    let units = ((time - grid.first_beat_sec) / grid.period_sec).round();
    (grid.first_beat_sec + units * grid.period_sec).max(0.0)
}

pub(super) fn build_energy_profile(
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
