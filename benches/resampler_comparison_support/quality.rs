//! Objective quality and complete-stream validation for comparison engines.

use std::f64::consts::PI;

use super::adapters::{AdapterProgress, EngineFactory};
use super::{
    rounded_output_frames, MetricClassification, QualitySummary, RatePair, SampleFormat,
    MAX_DRAIN_CALLS,
};

const TONE_AMPLITUDE: f64 = 0.5;
const PASSBAND_LOW_HZ: f64 = 997.0;
const PASSBAND_HIGH_HZ: f64 = 18_000.0;
const STOPBAND_INPUT_HZ: f64 = 23_000.0;
const MINIMUM_FIT_FRAMES: usize = 32;
const MAX_PROCESS_CALL_MULTIPLIER: usize = 8;
const MINIMUM_IMPULSE_PEAK: f64 = 1.0e-9;
const MINIMUM_IMPULSE_RMS: f64 = 1.0e-10;
const MINIMUM_PASSBAND_AMPLITUDE: f64 = TONE_AMPLITUDE * 1.0e-4;

struct RenderedSignal {
    samples: Vec<f64>,
    expected_frames: usize,
    actual_frames: usize,
    reported_api_buffering_latency_frames: Option<usize>,
    observed_input_frames_before_first_output: Option<usize>,
    finite: bool,
    terminal: bool,
}

#[derive(Clone, Copy)]
struct SineFit {
    amplitude: f64,
    thdn_db: f64,
}

