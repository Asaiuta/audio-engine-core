//! Audio Processor Traits
//!
//! Defines the unified interface for all DSP processors in the audio pipeline.
//! This abstraction enables a composable DSP chain with guaranteed continuity.

/// Processing result status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessResult {
    /// Normal processing completed
    Ok,
    /// Processor is disabled, signal passed through unchanged
    Bypassed,
}

/// Core audio processor trait
///
/// All DSP processors must implement this trait to be used in the DspChain.
/// The trait provides a unified interface for:
/// - Audio processing
/// - State reset
/// - Enable/disable control
///
/// # Thread Safety
///
/// Implementations must be `Send` because processors are owned by the audio thread.
/// Parameters should be passed via the snapshot types in `lockfree_params`.
///
/// # Example
///
/// ```ignore
/// use crate::processor::traits::{AudioProcessor, ProcessResult};
///
/// struct MyProcessor {
///     enabled: bool,
///     gain: f64,
/// }
///
/// impl AudioProcessor for MyProcessor {
///     fn name(&self) -> &'static str { "MyProcessor" }
///     
///     fn process(&mut self, buffer: &mut [f64], channels: usize) -> ProcessResult {
///         if !self.enabled {
///             return ProcessResult::Bypassed;
///         }
///         for sample in buffer.iter_mut() {
///             *sample *= self.gain;
///         }
///         ProcessResult::Ok
///     }
///     
///     fn reset(&mut self) {}
///     fn is_enabled(&self) -> bool { self.enabled }
///     fn set_enabled(&mut self, enabled: bool) { self.enabled = enabled; }
/// }
/// ```
pub trait AudioProcessor: Send {
    /// Processor name for debugging and logging
    fn name(&self) -> &'static str;

    /// Process audio samples in-place
    ///
    /// # Arguments
    /// * `buffer` - Interleaved audio samples [L, R, L, R, ...]
    /// * `channels` - Number of audio channels
    ///
    /// # Returns
    /// Processing result status indicating what happened
    fn process(&mut self, buffer: &mut [f64], channels: usize) -> ProcessResult;

    /// Reset internal state (filter delay lines, etc.)
    ///
    /// Called when:
    /// - Starting a new track
    /// - Changing sample rate
    /// - After gapless track switch
    fn reset(&mut self);

    /// Check if processor is enabled
    fn is_enabled(&self) -> bool;

    /// Enable or disable the processor
    fn set_enabled(&mut self, enabled: bool);

    /// Update sample rate and recalculate internal coefficients if needed.
    ///
    /// Default implementation is no-op for processors that are sample-rate agnostic.
    fn set_sample_rate(&mut self, _sample_rate: f64) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestProcessor {
        enabled: bool,
        gain: f64,
    }

    impl AudioProcessor for TestProcessor {
        fn name(&self) -> &'static str {
            "TestProcessor"
        }

        fn process(&mut self, buffer: &mut [f64], _channels: usize) -> ProcessResult {
            if !self.enabled {
                return ProcessResult::Bypassed;
            }
            for sample in buffer.iter_mut() {
                *sample *= self.gain;
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
    fn test_processor_enabled() {
        let mut proc = TestProcessor {
            enabled: true,
            gain: 0.5,
        };
        let mut buffer = vec![1.0, 1.0];
        let result = proc.process(&mut buffer, 1);
        assert_eq!(result, ProcessResult::Ok);
        assert!((buffer[0] - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_processor_bypassed() {
        let mut proc = TestProcessor {
            enabled: false,
            gain: 0.5,
        };
        let mut buffer = vec![1.0, 1.0];
        let result = proc.process(&mut buffer, 1);
        assert_eq!(result, ProcessResult::Bypassed);
        assert!((buffer[0] - 1.0).abs() < 1e-10);
    }
}
