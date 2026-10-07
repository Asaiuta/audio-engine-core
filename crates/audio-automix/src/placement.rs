//! Music evidence, cut placement, and stable result assembly.
use crate::features::{spectral_hop_size, spectral_observation_offset_sec, AnalysisSegment};
use crate::silence::{sound_boundaries, SoundBoundaries};
use crate::tempo::{self, BeatGrid, TempoEstimate};
use crate::{
    check_cancel, AutomixAnalysis, AutomixAnalysisOptions, AutomixError, ANALYSIS_VERSION,
    ENERGY_PROFILE_RATE, HEAD_BEAT_TOLERANCE_SEC, MAX_DECLARED_DURATION_SEC, SILENCE_THRESHOLD_DB,
};
use audio_engine_core::analysis::LoudnessMeter;

#[path = "drop.rs"]
mod drop;

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
    // Silence v2: every fade/cut/mix/outro consumer below reads this one result.
    let boundaries = sound_boundaries(head, tail, effective_duration, cancel)?;
    let SoundBoundaries {
        fade_in, fade_out, ..
    } = boundaries;
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
        let rate = head.envelope_rate();
        (fallback.as_slice(), rate, 1.0 / rate)
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
    let drop_pos = drop::detect_drop(head, head.envelope_rate(), tempo, cancel)?;
    let cut_in = calculate_smart_cut_in(tempo, drop_pos, fade_in);
    let cut_out = if mode.includes_tail() {
        Some(calculate_smart_cut_out(tempo, fade_out, effective_duration))
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
        // No independently validated vocal detector is exposed in schema v4.
        vocal_in_pos: None,
        vocal_out_pos: None,
        vocal_last_in_pos: None,
        outro_energy_level: tail
            .zip(boundaries.outro_end_bin)
            .and_then(|(segment, end)| {
                outro_energy(&segment.envelope, end, segment.envelope_rate())
            }),
    })
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

pub(super) fn calculate_smart_cut_out(tempo: TempoEstimate, fade_out: f64, duration: f64) -> f64 {
    if let Some(grid) = tempo.usable_grid() {
        return snap_to_beat(fade_out, grid).min(duration);
    }
    fade_out
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
        head.envelope_rate(),
        profile_rate,
    );
    if let Some(tail) = tail {
        fill_energy_profile(
            &mut profile,
            &tail.envelope,
            tail.start_time,
            tail.envelope_rate(),
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

/// RMS level of the up-to-10 s of tail envelope that ends at the shared
/// Silence v2 boundary bin `end` (exclusive), in dBFS; -70 for digital silence.
pub(super) fn outro_energy(tail: &[f32], end: usize, rate: f64) -> Option<f64> {
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

fn mean_square(values: &[f32]) -> f32 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().map(|value| value * value).sum::<f32>() / values.len() as f32
    }
}
