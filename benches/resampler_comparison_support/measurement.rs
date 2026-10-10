//! Identity-preserving trial collection and reconstruction of published metrics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    ComparisonCase, ComparisonConditions, EngineIdentity, MetricClassification, QualitySummary,
    RatePair, SampleFormat, WorkValidation, CHUNK_FRAMES, WARMUP_BUFFERS,
};
use crate::support::{summarize_trials, TrialDistribution};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MeasurementIdentity {
    pub(crate) engine_id: String,
    pub(crate) algorithm_id: String,
    pub(crate) sample_format: SampleFormat,
    pub(crate) rate_pair: String,
    pub(crate) from_rate_hz: u32,
    pub(crate) to_rate_hz: u32,
    pub(crate) channels: usize,
    pub(crate) chunk_frames: usize,
    pub(crate) output_capacity_frames: usize,
}

impl MeasurementIdentity {
    pub(crate) fn new(engine: &EngineIdentity, rate: RatePair, capacity: usize) -> Self {
        Self {
            engine_id: engine.engine_id.clone(),
            algorithm_id: engine.algorithm_id.clone(),
            sample_format: engine.sample_format,
            rate_pair: rate.id.to_string(),
            from_rate_hz: rate.from_hz,
            to_rate_hz: rate.to_hz,
            channels: rate.channels,
            chunk_frames: CHUNK_FRAMES,
            output_capacity_frames: capacity,
        }
    }

