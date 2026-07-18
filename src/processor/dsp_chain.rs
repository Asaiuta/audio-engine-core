//! DSP Processing Chain
//!
//! Manages a collection of audio processors in a pipeline.
//! Provides:
//! - Guaranteed continuous processing (no lock-induced skips)
//! - Unified statistics and debugging
//! - Easy dynamic configuration
//!
//! # Architecture
//!
//! ```text
//! Input Buffer
//!      │
//!      ▼
//! ┌─────────────────────────────────────────────────────┐
//! │                    DspChain                          │
//! │                                                      │
//! │  ┌──────────┐   ┌──────────┐   ┌──────────┐        │
//! │  │    EQ    │ → │ Saturation│ → │ Crossfeed│ → ...  │
//! │  └──────────┘   └──────────┘   └──────────┘        │
//! │                                                      │
//! │  Each processor:                                     │
//! │  - Reads lock-free params                           │
//! │  - Processes without blocking                       │
//! │  - Never skips due to contention                    │
//! │                                                      │
//! └─────────────────────────────────────────────────────┘
//!      │
//!      ▼
//! Output Buffer
//! ```

use super::traits::{
    finish_checked, process_checked, AudioBlockMut, FrameDuration, ProcessBuffers, ProcessError,
    ProcessProgress, ProcessState, StreamingProcessor, TailSpec,
};

/// Policy used by [`DspChain::finish`] for processors with asymptotic tails.
///
/// The policy is copied into the chain on the first finish call. Keeping the
/// values in frames makes the callback path independent of wall-clock units and
/// avoids per-call allocation or policy object ownership.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChainFinishPolicy {
    pub energy_threshold_dbfs: f64,
    pub silence_hold_frames: usize,
    pub max_tail_frames: usize,
}

impl ChainFinishPolicy {
    pub const fn new(
        energy_threshold_dbfs: f64,
        silence_hold_frames: usize,
        max_tail_frames: usize,
    ) -> Self {
        Self {
            energy_threshold_dbfs,
            silence_hold_frames,
            max_tail_frames,
        }
    }

    fn validate(self) -> Result<Self, ProcessError> {
        if !self.energy_threshold_dbfs.is_finite() || self.energy_threshold_dbfs > 0.0 {
            return Err(ProcessError::InvalidRenderPolicy {
                message: "chain finish energy threshold must be finite and no greater than 0 dBFS",
            });
        }
        if self.silence_hold_frames == 0 {
            return Err(ProcessError::InvalidRenderPolicy {
                message: "chain finish silence hold must be greater than zero",
            });
        }
        if self.max_tail_frames < self.silence_hold_frames {
            return Err(ProcessError::InvalidRenderPolicy {
                message: "chain finish maximum tail must be at least the silence hold",
            });
        }
        Ok(self)
    }

    fn threshold_power(self) -> f64 {
        // RMS dBFS is power-domain; no input-dependent work occurs here.
        10.0_f64.powf(self.energy_threshold_dbfs / 10.0)
    }
}

impl Default for ChainFinishPolicy {
    fn default() -> Self {
        Self::new(-120.0, 12_000, 1_440_000)
    }
}

/// DSP processing chain
///
/// Manages multiple audio processors in sequence.
/// All processors share the same buffer, processed in-place.
pub struct DspChain {
    /// Processors in execution order
    processors: Vec<Box<dyn StreamingProcessor>>,
    sample_rate_hz: u32,
    finish_stage: usize,
    finish_complete: bool,
    finish_policy: Option<ChainFinishPolicy>,
    finish_threshold_power: f64,
    finish_unknown_stage: Option<usize>,
    finish_observation_end_stage: usize,
    finish_protected_frames: usize,
    finish_quiet_frames: usize,
    finish_generated_frames: usize,
    finish_capped: bool,
}

impl DspChain {
    /// Create an empty DSP chain
    pub fn new(_sample_rate_hz: u32) -> Self {
        Self {
            processors: Vec::new(),
            sample_rate_hz: _sample_rate_hz,
            ..Self::empty_state()
        }
    }

    /// Create a chain with pre-allocated capacity
    pub fn with_capacity(capacity: usize, _sample_rate_hz: u32) -> Self {
        Self {
            processors: Vec::with_capacity(capacity),
            sample_rate_hz: _sample_rate_hz,
            ..Self::empty_state()
        }
    }

    const fn empty_state() -> Self {
        Self {
            processors: Vec::new(),
            sample_rate_hz: 0,
            finish_stage: 0,
            finish_complete: false,
            finish_policy: None,
            finish_threshold_power: 0.0,
            finish_unknown_stage: None,
            finish_observation_end_stage: 0,
            finish_protected_frames: 0,
            finish_quiet_frames: 0,
            finish_generated_frames: 0,
            finish_capped: false,
        }
    }

