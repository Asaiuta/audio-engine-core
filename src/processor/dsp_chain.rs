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
    process_checked, AudioBlockMut, ProcessBuffers, ProcessError, ProcessProgress, ProcessState,
    StreamingProcessor,
};

/// DSP processing chain
///
/// Manages multiple audio processors in sequence.
/// All processors share the same buffer, processed in-place.
pub struct DspChain {
    /// Processors in execution order
    processors: Vec<Box<dyn StreamingProcessor>>,
}

impl DspChain {
    /// Create an empty DSP chain
    pub fn new(_sample_rate_hz: u32) -> Self {
        Self {
            processors: Vec::new(),
        }
    }

    /// Create a chain with pre-allocated capacity
    pub fn with_capacity(capacity: usize, _sample_rate_hz: u32) -> Self {
        Self {
            processors: Vec::with_capacity(capacity),
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
        first_error.map_or(Ok(()), Err)
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
}