pub(crate) fn measure_quality(
    factory: &EngineFactory,
    rate: RatePair,
    channels: usize,
    chunk_frames: usize,
    input_frames: usize,
    output_capacity_frames: usize,
) -> Result<QualitySummary, String> {
    if channels == 0 || chunk_frames == 0 || input_frames == 0 || output_capacity_frames == 0 {
        return Err(format!(
            "quality workload requires non-zero geometry: channels={channels}, chunk_frames={chunk_frames}, input_frames={input_frames}, output_capacity_frames={output_capacity_frames}"
        ));
    }

    let impulse = render_signal(
        factory,
        rate,
        channels,
        chunk_frames,
        input_frames,
        output_capacity_frames,
        |frame, _channel| f64::from(frame == 0),
    )?;
    let tone_997 = render_signal(
        factory,
        rate,
        channels,
        chunk_frames,
        input_frames,
        output_capacity_frames,
        |frame, _channel| tone_sample(frame, rate.from_hz, PASSBAND_LOW_HZ, TONE_AMPLITUDE),
    )?;
    let tone_18k = render_signal(
        factory,
        rate,
        channels,
        chunk_frames,
        input_frames,
        output_capacity_frames,
        |frame, _channel| tone_sample(frame, rate.from_hz, PASSBAND_HIGH_HZ, TONE_AMPLITUDE),
    )?;

    let nominal_output_frames = rounded_output_frames(input_frames, rate.from_hz, rate.to_hz);
    let mut validity_errors = Vec::new();
    let (measured_impulse_peak_frame, measured_impulse_peak_magnitude) = match impulse_peak(
        &impulse.samples,
        channels,
    ) {
        Some((frame, magnitude)) if magnitude >= MINIMUM_IMPULSE_PEAK => (frame, magnitude),
        Some((frame, magnitude)) => {
            validity_errors.push(format!(
                    "impulse peak magnitude {magnitude:.6e} at frame {frame} is below analyzable threshold {MINIMUM_IMPULSE_PEAK:.6e}"
                ));
            (frame, magnitude)
        }
        None => {
            validity_errors.push("impulse output has no finite peak".to_string());
            (0, 0.0)
        }
    };
    match first_channel_rms(&impulse.samples, channels) {
        Some(rms) if rms >= MINIMUM_IMPULSE_RMS => {}
        Some(rms) => validity_errors.push(format!(
            "impulse output RMS {rms:.6e} is below analyzable threshold {MINIMUM_IMPULSE_RMS:.6e}"
        )),
        None => validity_errors.push("impulse output RMS is undefined".to_string()),
    }

    let fit_997 = measured_fit(
        &tone_997,
        channels,
        nominal_output_frames,
        rate.to_hz,
        PASSBAND_LOW_HZ,
        measured_impulse_peak_frame,
    )
    .map_err(|error| validity_errors.push(format!("997 Hz analysis failed: {error}")))
    .ok();
    let fit_18k = measured_fit(
        &tone_18k,
        channels,
        nominal_output_frames,
        rate.to_hz,
        PASSBAND_HIGH_HZ,
        measured_impulse_peak_frame,
    )
    .map_err(|error| validity_errors.push(format!("18 kHz analysis failed: {error}")))
    .ok();
    let gain_997_hz_db = fit_997
        .as_ref()
        .and_then(|fit| db_ratio_with_floor(fit.amplitude, TONE_AMPLITUDE).ok());
    let gain_18_khz_db = fit_18k
        .as_ref()
        .and_then(|fit| db_ratio_with_floor(fit.amplitude, TONE_AMPLITUDE).ok());
    let thdn_997_hz_db = fit_997.as_ref().map(|fit| fit.thdn_db);
    let passband_max_abs_deviation_db = gain_997_hz_db
        .zip(gain_18_khz_db)
        .map(|(low, high)| low.abs().max(high.abs()));

    let (stopband_input_hz, folded_alias_hz, alias_attenuation_db, alias_render) = if rate.from_hz
        == 48_000
        && rate.to_hz == 44_100
    {
        let rendered = render_signal(
            factory,
            rate,
            channels,
            chunk_frames,
            input_frames,
            output_capacity_frames,
            |frame, _channel| tone_sample(frame, rate.from_hz, STOPBAND_INPUT_HZ, TONE_AMPLITUDE),
        )?;
        let folded = fold_frequency(STOPBAND_INPUT_HZ, rate.to_hz);
        let amplitude = measured_amplitude(
            &rendered,
            channels,
            nominal_output_frames,
            rate.to_hz,
            folded,
            measured_impulse_peak_frame,
        )
        .map_err(|error| validity_errors.push(format!("23 kHz alias analysis failed: {error}")))
        .ok();
        (
            Some(STOPBAND_INPUT_HZ),
            Some(folded),
            amplitude.and_then(|value| db_ratio_with_floor(value, TONE_AMPLITUDE).ok()),
            Some(rendered),
        )
    } else {
        (None, None, None, None)
    };

    let mut renders = vec![
        ("impulse", &impulse),
        ("997 Hz", &tone_997),
        ("18 kHz", &tone_18k),
    ];
    if let Some(rendered) = &alias_render {
        renders.push(("23 kHz alias", rendered));
    }
    for (label, rendered) in &renders {
        if !rendered.finite {
            validity_errors.push(format!("{label} output contains non-finite samples"));
        }
        if rendered.actual_frames != rendered.expected_frames {
            validity_errors.push(format!(
                "{label} complete output has {} frames, expected {}",
                rendered.actual_frames, rendered.expected_frames
            ));
        }
        if rendered.expected_frames != impulse.expected_frames {
            validity_errors.push(format!(
                "{label} expected output {} differs from impulse expectation {}",
                rendered.expected_frames, impulse.expected_frames
            ));
        }
        if rendered.reported_api_buffering_latency_frames
            != impulse.reported_api_buffering_latency_frames
        {
            validity_errors.push(format!(
                "{label} API buffering latency changed from {:?} to {:?}",
                impulse.reported_api_buffering_latency_frames,
                rendered.reported_api_buffering_latency_frames
            ));
        }
        if rendered.observed_input_frames_before_first_output
            != impulse.observed_input_frames_before_first_output
        {
            validity_errors.push(format!(
                "{label} first-output buffering changed from {:?} to {:?}",
                impulse.observed_input_frames_before_first_output,
                rendered.observed_input_frames_before_first_output
            ));
        }
        if !rendered.terminal {
            validity_errors.push(format!("{label} drain did not reach terminal state"));
        }
    }
    let all_output_samples_finite = renders.iter().all(|(_, rendered)| rendered.finite);
    let valid = validity_errors.is_empty();

    Ok(QualitySummary {
        classification: MetricClassification::Report,
        valid,
        input_frames,
        expected_complete_output_frames: impulse.expected_frames,
        actual_complete_output_frames: impulse.actual_frames,
        all_output_samples_finite,
        reported_api_buffering_latency_frames: impulse.reported_api_buffering_latency_frames,
        observed_input_frames_before_first_output: impulse
            .observed_input_frames_before_first_output,
        measured_impulse_peak_frame,
        measured_impulse_peak_magnitude,
        gain_997_hz_db,
        gain_18_khz_db,
        passband_max_abs_deviation_db,
        thdn_997_hz_db,
        stopband_input_hz,
        folded_alias_hz,
        alias_attenuation_db,
        validity_errors,
    })
}

