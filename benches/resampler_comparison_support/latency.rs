//! Separately instrumented process-call tails; never part of throughput trials.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::adapters::EngineFactory;
use super::measurement::MeasurementIdentity;
use super::{
    drain_adapter, process_prepared, ComparisonCase, MetricClassification, PreparedInputs,
    RatePair, CHUNK_FRAMES, WARMUP_BUFFERS,
};
use crate::support::{summarize_callback_samples, CallbackTailDistribution};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct CallLatency {
    pub(crate) identity: MeasurementIdentity,
    pub(crate) classification: MetricClassification,
    pub(crate) scope: String,
    pub(crate) process_call_ns: CallbackTailDistribution,
    pub(crate) timer_read_overhead_ns: Vec<u64>,
    pub(crate) consumed_frames_per_call: Vec<usize>,
    pub(crate) produced_frames_per_call: Vec<usize>,
    pub(crate) warmup_consumed_frames: usize,
    pub(crate) warmup_output_frames: usize,
    pub(crate) drain_frames: usize,
    pub(crate) drain_terminal: bool,
    pub(crate) expected_complete_output_frames: usize,
    pub(crate) actual_complete_output_frames: usize,
    pub(crate) p99_source_realtime_utilization_pct: f64,
    pub(crate) calls_above_source_buffer_duration: usize,
    pub(crate) valid: bool,
}

pub(crate) fn measure(
    factory: &EngineFactory,
    rate: RatePair,
    inputs: &PreparedInputs,
    iterations: usize,
    capacity: usize,
) -> Result<CallLatency, String> {
    let chunk_frames = inputs.f64_samples.len() / rate.channels;
    let mut adapter = factory.create(rate, rate.channels, chunk_frames)?;
    if capacity < adapter.max_output_frames().max(1) {
        return Err("insufficient latency-pass capacity".to_string());
    }
    let mut output_f64 = vec![0.0; capacity * rate.channels];
    let mut output_f32 = vec![0.0; capacity * rate.channels];
    let timer_read_overhead_ns = (0..256)
        .map(|_| {
            let start = Instant::now();
            start.elapsed().as_nanos() as u64
        })
        .collect();
    let mut warmup_consumed = 0;
    let mut warmup_produced = 0;
    for _ in 0..WARMUP_BUFFERS {
        let progress = process_prepared(
            &mut adapter,
            inputs,
            &mut output_f64,
            &mut output_f32,
            false,
        )?;
        if progress.consumed_frames != chunk_frames || progress.finished {
            return Err("invalid progress during latency warmup".to_string());
        }
        warmup_consumed += progress.consumed_frames;
        warmup_produced += progress.produced_frames;
    }
    let mut durations = Vec::with_capacity(iterations);
    let mut consumed = Vec::with_capacity(iterations);
    let mut produced = Vec::with_capacity(iterations);
    for call in 0..iterations {
        let start = Instant::now();
        let progress = process_prepared(
            &mut adapter,
            inputs,
            &mut output_f64,
            &mut output_f32,
            call + 1 == iterations,
        )?;
        // A buffered call may finish within one clock tick. Preserve zero as
        // below timer resolution instead of rejecting it or inventing 1 ns.
        let duration = start.elapsed().as_nanos() as f64;
        if progress.consumed_frames != chunk_frames || progress.finished {
            return Err(format!("invalid progress during latency call {call}"));
        }
        durations.push(duration);
        consumed.push(progress.consumed_frames);
        produced.push(progress.produced_frames);
    }
    let expected =
        adapter.expected_complete_output_frames(warmup_consumed + consumed.iter().sum::<usize>());
    let (drain_frames, terminal) = drain_adapter(&mut adapter, &mut output_f64, &mut output_f32)?;
    let actual = warmup_produced + produced.iter().sum::<usize>() + drain_frames;
    let distribution = summarize_calls(durations)?;
    let source_ns = chunk_frames as f64 / f64::from(rate.from_hz) * 1.0e9;
    let mut identity = MeasurementIdentity::new(factory.identity(), rate, capacity);
    identity.chunk_frames = chunk_frames;
    Ok(CallLatency {
        identity,
        classification:MetricClassification::Report,
        scope:"separate_instrumented_process_calls;includes_timer_overhead;last_call_signals_end;excludes_setup_reset_drain_and_device_scheduling".to_string(),
        p99_source_realtime_utilization_pct:distribution.p99/source_ns*100.0,
        calls_above_source_buffer_duration:distribution.samples.iter().filter(|&&ns| ns>source_ns).count(),
        process_call_ns:distribution,timer_read_overhead_ns,
        consumed_frames_per_call:consumed,produced_frames_per_call:produced,
        warmup_consumed_frames:warmup_consumed,warmup_output_frames:warmup_produced,
        drain_frames,drain_terminal:terminal,expected_complete_output_frames:expected,
        actual_complete_output_frames:actual,valid:terminal && expected==actual,
    })
}

