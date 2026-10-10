//! Complete-stream rendering and startup accounting for comparison probes.

use super::adapters::{AdapterProgress, EngineFactory};
use super::{QualitySummary, RatePair, SampleFormat, MAX_DRAIN_CALLS};

const MAX_PROCESS_CALL_MULTIPLIER: usize = 8;

pub(super) struct RenderedSignal {
    pub(super) samples: Vec<f64>,
    pub(super) expected_frames: usize,
    pub(super) actual_frames: usize,
    pub(super) reported_api_buffering_latency_frames: Option<usize>,
    pub(super) observed_input_frames_before_first_output: Option<usize>,
    pub(super) observed_input_frames_through_first_output: Option<usize>,
    pub(super) first_output_during_drain: bool,
    pub(super) finite: bool,
    pub(super) terminal: bool,
}

pub(crate) fn measure_quality(
    factory: &EngineFactory,
    rate: RatePair,
    channels: usize,
    chunk_frames: usize,
    input_frames: usize,
    output_capacity_frames: usize,
) -> Result<QualitySummary, String> {
    super::quality_grid::measure_quality(
        factory,
        rate,
        channels,
        chunk_frames,
        input_frames,
        output_capacity_frames,
    )
}

pub(super) fn render_signal<F>(
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
    let mut observed_input_frames_through_first_output = None;
    let mut first_output_during_drain = false;
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
            observed_input_frames_through_first_output =
                Some(input_cursor + progress.consumed_frames);
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
            observed_input_frames_through_first_output = Some(input_frames);
            first_output_during_drain = true;
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
        observed_input_frames_through_first_output,
        first_output_during_drain,
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

pub(super) fn analysis_window(
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
    // Retain at least half of the usable stream. Capping the guard only by
    // `minimum_frames` collapsed 96k -> 44.1k analysis to a 32-frame fit.
    let maximum_guard = useful_end.saturating_sub(useful_start) / 4;
    let requested_guard = (sample_rate as usize / 10).max(4_096);
    let guard = requested_guard.min(maximum_guard);
    let start = useful_start.saturating_add(guard);
    let end = useful_end.saturating_sub(guard);
    (end.saturating_sub(start) >= minimum_frames).then_some((start, end - start))
}

#[cfg(test)]
#[allow(unused_imports)] // Custom-harness benches also compile with cfg(test).
mod tests {
    use super::*;
    use crate::resampler_comparison_support::adapters;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn first_output_counts_input_consumed_by_the_producing_call() {
        let factory = adapters::EngineFactory::silent_test();
        let rate = super::super::RATE_PAIRS[0];
        let rendered = render_signal(&factory, rate, 2, 512, 2048, 2048, |_, _| 0.0).unwrap();
        assert_eq!(rendered.observed_input_frames_before_first_output, Some(0));
        assert_eq!(
            rendered.observed_input_frames_through_first_output,
            Some(512)
        );
        assert!(!rendered.first_output_during_drain);
    }

    #[test]
    fn short_high_rate_probe_retains_a_useful_interior_window() {
        let rendered = RenderedSignal {
            samples: vec![],
            expected_frames: 7526,
            actual_frames: 7526,
            reported_api_buffering_latency_frames: None,
            observed_input_frames_before_first_output: Some(0),
            observed_input_frames_through_first_output: Some(512),
            first_output_during_drain: false,
            finite: true,
            terminal: true,
        };
        let (_, count) = analysis_window(&rendered, 7526, 44100, 32, 0).unwrap();
        assert!(count >= 3763);
    }

    #[test]
    fn project_adapter_renders_an_exact_finite_complete_stream() {
        let discovery = adapters::discover(None, &BTreeMap::new(), &BTreeSet::new());
        let factory = discovery
            .factories
            .iter()
            .find(|factory| factory.identity().engine_id == super::super::PROJECT_ENGINE_ID)
            .expect("project adapter must always be available");
        let rate = RatePair {
            id: "test",
            from_hz: 44_100,
            to_hz: 48_000,
            channels: 2,
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
            channels: 2,
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
