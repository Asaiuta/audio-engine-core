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

use super::traits::AudioProcessor;

/// DSP processing chain
///
/// Manages multiple audio processors in sequence.
/// All processors share the same buffer, processed in-place.
pub struct DspChain {
    /// Processors in execution order
    processors: Vec<Box<dyn AudioProcessor>>,
}

impl DspChain {
    /// Create an empty DSP chain
    pub fn new(_sample_rate: f64) -> Self {
        Self {
            processors: Vec::new(),
        }
    }

    /// Create a chain with pre-allocated capacity
    pub fn with_capacity(capacity: usize, _sample_rate: f64) -> Self {
        Self {
            processors: Vec::with_capacity(capacity),
        }
    }

    /// Add a processor to the end of the chain
    pub fn add<P: AudioProcessor + 'static>(&mut self, processor: P) -> &mut Self {
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
    pub fn process(&mut self, buffer: &mut [f64], channels: usize) {
        for processor in &mut self.processors {
            processor.process(buffer, channels);
        }
    }

    /// Reset all processors
    pub fn reset(&mut self) {
        for processor in &mut self.processors {
            processor.reset();
        }
    }

    /// Update sample rate for all processors
    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        for processor in &mut self.processors {
            processor.set_sample_rate(sample_rate);
        }
    }

    /// Get number of processors
    pub fn len(&self) -> usize {
        self.processors.len()
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
        Self::new(44100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::traits::ProcessResult;
    use super::*;

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

    impl AudioProcessor for DoublerProcessor {
        fn name(&self) -> &'static str {
            "Doubler"
        }

        fn process(&mut self, buffer: &mut [f64], _channels: usize) -> ProcessResult {
            if !self.enabled {
                return ProcessResult::Bypassed;
            }
            for sample in buffer.iter_mut() {
                *sample *= 2.0;
            }
            self.processed_count += 1;
            ProcessResult::Ok
        }

        fn reset(&mut self) {
            self.processed_count = 0;
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

    impl AudioProcessor for AdderProcessor {
        fn name(&self) -> &'static str {
            "Adder"
        }

        fn process(&mut self, buffer: &mut [f64], _channels: usize) -> ProcessResult {
            if !self.enabled {
                return ProcessResult::Bypassed;
            }
            for sample in buffer.iter_mut() {
                *sample += 1.0;
            }
            ProcessResult::Ok
        }

        fn reset(&mut self) {}

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn set_enabled(&mut self, enabled: bool) {
            self.enabled = enabled;
        }
    }

    #[test]
    fn test_empty_chain() {
        let mut chain = DspChain::new(44100.0);
        let mut buffer = vec![1.0, 2.0, 3.0];
        chain.process(&mut buffer, 1);
        assert_eq!(buffer, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_single_processor() {
        let mut chain = DspChain::new(44100.0);
        chain.add(DoublerProcessor::new());

        let mut buffer = vec![1.0, 2.0, 3.0];
        chain.process(&mut buffer, 1);

        assert_eq!(buffer, vec![2.0, 4.0, 6.0]);
    }

    #[test]
    fn test_chain_order() {
        let mut chain = DspChain::new(44100.0);
        chain.add(DoublerProcessor::new()); // Doubles first
        chain.add(AdderProcessor::new()); // Then adds 1

        // Start with 1.0 -> 2.0 (double) -> 3.0 (add 1)
        let mut buffer = vec![1.0];
        chain.process(&mut buffer, 1);
        assert_eq!(buffer, vec![3.0]);
    }

    #[test]
    fn test_bypassed_processor() {
        let mut chain = DspChain::new(44100.0);
        let mut doubler = DoublerProcessor::new();
        doubler.set_enabled(false);
        chain.add(doubler);

        let mut buffer = vec![5.0];
        chain.process(&mut buffer, 1);

        // Should be unchanged (bypassed)
        assert_eq!(buffer, vec![5.0]);
    }

    #[test]
    fn test_reset() {
        let mut chain = DspChain::new(44100.0);
        chain.add(DoublerProcessor::new());

        let mut buffer = vec![1.0; 100];
        chain.process(&mut buffer, 1);
        chain.reset();
        // Should not panic
    }
}