    fn case_key(&self) -> String {
        format!(
            "engine={};format={};rate={};from={};to={};channels={};frames={};algorithm={}",
            self.engine_id,
            self.sample_format.as_str(),
            self.rate_pair,
            self.from_rate_hz,
            self.to_rate_hz,
            self.channels,
            self.chunk_frames,
            self.algorithm_id
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct TrialMeasurement {
    pub(crate) setup_ns: f64,
    pub(crate) steady_ns_per_input_sample: f64,
    pub(crate) steady_ns_per_input_buffer: f64,
    pub(crate) steady_cycles_per_input_sample: Option<f64>,
    pub(crate) steady_cycles_per_input_buffer: Option<f64>,
    pub(crate) reset_ns: f64,
    pub(crate) drain_ns: f64,
    pub(crate) warmup_consumed_frames: usize,
    pub(crate) timed_consumed_frames: usize,
    pub(crate) expected_complete_output_frames: usize,
    pub(crate) warmup_output_frames: usize,
    pub(crate) steady_output_frames: usize,
    pub(crate) drain_frames: usize,
    pub(crate) actual_complete_output_frames: usize,
    pub(crate) drain_terminal: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct TrialRecord {
    pub(crate) identity: MeasurementIdentity,
    pub(crate) trial_index: usize,
    pub(crate) measurement: TrialMeasurement,
}

pub(crate) struct CaseAccumulator {
    engine: EngineIdentity,
    identity: MeasurementIdentity,
    records: Vec<Option<TrialRecord>>,
}

impl CaseAccumulator {
    pub(crate) fn new(
        engine: EngineIdentity,
        rate: RatePair,
        capacity: usize,
        trials: usize,
    ) -> Self {
        Self {
            identity: MeasurementIdentity::new(&engine, rate, capacity),
            engine,
            records: vec![None; trials],
        }
    }

    pub(crate) fn push(&mut self, record: TrialRecord) -> Result<(), String> {
        if record.identity != self.identity {
            return Err(format!(
                "trial {} identity mismatch: expected {:?}, received {:?}",
                record.trial_index, self.identity, record.identity
            ));
        }
        let index = record.trial_index;
        let slot = self
            .records
            .get_mut(index)
            .ok_or_else(|| format!("trial index {index} is outside the declared schedule"))?;
        if slot.is_some() {
            return Err(format!(
                "duplicate trial index {index} for {}",
                self.identity.case_key()
            ));
        }
        *slot = Some(record);
        Ok(())
    }

    fn complete_records(&self) -> Result<Vec<TrialRecord>, String> {
        if self.records.is_empty() {
            return Err("a case requires at least one trial".to_string());
        }
        self.records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                record.clone().ok_or_else(|| {
                    format!(
                        "missing trial index {index} for {}",
                        self.identity.case_key()
                    )
                })
            })
            .collect()
    }

    pub(crate) fn finish(
        self,
        iterations: usize,
        quality: QualitySummary,
    ) -> Result<ComparisonCase, String> {
        let records = self.complete_records()?;
        let metrics = records.iter().map(|r| &r.measurement).collect::<Vec<_>>();
        let distribution =
            |get: fn(&TrialMeasurement) -> f64| -> Result<TrialDistribution, String> {
                summarize_trials(metrics.iter().map(|m| get(m)).collect())
            };
        let cycles = |get: fn(&TrialMeasurement) -> Option<f64>| -> Result<Option<TrialDistribution>, String> {
            let values = metrics.iter().filter_map(|m| get(m)).collect::<Vec<_>>();
            if values.is_empty() {
                Ok(None)
            } else if values.len() != records.len() {
                Err("thread cycle counter is missing from some trials".to_string())
            } else {
                summarize_trials(values).map(Some)
            }
        };
        let counts = |get: fn(&TrialMeasurement) -> usize| metrics.iter().map(|m| get(m)).collect();
        let expected_warmup = CHUNK_FRAMES * WARMUP_BUFFERS;
        let expected_timed = CHUNK_FRAMES * iterations;
        let valid = quality.valid
            && metrics.iter().all(|m| {
                m.warmup_consumed_frames == expected_warmup
                    && m.timed_consumed_frames == expected_timed
                    && m.expected_complete_output_frames == m.actual_complete_output_frames
                    && m.warmup_output_frames
                        .checked_add(m.steady_output_frames)
                        .and_then(|n| n.checked_add(m.drain_frames))
                        == Some(m.actual_complete_output_frames)
                    && m.drain_terminal
            });
        let steady_ns_per_input_sample = distribution(|m| m.steady_ns_per_input_sample)?;
        let steady_ns_per_input_buffer = distribution(|m| m.steady_ns_per_input_buffer)?;
        let source_buffer_duration_ns =
            CHUNK_FRAMES as f64 / self.identity.from_rate_hz as f64 * 1.0e9;
        Ok(ComparisonCase {
            case_key: self.identity.case_key(),
            engine: self.engine,
            rate_pair: self.identity.rate_pair,
            from_rate_hz: self.identity.from_rate_hz,
            to_rate_hz: self.identity.to_rate_hz,
            channels: self.identity.channels,
            chunk_frames: self.identity.chunk_frames,
            output_capacity_frames: self.identity.output_capacity_frames,
            setup_ns: distribution(|m| m.setup_ns)?,
            median_source_realtime_utilization_pct: steady_ns_per_input_buffer.median
                / source_buffer_duration_ns
                * 100.0,
            p95_source_realtime_utilization_pct: steady_ns_per_input_buffer.p95
                / source_buffer_duration_ns
                * 100.0,
            steady_ns_per_input_sample,
            steady_ns_per_input_buffer,
            steady_cycles_per_input_sample: cycles(|m| m.steady_cycles_per_input_sample)?,
            steady_cycles_per_input_buffer: cycles(|m| m.steady_cycles_per_input_buffer)?,
            reset_ns: distribution(|m| m.reset_ns)?,
            drain_ns: distribution(|m| m.drain_ns)?,
            source_buffer_duration_ns,
            quality,
            work_validation: WorkValidation {
                classification: MetricClassification::Gate,
                valid,
                expected_warmup_consumed_frames_per_trial: expected_warmup,
                warmup_consumed_frames_per_trial: counts(|m| m.warmup_consumed_frames),
                expected_timed_consumed_frames_per_trial: expected_timed,
                timed_consumed_frames_per_trial: counts(|m| m.timed_consumed_frames),
                expected_complete_output_frames_per_trial: counts(|m| {
                    m.expected_complete_output_frames
                }),
                warmup_output_frames_per_trial: counts(|m| m.warmup_output_frames),
                steady_output_frames_per_trial: counts(|m| m.steady_output_frames),
                drain_frames_per_trial: counts(|m| m.drain_frames),
                actual_complete_output_frames_per_trial: counts(|m| {
                    m.actual_complete_output_frames
                }),
                drain_terminal_per_trial: metrics.iter().map(|m| m.drain_terminal).collect(),
            },
            trial_records: records,
            process_call_latency: None,
        })
    }
}

/// Schedule order changes; the map key never changes its engine identity.
/// This is the same collector used by the benchmark and the regression tests.
pub(crate) fn collect_scheduled_trials(
    accumulators: &mut BTreeMap<usize, CaseAccumulator>,
    trials: usize,
    mut measure: impl FnMut(usize, usize) -> Result<TrialRecord, String>,
) -> BTreeMap<usize, String> {
    let indices = accumulators.keys().copied().collect::<Vec<_>>();
    let mut failures = BTreeMap::new();
    for trial in 0..trials {
        let mut order = indices.clone();
        if !trial.is_multiple_of(2) {
            order.reverse();
        }
        for factory_index in order {
            if failures.contains_key(&factory_index) {
                continue;
            }
            let result = measure(factory_index, trial).and_then(|record| {
                if record.trial_index != trial {
                    return Err(format!(
                        "scheduled trial {trial} returned index {}",
                        record.trial_index
                    ));
                }
                accumulators
                    .get_mut(&factory_index)
                    .ok_or_else(|| format!("missing accumulator for factory {factory_index}"))?
                    .push(record)
            });
            if let Err(error) = result {
                failures.insert(factory_index, format!("trial {trial}: {error}"));
            }
        }
    }
    failures
}

/// Rebuild every published distribution and work counter from identified records.
/// In particular a stored `valid: true` cannot override corrupted work or identity.
pub(crate) fn validate_case_trials(
    case: &ComparisonCase,
    conditions: &ComparisonConditions,
) -> Result<(), String> {
    let rate = super::RATE_PAIRS
        .iter()
        .find(|rate| rate.id == case.rate_pair)
        .ok_or_else(|| format!("unknown rate pair {}", case.rate_pair))?;
    let capacity = *conditions
        .output_capacity_frames_by_rate
        .get(rate.id)
        .ok_or_else(|| format!("missing capacity for {}", rate.id))?;
    if case.trial_records.len() != conditions.trials {
        return Err(format!(
            "{} has {} records, expected {}",
            case.case_key,
            case.trial_records.len(),
            conditions.trials
        ));
    }
    let mut accumulator =
        CaseAccumulator::new(case.engine.clone(), *rate, capacity, conditions.trials);
    for record in &case.trial_records {
        accumulator.push(record.clone())?;
    }
    let mut reconstructed =
        accumulator.finish(conditions.iterations_per_trial, case.quality.clone())?;
    // The independent pass has its own raw-record validator.
    reconstructed.process_call_latency = case.process_call_latency.clone();
    if &reconstructed != case {
        return Err(format!(
            "{} published fields disagree with its identified raw trials",
            case.case_key
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(engine: &EngineIdentity, rate: RatePair, index: usize, marker: f64) -> TrialRecord {
        TrialRecord {
            identity: MeasurementIdentity::new(engine, rate, 2048),
            trial_index: index,
            measurement: TrialMeasurement {
                setup_ns: marker,
                steady_ns_per_input_sample: marker,
                steady_ns_per_input_buffer: marker,
                steady_cycles_per_input_sample: None,
                steady_cycles_per_input_buffer: None,
                reset_ns: marker,
                drain_ns: marker,
                warmup_consumed_frames: 1,
                timed_consumed_frames: 1,
                expected_complete_output_frames: marker as usize,
                warmup_output_frames: 0,
                steady_output_frames: marker as usize,
                drain_frames: 0,
                actual_complete_output_frames: marker as usize,
                drain_terminal: true,
            },
        }
    }

    #[test]
    fn persisted_trial_records_preserve_float_bits() {
        let rate = super::super::RATE_PAIRS[0];
        let engine = super::super::adapters::EngineFactory::silent_test()
            .identity()
            .clone();
        // Shortest decimal JSON must reconstruct the original binary values:
        // downstream integrity checks recompute derived statistics from them.
        for index in 0..512_u64 {
            let marker = f64::from_bits(0x4020_0000_0000_0000 + index * 1_000_000_007);
            for marker in [
                marker,
                marker / 11_609_977.324_263_038 * 100.0,
                marker * 1e9,
            ] {
                let original = record(&engine, rate, index as usize, marker);
                let json = serde_json::to_vec(&original).unwrap();
                let restored: TrialRecord = serde_json::from_slice(&json).unwrap();
                assert_eq!(
                    original, restored,
                    "trial {index} changed during JSON reload"
                );
            }
        }
    }

    #[test]
    fn alternating_schedule_keeps_sparse_factory_identities_and_all_trials() {
        let rate = super::super::RATE_PAIRS[4];
        let base = super::super::adapters::EngineFactory::silent_test()
            .identity()
            .clone();
        let engines = [0, 2, 7]
            .map(|index| {
                let mut engine = base.clone();
                engine.engine_id = format!("engine_{index}");
                (index, engine)
            })
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let mut cases = engines
            .iter()
            .map(|(&index, engine)| (index, CaseAccumulator::new(engine.clone(), rate, 2048, 4)))
            .collect::<BTreeMap<_, _>>();
        let mut schedule = Vec::new();
        let failures = collect_scheduled_trials(&mut cases, 4, |index, trial| {
            schedule.push((index, trial));
            Ok(record(
                &engines[&index],
                rate,
                trial,
                (index * 100 + trial + 1) as f64,
            ))
        });
        assert!(failures.is_empty());
        assert_eq!(
            schedule,
            vec![
                (0, 0),
                (2, 0),
                (7, 0),
                (7, 1),
                (2, 1),
                (0, 1),
                (0, 2),
                (2, 2),
                (7, 2),
                (7, 3),
                (2, 3),
                (0, 3)
            ]
        );
        for (index, case) in cases {
            let records = case.complete_records().unwrap();
            for (trial, record) in records.iter().enumerate() {
                assert_eq!(record.identity.engine_id, format!("engine_{index}"));
                assert_eq!(record.trial_index, trial);
                assert_eq!(
                    record.measurement.actual_complete_output_frames,
                    index * 100 + trial + 1
                );
            }
        }
    }

    #[test]
    fn rejects_missing_duplicate_out_of_range_and_wrong_identity_trials() {
        let rate = super::super::RATE_PAIRS[0];
        let engine = super::super::adapters::EngineFactory::silent_test()
            .identity()
            .clone();
        let original = record(&engine, rate, 0, 1.0);
        let mut case = CaseAccumulator::new(engine, rate, 2048, 2);
        assert!(case
            .complete_records()
            .unwrap_err()
            .contains("missing trial"));
        case.push(original.clone()).unwrap();
        assert!(case
            .push(original.clone())
            .unwrap_err()
            .contains("duplicate"));
        let mut invalid = original.clone();
        invalid.trial_index = 2;
        assert!(case.push(invalid).unwrap_err().contains("outside"));
        let mutations: [fn(&mut MeasurementIdentity); 9] = [
            |i| i.engine_id.push('x'),
            |i| i.algorithm_id.push('x'),
            |i| i.sample_format = SampleFormat::InterleavedF32,
            |i| i.rate_pair.push('x'),
            |i| i.from_rate_hz += 1,
            |i| i.to_rate_hz += 1,
            |i| i.channels += 1,
            |i| i.chunk_frames += 1,
            |i| i.output_capacity_frames += 1,
        ];
        for mutate in mutations {
            let mut invalid = original.clone();
            invalid.trial_index = 1;
            mutate(&mut invalid.identity);
            assert!(case
                .push(invalid)
                .unwrap_err()
                .contains("identity mismatch"));
        }
        let mut final_record = original;
        final_record.trial_index = 1;
        case.push(final_record).unwrap();
        assert_eq!(case.complete_records().unwrap().len(), 2);
    }
}