fn render_signal<F>(
    factory: &EngineFactory,
    rate: RatePair,
    channels: usize,
    chunk_frames: usize,
    input_frames: usize,
    output_capacity_frames: usize,
    sample_at: F,
) -> Result<RenderedSignal, String>
where
    F: Fn(usize, usize) -> f64,
{
    let mut input_f64 = Vec::with_capacity(input_frames.saturating_mul(channels));
    for frame in 0..input_frames {
        for channel in 0..channels {
            input_f64.push(sample_at(frame, channel));
        }
    }
    let input_f32 = input_f64
        .iter()
        .map(|sample| *sample as f32)
        .collect::<Vec<_>>();

    let mut adapter = factory.create(rate, channels, chunk_frames)?;
    let expected_frames = adapter.expected_complete_output_frames(input_frames);
    let reported_api_buffering_latency_frames = adapter.api_buffering_latency_frames();
    if output_capacity_frames < adapter.max_output_frames().max(1) {
        return Err(format!(
            "common quality output capacity {output_capacity_frames} is below {} required frames for {}",
            adapter.max_output_frames(),
            factory.identity().engine_id
        ));
    }
    let mut output_f64 = vec![0.0; output_capacity_frames.saturating_mul(channels)];
    let mut output_f32 = vec![0.0; output_capacity_frames.saturating_mul(channels)];
    let mut samples = Vec::with_capacity(expected_frames.saturating_mul(channels));
    let mut input_cursor = 0usize;
    let mut observed_input_frames_before_first_output = None;
    let maximum_process_calls = input_frames
        .div_ceil(chunk_frames)
        .saturating_mul(MAX_PROCESS_CALL_MULTIPLIER)
        .saturating_add(MAX_DRAIN_CALLS);
    let mut process_calls = 0usize;

    while input_cursor < input_frames {
        process_calls += 1;
        if process_calls > maximum_process_calls {
            return Err(format!(
                "{} exceeded {maximum_process_calls} quality process calls at frame {input_cursor}/{input_frames}",
                factory.identity().engine_id
            ));
        }
        let end = input_cursor.saturating_add(chunk_frames).min(input_frames);
        let supplied_frames = end - input_cursor;
        let progress = match adapter.sample_format() {
            SampleFormat::InterleavedF64 => {
                let input = &input_f64[input_cursor * channels..end * channels];
                if end == input_frames {
                    adapter.process_final_f64(input, &mut output_f64)?
                } else {
                    adapter.process_f64(input, &mut output_f64)?
                }
            }
            SampleFormat::InterleavedF32 => {
                let input = &input_f32[input_cursor * channels..end * channels];
                if end == input_frames {
                    adapter.process_final_f32(input, &mut output_f32)?
                } else {
                    adapter.process_f32(input, &mut output_f32)?
                }
            }
        };
        validate_driver_progress(
            factory,
            "quality process",
            progress,
            supplied_frames,
            output_capacity_frames,
        )?;
        if progress.finished {
            return Err(format!(
                "{} returned terminal state from ordinary quality process",
                factory.identity().engine_id
            ));
        }
        append_output(
            adapter.sample_format(),
            progress.produced_frames,
            channels,
            &output_f64,
            &output_f32,
            &mut samples,
        );
        if progress.produced_frames > 0 && observed_input_frames_before_first_output.is_none() {
            observed_input_frames_before_first_output = Some(input_cursor);
        }
        input_cursor = input_cursor.saturating_add(progress.consumed_frames);
    }

    let mut terminal = false;
    for _ in 0..MAX_DRAIN_CALLS {
        let progress = match adapter.sample_format() {
            SampleFormat::InterleavedF64 => adapter.drain_f64(&mut output_f64)?,
            SampleFormat::InterleavedF32 => adapter.drain_f32(&mut output_f32)?,
        };
        validate_driver_progress(
            factory,
            "quality drain",
            progress,
            0,
            output_capacity_frames,
        )?;
        append_output(
            adapter.sample_format(),
            progress.produced_frames,
            channels,
            &output_f64,
            &output_f32,
            &mut samples,
        );
        if progress.produced_frames > 0 && observed_input_frames_before_first_output.is_none() {
            observed_input_frames_before_first_output = Some(input_frames);
        }
        if progress.finished {
            terminal = true;
            break;
        }
        if progress.produced_frames == 0 {
            return Err(format!(
                "{} quality drain stalled before terminal state",
                factory.identity().engine_id
            ));
        }
    }

    let actual_frames = samples.len() / channels;
    let finite = samples.iter().all(|sample| sample.is_finite());
    Ok(RenderedSignal {
        samples,
        expected_frames,
        actual_frames,
        reported_api_buffering_latency_frames,
        observed_input_frames_before_first_output,
        finite,
        terminal,
    })
}