pub(crate) fn validate(case: &ComparisonCase, iterations: usize) -> Result<(), String> {
    let latency = case
        .process_call_latency
        .as_ref()
        .ok_or_else(|| "missing process-call latency pass".to_string())?;
    let identity = &case
        .trial_records
        .first()
        .ok_or_else(|| "missing throughput trial identity".to_string())?
        .identity;
    let distribution = summarize_calls(latency.process_call_ns.samples.clone())?;
    let actual = latency
        .warmup_output_frames
        .checked_add(latency.produced_frames_per_call.iter().sum())
        .and_then(|n| n.checked_add(latency.drain_frames));
    let valid = latency.drain_terminal && actual == Some(latency.expected_complete_output_frames);
    if &latency.identity != identity
        || distribution != latency.process_call_ns
        || distribution.samples.len() != iterations
        || latency.consumed_frames_per_call.len() != iterations
        || latency.produced_frames_per_call.len() != iterations
        || !latency
            .consumed_frames_per_call
            .iter()
            .all(|&n| n == CHUNK_FRAMES)
        || latency.warmup_consumed_frames != WARMUP_BUFFERS * CHUNK_FRAMES
        || actual != Some(latency.actual_complete_output_frames)
        || latency.valid != valid
        || !valid
        || latency.p99_source_realtime_utilization_pct
            != distribution.p99 / case.source_buffer_duration_ns * 100.0
        || latency.calls_above_source_buffer_duration
            != distribution
                .samples
                .iter()
                .filter(|&&ns| ns > case.source_buffer_duration_ns)
                .count()
    {
        return Err(format!(
            "{} invalid identified process-call latency records",
            case.case_key
        ));
    }
    Ok(())
}

fn summarize_calls(samples: Vec<f64>) -> Result<CallbackTailDistribution, String> {
    if !samples.contains(&0.0) {
        return summarize_callback_samples(samples);
    }
    if samples.iter().any(|n| !n.is_finite() || *n < 0.0) {
        return Err("invalid process-call duration".into());
    }
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| sorted[((sorted.len() as f64 * p).ceil() as usize).max(1) - 1];
    let middle = sorted.len() / 2;
    let median = if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    };
    Ok(CallbackTailDistribution {
        min: sorted[0],
        median,
        p95: percentile(0.95),
        p99: percentile(0.99),
        p99_9: percentile(0.999),
        max: sorted[sorted.len() - 1],
        samples,
    })
}

#[cfg(test)]
mod resolution_tests {
    #[test]
    fn sub_tick_calls_remain_zero_in_distribution() {
        let result = super::summarize_calls(vec![0.0, 100.0, 0.0, 200.0]).unwrap();
        assert_eq!(result.min, 0.0);
        assert_eq!(result.median, 50.0);
        assert_eq!(result.p99, 200.0);
        assert_eq!(result.samples, [0.0, 100.0, 0.0, 200.0]);
        assert!(super::summarize_calls(vec![0.0, -1.0]).is_err());
    }
}