    /// Add a processor to the end of the chain
    pub fn add<P: StreamingProcessor + 'static>(&mut self, processor: P) -> &mut Self {
        self.processors.push(Box::new(processor));
        self
    }

    /// Process audio through all processors
    ///
    /// # Key Properties
    ///
    /// 1. **Continuous**: Never skips processors due to lock contention
    /// 2. **In-place**: Modifies buffer directly
    /// 3. **Lock-free**: All parameter updates use atomic operations
    ///
    /// # Arguments
    ///
    /// * `buffer` - Interleaved audio samples [L, R, L, R, ...]
    /// * `channels` - Number of audio channels
    pub fn process(
        &mut self,
        buffer: &mut [f64],
        channels: usize,
    ) -> Result<ProcessProgress, ProcessError> {
        if self.finish_policy.is_some() || self.finish_stage > 0 || self.finish_complete {
            return Err(ProcessError::AlreadyFinished {
                processor: "DspChain",
            });
        }
        let mut block = AudioBlockMut::new(buffer, channels)?;
        let frames = block.frames();
        let mut all_bypassed = true;

        for processor in &mut self.processors {
            let progress = process_checked(
                processor.as_mut(),
                ProcessBuffers::in_place(block.reborrow()),
            )?;
            all_bypassed &= progress.is_bypassed();
        }

        Ok(
            ProcessProgress::new(frames, frames, ProcessState::NeedInput)
                .with_bypassed(all_bypassed),
        )
    }

    /// Drain the chain's callback-facing tail into caller-owned output.
    ///
    /// Fixed 1:1 downstream stages are applied to each upstream finish block
    /// before it is returned. The loop is bounded by the stage count and the
    /// caller's output capacity; asymptotic tails are stopped by the selected
    /// energy/hold/cap policy.
    pub fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        self.finish_with_policy(output, ChainFinishPolicy::default())
    }

    /// Drain with an explicit unknown-tail policy. The first call locks the
    /// policy for the current stream; callers must reset before changing it.
    pub fn finish_with_policy(
        &mut self,
        output: AudioBlockMut<'_>,
        policy: ChainFinishPolicy,
    ) -> Result<ProcessProgress, ProcessError> {
        if self.finish_complete {
            return Ok(ProcessProgress::finished(0));
        }
        if self.finish_policy.is_none() {
            let policy = policy.validate()?;
            self.finish_threshold_power = policy.threshold_power();
            self.finish_policy = Some(policy);
        }

        if self.processors.is_empty() {
            self.finish_complete = true;
            return Ok(ProcessProgress::finished(0));
        }

        let channels = output.channels();
        let capacity = output.frames();
        if capacity == 0 {
            return Ok(ProcessProgress::new(0, 0, ProcessState::NeedOutput));
        }

        let mut output = output;
        let output_samples = output.samples_mut();
        let mut produced_total = 0usize;

        while self.finish_stage < self.processors.len() && produced_total < capacity {
            let stage_index = self.finish_stage;
            let stage_unknown = matches!(
                self.processors[stage_index].tail(),
                TailSpec::Unknown | TailSpec::Infinite
            );
            if stage_unknown && self.finish_unknown_stage != Some(stage_index) {
                self.finish_unknown_stage = Some(stage_index);
                self.finish_observation_end_stage =
                    self.tail_energy_observation_end_stage(stage_index);
                self.finish_protected_frames = self.downstream_finish_protected_frames(
                    stage_index,
                    self.finish_observation_end_stage,
                )?;
                self.finish_quiet_frames = 0;
                self.finish_generated_frames = 0;
            }

            let mut remaining_frames = capacity - produced_total;
            if stage_unknown {
                let policy = self.finish_policy.ok_or(ProcessError::Backend {
                    processor: "DspChain",
                    operation: "finish",
                    message: "finish policy was not initialized",
                })?;
                let remaining_observed = policy
                    .max_tail_frames
                    .saturating_sub(self.finish_generated_frames);
                let remaining_quiet = policy
                    .silence_hold_frames
                    .saturating_sub(self.finish_quiet_frames);
                let allowed = self
                    .finish_protected_frames
                    .saturating_add(remaining_observed.min(remaining_quiet));
                if allowed == 0 {
                    self.finish_capped = true;
                    self.finish_stage += 1;
                    self.finish_unknown_stage = None;
                    self.finish_observation_end_stage = 0;
                    self.finish_protected_frames = 0;
                    self.finish_quiet_frames = 0;
                    self.finish_generated_frames = 0;
                    continue;
                }
                remaining_frames = remaining_frames.min(allowed);
            }
            let start_sample = produced_total * channels;
            let end_sample = start_sample + remaining_frames * channels;
            let finish_progress = {
                let block =
                    AudioBlockMut::new(&mut output_samples[start_sample..end_sample], channels)?;
                finish_checked(self.processors[stage_index].as_mut(), block)?
            };
            let produced = finish_progress.produced_frames();

            if produced > 0 {
                let segment_end = start_sample + produced * channels;
                let mut observation = None;
                if stage_unknown && self.finish_observation_end_stage <= stage_index + 1 {
                    observation = Some(self.observe_finish_energy(
                        &output_samples[start_sample..segment_end],
                        channels,
                        produced,
                    ));
                }
                for downstream_index in (stage_index + 1)..self.processors.len() {
                    if stage_unknown
                        && observation.is_none()
                        && downstream_index == self.finish_observation_end_stage
                    {
                        observation = Some(self.observe_finish_energy(
                            &output_samples[start_sample..segment_end],
                            channels,
                            produced,
                        ));
                    }
                    let block = AudioBlockMut::new(
                        &mut output_samples[start_sample..segment_end],
                        channels,
                    )?;
                    let _ = process_checked(
                        self.processors[downstream_index].as_mut(),
                        ProcessBuffers::in_place(block),
                    )?;
                }

                if stage_unknown {
                    let (quiet, observed_frames) = observation.unwrap_or_else(|| {
                        self.observe_finish_energy(
                            &output_samples[start_sample..segment_end],
                            channels,
                            produced,
                        )
                    });
                    self.finish_generated_frames =
                        self.finish_generated_frames.saturating_add(observed_frames);
                    let policy = match self.finish_policy {
                        Some(policy) => policy,
                        None => {
                            return Err(ProcessError::Backend {
                                processor: "DspChain",
                                operation: "finish",
                                message: "finish policy was not initialized",
                            });
                        }
                    };
                    if quiet || self.finish_generated_frames >= policy.max_tail_frames {
                        self.finish_capped |= !quiet;
                        self.finish_stage += 1;
                        self.finish_unknown_stage = None;
                        self.finish_observation_end_stage = 0;
                        self.finish_protected_frames = 0;
                        self.finish_quiet_frames = 0;
                        self.finish_generated_frames = 0;
                    }
                }
            }

            produced_total += produced;

            if finish_progress.state() == ProcessState::NeedOutput
                && self.finish_stage == stage_index
            {
                // Unknown tails may receive a sub-block bounded by the quiet
                // hold still needed. If signal energy reset the hold, consume
                // another bounded segment while caller capacity remains.
                if produced_total < capacity {
                    continue;
                }
                break;
            }
            if finish_progress.state() == ProcessState::Finished && self.finish_stage == stage_index
            {
                self.finish_stage += 1;
                self.finish_unknown_stage = None;
                self.finish_observation_end_stage = 0;
                self.finish_protected_frames = 0;
                self.finish_quiet_frames = 0;
                self.finish_generated_frames = 0;
            }
        }

        if self.finish_stage >= self.processors.len() {
            self.finish_complete = true;
            return Ok(ProcessProgress::finished(produced_total));
        }
        if produced_total == capacity {
            return Ok(ProcessProgress::new(
                0,
                produced_total,
                ProcessState::NeedOutput,
            ));
        }

        // A zero-output non-terminal stage should not occur under the shared
        // finish contract. Return a typed backend error instead of spinning.
        Err(ProcessError::Backend {
            processor: "DspChain",
            operation: "finish",
            message: "stage made no finish progress",
        })
    }

    fn tail_energy_observation_end_stage(&self, stage_index: usize) -> usize {
        self.processors
            .iter()
            .enumerate()
            .skip(stage_index + 1)
            .find(|(_, processor)| processor.tail_energy_observation_barrier())
            .map(|(index, _)| index)
            .unwrap_or(self.processors.len())
    }

    fn downstream_finish_protected_frames(
        &self,
        stage_index: usize,
        observation_end_stage: usize,
    ) -> Result<usize, ProcessError> {
        let mut total = 0.0;
        for processor in &self.processors[stage_index..observation_end_stage] {
            total += processor
                .latency()
                .frames_at_rate_f64(self.sample_rate_hz)?;
            if let TailSpec::Finite(tail) = processor.tail() {
                total += tail.frames_at_rate_f64(self.sample_rate_hz)?;
            }
        }
        if !total.is_finite() || total < 0.0 || total > usize::MAX as f64 {
            return Err(ProcessError::Backend {
                processor: "DspChain",
                operation: "compose finish timing",
                message: "downstream finish protection exceeds the frame domain",
            });
        }
        Ok(total.ceil() as usize)
    }

    fn observe_finish_energy(
        &mut self,
        samples: &[f64],
        channels: usize,
        frames: usize,
    ) -> (bool, usize) {
        let Some(policy) = self.finish_policy else {
            return (false, 0);
        };
        let protected = self.finish_protected_frames.min(frames);
        self.finish_protected_frames -= protected;
        let observed_frames = frames - protected;
        if observed_frames == 0 {
            return (false, 0);
        }

        for frame in protected..frames {
            let start = frame * channels;
            let end = start + channels;
            let mut power = 0.0;
            for &sample in &samples[start..end] {
                power += sample * sample;
            }
            power /= channels.max(1) as f64;
            if !power.is_finite() || power > self.finish_threshold_power {
                self.finish_quiet_frames = 0;
            } else {
                self.finish_quiet_frames = self.finish_quiet_frames.saturating_add(1);
            }
        }
        (
            self.finish_quiet_frames >= policy.silence_hold_frames,
            observed_frames,
        )
    }

    /// Reset all processors, returning the first error after attempting every stage.
    pub fn reset(&mut self) -> Result<(), ProcessError> {
        let mut first_error = None;
        for processor in &mut self.processors {
            if let Err(error) = processor.reset() {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        self.finish_stage = 0;
        self.finish_complete = false;
        self.finish_policy = None;
        self.finish_threshold_power = 0.0;
        self.finish_unknown_stage = None;
        self.finish_observation_end_stage = 0;
        self.finish_protected_frames = 0;
        self.finish_quiet_frames = 0;
        self.finish_generated_frames = 0;
        self.finish_capped = false;
        first_error.map_or(Ok(()), Err)
    }

    /// Update sample rate for all processors, returning the first stage error.
    pub fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        if sample_rate_hz == 0 {
            return Err(ProcessError::InvalidSampleRate {
                processor: "DspChain",
                sample_rate_hz,
            });
        }

        let mut first_error = None;
        for processor in &mut self.processors {
            if let Err(error) = processor.set_sample_rate(sample_rate_hz) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        self.sample_rate_hz = sample_rate_hz;
        self.finish_stage = 0;
        self.finish_complete = false;
        self.finish_policy = None;
        self.finish_threshold_power = 0.0;
        self.finish_unknown_stage = None;
        self.finish_observation_end_stage = 0;
        self.finish_protected_frames = 0;
        self.finish_quiet_frames = 0;
        self.finish_generated_frames = 0;
        self.finish_capped = false;
        first_error.map_or(Ok(()), Err)
    }

    /// Composed algorithmic latency in the chain's fixed sample-rate domain.
    pub fn latency(&self) -> FrameDuration {
        let total = self
            .processors
            .iter()
            .filter_map(|processor| {
                processor
                    .latency()
                    .frames_at_rate_f64(self.sample_rate_hz)
                    .ok()
            })
            .sum::<f64>();
        if !total.is_finite() || total < 0.0 || total == 0.0 || self.sample_rate_hz == 0 {
            return FrameDuration::ZERO;
        }
        match FrameDuration::new(total.round() as usize, self.sample_rate_hz) {
            Ok(duration) => duration,
            Err(_) => FrameDuration::ZERO,
        }
    }

    /// Composed semantic tail. Unknown/infinite tails dominate finite sums.
    pub fn tail(&self) -> TailSpec {
        let mut finite_frames = 0.0_f64;
        for processor in &self.processors {
            match processor.tail() {
                TailSpec::None => {}
                TailSpec::Unknown => return TailSpec::Unknown,
                TailSpec::Infinite => return TailSpec::Infinite,
                TailSpec::Finite(duration) => {
                    let Some(frames) = duration
                        .frames_at_rate_f64(self.sample_rate_hz)
                        .ok()
                        .filter(|frames| frames.is_finite() && *frames >= 0.0)
                    else {
                        return TailSpec::Unknown;
                    };
                    finite_frames += frames;
                }
            }
        }
        if !finite_frames.is_finite() || finite_frames > usize::MAX as f64 {
            return TailSpec::Unknown;
        }
        TailSpec::finite(finite_frames.ceil() as usize, self.sample_rate_hz)
            .unwrap_or(TailSpec::Unknown)
    }

    /// Whether an unknown tail reached the configured safety cap.
    pub fn finish_was_capped(&self) -> bool {
        self.finish_capped
    }

    /// Get number of processors
    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// Return processor names in execution order.
    ///
    /// This is intended for setup-time diagnostics and tests. It allocates the
    /// returned vector, so do not call it from the realtime callback.
    pub fn processor_names(&self) -> Vec<&'static str> {
        self.processors
            .iter()
            .map(|processor| processor.name())
            .collect()
    }

    /// Check if chain is empty
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }

    /// Clear all processors
    pub fn clear(&mut self) {
        self.processors.clear();
        self.finish_stage = 0;
        self.finish_complete = false;
        self.finish_policy = None;
        self.finish_threshold_power = 0.0;
        self.finish_unknown_stage = None;
        self.finish_observation_end_stage = 0;
        self.finish_protected_frames = 0;
        self.finish_quiet_frames = 0;
        self.finish_generated_frames = 0;
        self.finish_capped = false;
    }
}