fn validate_driver_progress(
    factory: &EngineFactory,
    operation: &str,
    progress: AdapterProgress,
    input_capacity: usize,
    output_capacity: usize,
) -> Result<(), String> {
    if progress.consumed_frames > input_capacity || progress.produced_frames > output_capacity {
        return Err(format!(
            "{} {operation} returned consumed={}/{input_capacity}, produced={}/{output_capacity}",
            factory.identity().engine_id,
            progress.consumed_frames,
            progress.produced_frames
        ));
    }
    if input_capacity > 0 && progress.consumed_frames == 0 && progress.produced_frames == 0 {
        return Err(format!(
            "{} {operation} made no progress",
            factory.identity().engine_id
        ));
    }
    Ok(())
}

fn append_output(
    sample_format: SampleFormat,
    produced_frames: usize,
    channels: usize,
    output_f64: &[f64],
    output_f32: &[f32],
    samples: &mut Vec<f64>,
) {
    let produced_samples = produced_frames.saturating_mul(channels);
    match sample_format {
        SampleFormat::InterleavedF64 => {
            samples.extend_from_slice(&output_f64[..produced_samples]);
        }
        SampleFormat::InterleavedF32 => {
            samples.extend(
                output_f32[..produced_samples]
                    .iter()
                    .map(|sample| *sample as f64),
            );
        }
    }
}

fn measured_fit(
    rendered: &RenderedSignal,
    channels: usize,
    nominal_output_frames: usize,
    sample_rate: u32,
    frequency: f64,
    leading_output_delay_frames: usize,
) -> Result<SineFit, String> {
    let (start, take) = analysis_window(
        rendered,
        nominal_output_frames,
        sample_rate,
        MINIMUM_FIT_FRAMES,
        leading_output_delay_frames,
    )
    .ok_or_else(|| {
        format!(
            "not enough complete output for {frequency} Hz analysis: actual={}, nominal={}, leading_delay={leading_output_delay_frames}",
            rendered.actual_frames, nominal_output_frames
        )
    })?;
    let fit = fit_sine_interleaved(
        &rendered.samples,
        channels,
        sample_rate,
        frequency,
        start,
        take,
    )?;
    if !fit.amplitude.is_finite() || fit.amplitude < MINIMUM_PASSBAND_AMPLITUDE {
        return Err(format!(
            "fitted amplitude {:.6e} is below analyzable threshold {:.6e}",
            fit.amplitude, MINIMUM_PASSBAND_AMPLITUDE
        ));
    }
    if !fit.thdn_db.is_finite() {
        return Err("THD+N is undefined for the fitted passband signal".to_string());
    }
    Ok(fit)
}

fn measured_amplitude(
    rendered: &RenderedSignal,
    channels: usize,
    nominal_output_frames: usize,
    sample_rate: u32,
    frequency: f64,
    leading_output_delay_frames: usize,
) -> Result<f64, String> {
    let (start, take) = analysis_window(
        rendered,
        nominal_output_frames,
        sample_rate,
        MINIMUM_FIT_FRAMES,
        leading_output_delay_frames,
    )
    .ok_or_else(|| {
        format!(
            "not enough complete output for {frequency} Hz amplitude analysis: actual={}, nominal={}, leading_delay={leading_output_delay_frames}",
            rendered.actual_frames, nominal_output_frames
        )
    })?;
    let fit = fit_sine_interleaved(
        &rendered.samples,
        channels,
        sample_rate,
        frequency,
        start,
        take,
    )?;
    fit.amplitude
        .is_finite()
        .then_some(fit.amplitude)
        .ok_or_else(|| "fitted amplitude is non-finite".to_string())
}

fn analysis_window(
    rendered: &RenderedSignal,
    nominal_output_frames: usize,
    sample_rate: u32,
    minimum_frames: usize,
    leading_output_delay_frames: usize,
) -> Option<(usize, usize)> {
    let useful_start = leading_output_delay_frames;
    let useful_end = useful_start
        .saturating_add(nominal_output_frames)
        .min(rendered.actual_frames);
    let maximum_guard = useful_end.saturating_sub(useful_start + minimum_frames) / 2;
    let requested_guard = (sample_rate as usize / 10).max(4_096);
    let guard = requested_guard.min(maximum_guard);
    let start = useful_start.saturating_add(guard);
    let end = useful_end.saturating_sub(guard);
    (end.saturating_sub(start) >= minimum_frames).then_some((start, end - start))
}

fn fit_sine_interleaved(
    samples: &[f64],
    channels: usize,
    sample_rate: u32,
    frequency: f64,
    start_frame: usize,
    frame_count: usize,
) -> Result<SineFit, String> {
    if channels == 0 || !samples.len().is_multiple_of(channels) {
        return Err("invalid interleaved geometry for sine fit".to_string());
    }
    let total_frames = samples.len() / channels;
    let start = start_frame.min(total_frames);
    let count = frame_count.min(total_frames.saturating_sub(start));
    if count < MINIMUM_FIT_FRAMES {
        return Err(format!(
            "not enough samples for sine fit: count={count}, start={start}, total={total_frames}"
        ));
    }

    let omega = 2.0 * PI * frequency / sample_rate as f64;
    let mut matrix = [[0.0; 3]; 3];
    let mut rhs = [0.0; 3];
    for local in 0..count {
        let frame = start + local;
        let phase = omega * frame as f64;
        let basis = [phase.sin(), phase.cos(), 1.0];
        let sample = samples[frame * channels];
        for row in 0..3 {
            rhs[row] += basis[row] * sample;
            for col in 0..3 {
                matrix[row][col] += basis[row] * basis[col];
            }
        }
    }

    let coeffs = solve_3x3(matrix, rhs)?;
    let mut residual_sum = 0.0;
    for local in 0..count {
        let frame = start + local;
        let phase = omega * frame as f64;
        let fitted = coeffs[0] * phase.sin() + coeffs[1] * phase.cos() + coeffs[2];
        let error = samples[frame * channels] - fitted;
        residual_sum += error * error;
    }
    let amplitude = coeffs[0].hypot(coeffs[1]);
    let signal_rms = amplitude / 2.0_f64.sqrt();
    let residual_rms = (residual_sum / count as f64).sqrt();
    Ok(SineFit {
        amplitude,
        thdn_db: db_ratio_with_floor(residual_rms, signal_rms).unwrap_or(f64::NAN),
    })
}

#[allow(clippy::needless_range_loop)]
fn solve_3x3(mut matrix: [[f64; 3]; 3], mut rhs: [f64; 3]) -> Result<[f64; 3], String> {
    for pivot in 0..3 {
        let mut best_row = pivot;
        let mut best_abs = matrix[pivot][pivot].abs();
        for row in (pivot + 1)..3 {
            let candidate = matrix[row][pivot].abs();
            if candidate > best_abs {
                best_abs = candidate;
                best_row = row;
            }
        }
        if best_abs < 1.0e-24 {
            return Err("singular sine-fit matrix".to_string());
        }
        if best_row != pivot {
            matrix.swap(pivot, best_row);
            rhs.swap(pivot, best_row);
        }
        let pivot_value = matrix[pivot][pivot];
        for col in pivot..3 {
            matrix[pivot][col] /= pivot_value;
        }
        rhs[pivot] /= pivot_value;
        for row in 0..3 {
            if row == pivot {
                continue;
            }
            let factor = matrix[row][pivot];
            for col in pivot..3 {
                matrix[row][col] -= factor * matrix[pivot][col];
            }
            rhs[row] -= factor * rhs[pivot];
        }
    }
    Ok(rhs)
}

fn impulse_peak(samples: &[f64], channels: usize) -> Option<(usize, f64)> {
    if channels == 0 || samples.len() < channels {
        return None;
    }
    let mut peak_frame = 0usize;
    let mut peak = -1.0_f64;
    for (frame, samples) in samples.chunks_exact(channels).enumerate() {
        let magnitude = samples[0].abs();
        if !magnitude.is_finite() {
            return None;
        }
        if magnitude > peak {
            peak = magnitude;
            peak_frame = frame;
        }
    }
    Some((peak_frame, peak))
}