impl Default for DspChain {
    fn default() -> Self {
        Self::new(44_100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processor::traits::ProcessBufferParts;

    fn process_test_stage<F>(
        enabled: bool,
        buffers: ProcessBuffers<'_>,
        mut apply: F,
    ) -> Result<ProcessProgress, ProcessError>
    where
        F: FnMut(&mut f64),
    {
        let progress = match buffers.into_parts() {
            super::super::traits::ProcessBufferParts::InPlace(mut block) => {
                if enabled {
                    block.samples_mut().iter_mut().for_each(&mut apply);
                }
                ProcessProgress::new(block.frames(), block.frames(), ProcessState::NeedInput)
                    .with_bypassed(!enabled)
            }
            super::super::traits::ProcessBufferParts::OutOfPlace { input, mut output } => {
                let frames = input.frames().min(output.frames());
                let samples = frames * input.channels();
                output.samples_mut()[..samples].copy_from_slice(&input.samples()[..samples]);
                if enabled {
                    output.samples_mut()[..samples]
                        .iter_mut()
                        .for_each(&mut apply);
                }
                let state = if frames < input.frames() {
                    ProcessState::NeedOutput
                } else {
                    ProcessState::NeedInput
                };
                ProcessProgress::new(frames, frames, state).with_bypassed(!enabled)
            }
        };
        Ok(progress)
    }

    // Test processor that doubles samples
    struct DoublerProcessor {
        enabled: bool,
        processed_count: u64,
    }

    impl DoublerProcessor {
        fn new() -> Self {
            Self {
                enabled: true,
                processed_count: 0,
            }
        }
    }

    impl StreamingProcessor for DoublerProcessor {
        fn name(&self) -> &'static str {
            "Doubler"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            let progress = process_test_stage(self.enabled, buffers, |sample| *sample *= 2.0)?;
            if self.enabled {
                self.processed_count += 1;
            }
            Ok(progress)
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            self.processed_count = 0;
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn set_enabled(&mut self, enabled: bool) {
            self.enabled = enabled;
        }
    }

    // Test processor that adds 1.0
    struct AdderProcessor {
        enabled: bool,
    }

    impl AdderProcessor {
        fn new() -> Self {
            Self { enabled: true }
        }
    }

    impl StreamingProcessor for AdderProcessor {
        fn name(&self) -> &'static str {
            "Adder"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            process_test_stage(self.enabled, buffers, |sample| *sample += 1.0)
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn set_enabled(&mut self, enabled: bool) {
            self.enabled = enabled;
        }
    }

    #[test]
    fn test_empty_chain() {
        let mut chain = DspChain::new(44_100);
        let mut buffer = vec![1.0, 2.0, 3.0];
        let progress = chain.process(&mut buffer, 1).unwrap();
        assert_eq!(buffer, vec![1.0, 2.0, 3.0]);
        assert_eq!(progress.consumed_frames(), 3);
        assert_eq!(progress.produced_frames(), 3);
        assert!(progress.is_bypassed());
    }

    #[test]
    fn test_single_processor() {
        let mut chain = DspChain::new(44_100);
        chain.add(DoublerProcessor::new());

        let mut buffer = vec![1.0, 2.0, 3.0];
        let _ = chain.process(&mut buffer, 1).unwrap();

        assert_eq!(buffer, vec![2.0, 4.0, 6.0]);
    }

    #[test]
    fn test_chain_order() {
        let mut chain = DspChain::new(44_100);
        chain.add(DoublerProcessor::new()); // Doubles first
        chain.add(AdderProcessor::new()); // Then adds 1

        // Start with 1.0 -> 2.0 (double) -> 3.0 (add 1)
        let mut buffer = vec![1.0];
        let _ = chain.process(&mut buffer, 1).unwrap();
        assert_eq!(buffer, vec![3.0]);
    }

    #[test]
    fn test_processor_names_follow_execution_order() {
        let mut chain = DspChain::new(44_100);
        chain.add(DoublerProcessor::new());
        chain.add(AdderProcessor::new());

        assert_eq!(chain.processor_names(), vec!["Doubler", "Adder"]);
    }

    #[test]
    fn test_bypassed_processor() {
        let mut chain = DspChain::new(44_100);
        let mut doubler = DoublerProcessor::new();
        doubler.set_enabled(false);
        chain.add(doubler);

        let mut buffer = vec![5.0];
        let progress = chain.process(&mut buffer, 1).unwrap();

        // Should be unchanged (bypassed)
        assert_eq!(buffer, vec![5.0]);
        assert!(progress.is_bypassed());
    }

    #[test]
    fn test_reset() {
        let mut chain = DspChain::new(44_100);
        chain.add(DoublerProcessor::new());

        let mut buffer = vec![1.0; 100];
        let _ = chain.process(&mut buffer, 1).unwrap();
        chain.reset().unwrap();
    }

    #[test]
    fn process_rejects_invalid_interleaved_shape() {
        let mut chain = DspChain::new(44_100);
        let mut buffer = [0.0; 3];

        assert!(matches!(
            chain.process(&mut buffer, 2),
            Err(ProcessError::InvalidBlock(_))
        ));
    }

    #[test]
    fn set_sample_rate_rejects_zero() {
        let mut chain = DspChain::new(44_100);
        assert_eq!(
            chain.set_sample_rate(0),
            Err(ProcessError::InvalidSampleRate {
                processor: "DspChain",
                sample_rate_hz: 0,
            })
        );
    }

    #[test]
    fn steady_state_process_is_allocation_free() {
        let mut chain = DspChain::new(48_000);
        chain.add(DoublerProcessor::new());
        let mut buffer = [0.25; 512 * 2];
        let _ = chain.process(&mut buffer, 2).unwrap();

        assert_no_alloc::assert_no_alloc(|| {
            let _ = chain.process(&mut buffer, 2).unwrap();
        });
    }

    struct UnknownDecay {
        state: f64,
        finished: bool,
    }

    struct UnknownImpulseThenSilence {
        generated_frames: usize,
    }

    impl StreamingProcessor for UnknownImpulseThenSilence {
        fn name(&self) -> &'static str {
            "UnknownImpulseThenSilence"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            match buffers.into_parts() {
                ProcessBufferParts::InPlace(block) => Ok(ProcessProgress::new(
                    block.frames(),
                    block.frames(),
                    ProcessState::NeedInput,
                )),
                ProcessBufferParts::OutOfPlace { .. } => Err(ProcessError::UnsupportedBufferMode {
                    processor: self.name(),
                    mode: super::super::traits::ProcessBufferMode::OutOfPlace,
                }),
            }
        }

        fn finish(
            &mut self,
            mut output: AudioBlockMut<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            let channels = output.channels();
            for frame in output.samples_mut().chunks_exact_mut(channels) {
                let sample = if self.generated_frames == 0 { 1.0 } else { 0.0 };
                frame.fill(sample);
                self.generated_frames += 1;
            }
            Ok(ProcessProgress::new(
                0,
                output.frames(),
                ProcessState::NeedOutput,
            ))
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            self.generated_frames = 0;
            Ok(())
        }

        fn tail(&self) -> TailSpec {
            TailSpec::Unknown
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn set_enabled(&mut self, _enabled: bool) {}
    }

    impl StreamingProcessor for UnknownDecay {
        fn name(&self) -> &'static str {
            "UnknownDecay"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            match buffers.into_parts() {
                ProcessBufferParts::InPlace(mut block) => {
                    if self.finished {
                        return Err(ProcessError::AlreadyFinished {
                            processor: self.name(),
                        });
                    }
                    for sample in block.samples_mut() {
                        self.state = *sample;
                    }
                    Ok(ProcessProgress::new(
                        block.frames(),
                        block.frames(),
                        ProcessState::NeedInput,
                    ))
                }
                ProcessBufferParts::OutOfPlace { .. } => Err(ProcessError::UnsupportedBufferMode {
                    processor: self.name(),
                    mode: super::super::traits::ProcessBufferMode::OutOfPlace,
                }),
            }
        }

        fn finish(
            &mut self,
            mut output: AudioBlockMut<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            if self.finished {
                return Ok(ProcessProgress::finished(0));
            }
            for sample in output.samples_mut() {
                *sample = self.state;
            }
            self.state *= 0.5;
            Ok(ProcessProgress::new(
                0,
                output.frames(),
                ProcessState::NeedOutput,
            ))
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            self.state = 0.0;
            self.finished = false;
            Ok(())
        }

        fn tail(&self) -> TailSpec {
            TailSpec::Unknown
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn set_enabled(&mut self, _enabled: bool) {}
    }

    struct GainProcessor;

    impl StreamingProcessor for GainProcessor {
        fn name(&self) -> &'static str {
            "Gain"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            match buffers.into_parts() {
                ProcessBufferParts::InPlace(mut block) => {
                    for sample in block.samples_mut() {
                        *sample *= 2.0;
                    }
                    Ok(ProcessProgress::new(
                        block.frames(),
                        block.frames(),
                        ProcessState::NeedInput,
                    ))
                }
                ProcessBufferParts::OutOfPlace { .. } => Err(ProcessError::UnsupportedBufferMode {
                    processor: self.name(),
                    mode: super::super::traits::ProcessBufferMode::OutOfPlace,
                }),
            }
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn set_enabled(&mut self, _enabled: bool) {}
    }

    struct TerminalNoise {
        level: f64,
    }

    struct FractionalTail(FrameDuration);

    impl StreamingProcessor for FractionalTail {
        fn name(&self) -> &'static str {
            "FractionalTail"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            match buffers.into_parts() {
                ProcessBufferParts::InPlace(block) => Ok(ProcessProgress::new(
                    block.frames(),
                    block.frames(),
                    ProcessState::NeedInput,
                )),
                ProcessBufferParts::OutOfPlace { .. } => Err(ProcessError::UnsupportedBufferMode {
                    processor: self.name(),
                    mode: super::super::traits::ProcessBufferMode::OutOfPlace,
                }),
            }
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            Ok(())
        }

        fn tail(&self) -> TailSpec {
            TailSpec::Finite(self.0)
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn set_enabled(&mut self, _enabled: bool) {}
    }

    impl StreamingProcessor for TerminalNoise {
        fn name(&self) -> &'static str {
            "TerminalNoise"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            match buffers.into_parts() {
                ProcessBufferParts::InPlace(mut block) => {
                    for sample in block.samples_mut() {
                        *sample += self.level;
                    }
                    Ok(ProcessProgress::new(
                        block.frames(),
                        block.frames(),
                        ProcessState::NeedInput,
                    ))
                }
                ProcessBufferParts::OutOfPlace { .. } => Err(ProcessError::UnsupportedBufferMode {
                    processor: self.name(),
                    mode: super::super::traits::ProcessBufferMode::OutOfPlace,
                }),
            }
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn tail_energy_observation_barrier(&self) -> bool {
            true
        }

        fn set_enabled(&mut self, _enabled: bool) {}
    }

    struct LateFinitePulse {
        delay_frames: usize,
        remaining_frames: usize,
        pulse: f64,
        armed: bool,
    }

    impl StreamingProcessor for LateFinitePulse {
        fn name(&self) -> &'static str {
            "LateFinitePulse"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            match buffers.into_parts() {
                ProcessBufferParts::InPlace(mut block) => {
                    for sample in block.samples_mut() {
                        if !self.armed {
                            self.pulse = *sample;
                            self.remaining_frames = self.delay_frames;
                            self.armed = true;
                            *sample = 0.0;
                        } else if self.remaining_frames > 0 {
                            self.remaining_frames -= 1;
                            *sample = if self.remaining_frames == 0 {
                                self.pulse
                            } else {
                                0.0
                            };
                        } else {
                            *sample = 0.0;
                        }
                    }
                    Ok(ProcessProgress::new(
                        block.frames(),
                        block.frames(),
                        ProcessState::NeedInput,
                    ))
                }
                ProcessBufferParts::OutOfPlace { .. } => Err(ProcessError::UnsupportedBufferMode {
                    processor: self.name(),
                    mode: super::super::traits::ProcessBufferMode::OutOfPlace,
                }),
            }
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            self.remaining_frames = 0;
            self.pulse = 0.0;
            self.armed = false;
            Ok(())
        }

        fn tail(&self) -> TailSpec {
            TailSpec::finite(self.delay_frames, 48_000).unwrap()
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn set_enabled(&mut self, _enabled: bool) {}
    }

    #[test]
    fn finish_drives_unknown_tail_through_downstream_without_scratch() {
        let mut chain = DspChain::with_capacity(2, 48_000);
        chain.add(UnknownDecay {
            state: 0.0,
            finished: false,
        });
        chain.add(GainProcessor);

        let mut input = [1.0_f64];
        let _ = chain.process(&mut input, 1).unwrap();

        let policy = ChainFinishPolicy::new(-6.0, 2, 20);
        let mut output = [0.0_f64; 4];
        let mut first = None;
        assert_no_alloc::assert_no_alloc(|| {
            first = Some(
                chain
                    .finish_with_policy(AudioBlockMut::new(&mut output, 1).unwrap(), policy)
                    .unwrap(),
            );
        });
        let first = first.unwrap();
        assert_eq!(first, ProcessProgress::new(0, 4, ProcessState::NeedOutput));
        assert_eq!(output, [2.0, 2.0, 1.0, 1.0]);

        let mut terminal_output = [9.0_f64; 4];
        let terminal = chain
            .finish_with_policy(AudioBlockMut::new(&mut terminal_output, 1).unwrap(), policy)
            .unwrap();
        assert_eq!(terminal, ProcessProgress::finished(2));
        assert_eq!(terminal_output[..2], [0.5, 0.5]);
        assert_eq!(
            chain
                .finish_with_policy(AudioBlockMut::new(&mut terminal_output, 1).unwrap(), policy,)
                .unwrap(),
            ProcessProgress::finished(0)
        );
    }

    #[test]
    fn unknown_tail_stops_at_hold_boundary_independent_of_output_capacity() {
        fn render(output_frames: usize) -> (Vec<f64>, bool) {
            let mut chain = DspChain::new(48_000);
            chain.add(UnknownImpulseThenSilence {
                generated_frames: 0,
            });
            let policy = ChainFinishPolicy::new(-80.0, 3, 20);
            let mut scratch = vec![9.0; output_frames];
            let mut rendered = Vec::new();
            loop {
                let progress = chain
                    .finish_with_policy(AudioBlockMut::new(&mut scratch, 1).unwrap(), policy)
                    .unwrap();
                rendered.extend_from_slice(&scratch[..progress.produced_frames()]);
                if progress.state() == ProcessState::Finished {
                    break;
                }
            }
            (rendered, chain.finish_was_capped())
        }

        let one_frame = render(1);
        let large_block = render(64);
        assert_eq!(one_frame, large_block);
        assert_eq!(large_block.0, [1.0, 0.0, 0.0, 0.0]);
        assert!(!large_block.1);
    }

    #[test]
    fn unknown_tail_waits_for_downstream_finite_support_before_energy_stop() {
        let mut chain = DspChain::with_capacity(2, 48_000);
        chain.add(UnknownDecay {
            state: 0.0,
            finished: false,
        });
        chain.add(LateFinitePulse {
            delay_frames: 6,
            remaining_frames: 0,
            pulse: 0.0,
            armed: false,
        });
        let mut input = [1.0_f64];
        let _ = chain.process(&mut input, 1).unwrap();

        let policy = ChainFinishPolicy::new(-80.0, 2, 20);
        let mut rendered = Vec::new();
        let mut output = [0.0_f64; 4];
        for _ in 0..16 {
            let progress = chain
                .finish_with_policy(AudioBlockMut::new(&mut output, 1).unwrap(), policy)
                .unwrap();
            rendered.extend_from_slice(&output[..progress.produced_frames()]);
            if progress.state() == ProcessState::Finished {
                break;
            }
        }

        assert!(rendered
            .iter()
            .any(|sample| (*sample - 1.0).abs() <= 1.0e-12));
        assert!(!chain.finish_was_capped());
    }

    #[test]
    fn unknown_tail_energy_is_observed_before_terminal_noise() {
        let mut chain = DspChain::with_capacity(2, 48_000);
        chain.add(UnknownDecay {
            state: 0.0,
            finished: false,
        });
        chain.add(TerminalNoise { level: 0.25 });
        let mut input = [1.0_f64];
        let _ = chain.process(&mut input, 1).unwrap();

        let policy = ChainFinishPolicy::new(-20.0, 2, 20);
        let mut output = [0.0_f64; 1];
        let mut terminal = ProcessProgress::new(0, 0, ProcessState::NeedOutput);
        for _ in 0..24 {
            terminal = chain
                .finish_with_policy(AudioBlockMut::new(&mut output, 1).unwrap(), policy)
                .unwrap();
            if terminal.state() == ProcessState::Finished {
                break;
            }
        }

        assert_eq!(terminal.state(), ProcessState::Finished);
        assert!(!chain.finish_was_capped());
    }

    #[test]
    fn unknown_tail_safety_cap_does_not_overshoot_large_output_blocks() {
        let mut chain = DspChain::new(48_000);
        chain.add(UnknownDecay {
            state: 0.0,
            finished: false,
        });
        let mut input = [1.0_f64];
        let _ = chain.process(&mut input, 1).unwrap();

        let policy = ChainFinishPolicy::new(-120.0, 1, 5);
        let mut output = [0.0_f64; 64];
        let progress = chain
            .finish_with_policy(AudioBlockMut::new(&mut output, 1).unwrap(), policy)
            .unwrap();

        assert_eq!(progress, ProcessProgress::finished(5));
        assert!(chain.finish_was_capped());
    }

    #[test]
    fn capped_unknown_tail_continues_to_downstream_finish_in_the_same_call() {
        let mut chain = DspChain::with_capacity(2, 48_000);
        chain.add(UnknownDecay {
            state: 0.0,
            finished: false,
        });
        chain.add(GainProcessor);
        let mut input = [1.0_f64];
        let _ = chain.process(&mut input, 1).unwrap();

        let policy = ChainFinishPolicy::new(-120.0, 1, 5);
        let mut output = [0.0_f64; 64];
        let progress = chain
            .finish_with_policy(AudioBlockMut::new(&mut output, 1).unwrap(), policy)
            .unwrap();

        assert_eq!(progress, ProcessProgress::finished(5));
        assert_eq!(output[..5], [2.0, 1.0, 0.5, 0.25, 0.125]);
        assert!(chain.finish_was_capped());
    }

    #[test]
    fn chain_composes_latency_and_unknown_tail() {
        let mut chain = DspChain::new(48_000);
        chain.add(UnknownDecay {
            state: 0.0,
            finished: false,
        });
        assert_eq!(chain.latency(), FrameDuration::ZERO);
        assert_eq!(chain.tail(), TailSpec::Unknown);
    }

    #[test]
    fn finite_tail_rounding_happens_after_cross_rate_sum() {
        let mut chain = DspChain::new(48_000);
        chain.add(FractionalTail(FrameDuration::new(1, 96_000).unwrap()));
        chain.add(FractionalTail(FrameDuration::new(1, 96_000).unwrap()));

        assert_eq!(chain.tail(), TailSpec::finite(1, 48_000).unwrap());
    }
}