fn first_channel_rms(samples: &[f64], channels: usize) -> Option<f64> {
    if channels == 0 || samples.len() < channels || !samples.len().is_multiple_of(channels) {
        return None;
    }
    let mut sum = 0.0;
    let mut frames = 0usize;
    for frame in samples.chunks_exact(channels) {
        let sample = frame[0];
        if !sample.is_finite() {
            return None;
        }
        sum += sample * sample;
        frames += 1;
    }
    (frames > 0).then(|| (sum / frames as f64).sqrt())
}

fn tone_sample(frame: usize, sample_rate: u32, frequency: f64, amplitude: f64) -> f64 {
    amplitude * (2.0 * PI * frequency * frame as f64 / sample_rate as f64).sin()
}

fn fold_frequency(frequency: f64, sample_rate: u32) -> f64 {
    let sample_rate = sample_rate as f64;
    let folded = frequency.rem_euclid(sample_rate);
    if folded > sample_rate / 2.0 {
        sample_rate - folded
    } else {
        folded
    }
}

fn db_ratio_with_floor(numerator: f64, denominator: f64) -> Result<f64, String> {
    if !numerator.is_finite() || !denominator.is_finite() || numerator < 0.0 || denominator <= 0.0 {
        Err(format!(
            "invalid dB ratio numerator={numerator}, denominator={denominator}"
        ))
    } else if numerator == 0.0 {
        Ok(-400.0)
    } else {
        Ok((20.0 * (numerator / denominator).log10()).max(-400.0))
    }
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::resampler_comparison_support::adapters;

    #[test]
    fn least_squares_fit_handles_non_integral_frequency_and_dc() {
        let frames = 5_000;
        let channels = 2;
        let mut samples = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            let sample = 0.125 + tone_sample(frame, 48_000, 997.0, 0.5);
            samples.extend_from_slice(&[sample, sample]);
        }
        let fit = fit_sine_interleaved(&samples, channels, 48_000, 997.0, 257, 4_000).unwrap();
        assert!((fit.amplitude - 0.5).abs() < 1.0e-12);
        assert!(fit.thdn_db < -200.0, "THD+N was {} dB", fit.thdn_db);
    }

    #[test]
    fn stopband_frequency_folds_to_expected_alias() {
        assert!((fold_frequency(23_000.0, 44_100) - 21_100.0).abs() < 1.0e-12);
    }

    #[test]
    fn project_adapter_renders_an_exact_finite_complete_stream() {
        let discovery = adapters::discover(
            None,
            &BTreeMap::new(),
            &BTreeSet::new(),
            #[cfg(feature = "rubato")]
            adapters::RawRubatoGeometry::FFT_512_1,
        );
        let factory = discovery
            .factories
            .iter()
            .find(|factory| factory.identity().engine_id == super::super::PROJECT_ENGINE_ID)
            .expect("project adapter must always be available");
        let rate = RatePair {
            id: "test",
            from_hz: 44_100,
            to_hz: 48_000,
        };
        let rendered =
            render_signal(factory, rate, 2, 512, 2_048, 2_048, |_frame, _channel| 0.0).unwrap();
        assert!(rendered.terminal);
        assert!(rendered.finite);
        assert_eq!(rendered.actual_frames, rendered.expected_frames);
    }

    #[test]
    fn exact_length_silent_adapter_cannot_pass_quality_validation() {
        let factory = adapters::EngineFactory::silent_test();
        let rate = RatePair {
            id: "silent_test",
            from_hz: 44_100,
            to_hz: 48_000,
        };
        let quality = measure_quality(&factory, rate, 2, 512, 2_048, 2_048).unwrap();

        assert!(!quality.valid);
        assert_eq!(
            quality.expected_complete_output_frames,
            quality.actual_complete_output_frames
        );
        assert!(quality.all_output_samples_finite);
        assert_eq!(quality.measured_impulse_peak_magnitude, 0.0);
        assert!(quality.gain_997_hz_db.is_none());
        assert!(quality.thdn_997_hz_db.is_none());
        assert!(quality
            .validity_errors
            .iter()
            .any(|error| error.contains("impulse peak magnitude")));
        assert!(quality
            .validity_errors
            .iter()
            .any(|error| error.contains("997 Hz analysis failed")));
    }
}
