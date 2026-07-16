//! Audio Processor Traits
//!
//! Defines the unified interface for all DSP processors in the audio pipeline.
//! This abstraction enables a composable DSP chain with guaranteed continuity.

use std::num::{NonZeroU32, NonZeroUsize};

use thiserror::Error;

/// Validation failure for a borrowed interleaved audio block.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioBlockError {
    /// An interleaved block cannot describe frames without at least one channel.
    #[error("audio block channel count must be greater than zero")]
    ZeroChannels,
    /// The sample slice ends with an incomplete interleaved frame.
    #[error("interleaved sample count {samples} is not divisible by channel count {channels}")]
    IncompleteFrame { samples: usize, channels: usize },
    /// An out-of-place call requires the same channel count on both sides.
    #[error(
        "input/output channel mismatch: input has {input_channels}, output has {output_channels}"
    )]
    ChannelMismatch {
        input_channels: usize,
        output_channels: usize,
    },
}

/// Zero-copy view over a complete interleaved `f64` input block.
#[derive(Debug, Clone, Copy)]
pub struct AudioBlockRef<'a> {
    samples: &'a [f64],
    channels: NonZeroUsize,
    frames: usize,
}

impl<'a> AudioBlockRef<'a> {
    /// Validate and borrow an interleaved sample slice.
    pub fn new(samples: &'a [f64], channels: usize) -> Result<Self, AudioBlockError> {
        let channels = NonZeroUsize::new(channels).ok_or(AudioBlockError::ZeroChannels)?;
        if !samples.len().is_multiple_of(channels.get()) {
            return Err(AudioBlockError::IncompleteFrame {
                samples: samples.len(),
                channels: channels.get(),
            });
        }

        Ok(Self {
            samples,
            channels,
            frames: samples.len() / channels.get(),
        })
    }

    /// Borrow all interleaved samples in the block.
    pub fn samples(self) -> &'a [f64] {
        self.samples
    }

    /// Number of interleaved channels.
    pub fn channels(self) -> usize {
        self.channels.get()
    }

    /// Number of complete frames in the block.
    pub fn frames(self) -> usize {
        self.frames
    }

    /// Number of interleaved samples in the block.
    pub fn sample_count(self) -> usize {
        self.samples.len()
    }

    /// Whether the block contains zero frames.
    pub fn is_empty(self) -> bool {
        self.frames == 0
    }
}

/// Zero-copy mutable view over a complete interleaved `f64` block.
#[derive(Debug)]
pub struct AudioBlockMut<'a> {
    samples: &'a mut [f64],
    channels: NonZeroUsize,
    frames: usize,
}

impl<'a> AudioBlockMut<'a> {
    /// Validate and mutably borrow an interleaved sample slice.
    pub fn new(samples: &'a mut [f64], channels: usize) -> Result<Self, AudioBlockError> {
        let channels = NonZeroUsize::new(channels).ok_or(AudioBlockError::ZeroChannels)?;
        if !samples.len().is_multiple_of(channels.get()) {
            return Err(AudioBlockError::IncompleteFrame {
                samples: samples.len(),
                channels: channels.get(),
            });
        }

        Ok(Self {
            frames: samples.len() / channels.get(),
            samples,
            channels,
        })
    }

    /// Borrow all interleaved samples immutably.
    pub fn samples(&self) -> &[f64] {
        self.samples
    }

    /// Borrow all interleaved samples mutably.
    pub fn samples_mut(&mut self) -> &mut [f64] {
        self.samples
    }

    /// Consume the view and return its original mutable slice.
    pub fn into_samples(self) -> &'a mut [f64] {
        self.samples
    }

    /// Borrow this mutable view for a shorter lifetime.
    pub fn reborrow(&mut self) -> AudioBlockMut<'_> {
        AudioBlockMut {
            samples: self.samples,
            channels: self.channels,
            frames: self.frames,
        }
    }

    /// Create an immutable view over the same block.
    pub fn as_ref(&self) -> AudioBlockRef<'_> {
        AudioBlockRef {
            samples: self.samples,
            channels: self.channels,
            frames: self.frames,
        }
    }

    /// Number of interleaved channels.
    pub fn channels(&self) -> usize {
        self.channels.get()
    }

    /// Number of complete frames in the block.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Number of interleaved samples in the block.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Whether the block contains zero frames.
    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }
}

/// Buffer shape selected for one streaming processor call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessBufferMode {
    /// Read and write the same fixed-size block.
    InPlace,
    /// Read input and write to independent caller-owned storage.
    OutOfPlace,
}

/// Borrowed buffers yielded to a processor implementation.
#[derive(Debug)]
pub enum ProcessBufferParts<'a> {
    /// Fixed-size zero-copy processing.
    InPlace(AudioBlockMut<'a>),
    /// Variable-I/O or caller-separated processing.
    OutOfPlace {
        input: AudioBlockRef<'a>,
        output: AudioBlockMut<'a>,
    },
}

/// Validated input/output buffers for one [`StreamingProcessor::process`] call.
#[derive(Debug)]
pub struct ProcessBuffers<'a> {
    parts: ProcessBufferParts<'a>,
}

impl<'a> ProcessBuffers<'a> {
    /// Create an in-place call over one complete interleaved block.
    pub fn in_place(block: AudioBlockMut<'a>) -> Self {
        Self {
            parts: ProcessBufferParts::InPlace(block),
        }
    }

    /// Create an out-of-place call with matching channel counts.
    pub fn out_of_place(
        input: AudioBlockRef<'a>,
        output: AudioBlockMut<'a>,
    ) -> Result<Self, AudioBlockError> {
        if input.channels() != output.channels() {
            return Err(AudioBlockError::ChannelMismatch {
                input_channels: input.channels(),
                output_channels: output.channels(),
            });
        }

        Ok(Self {
            parts: ProcessBufferParts::OutOfPlace { input, output },
        })
    }

    /// Processing shape used for this call.
    pub fn mode(&self) -> ProcessBufferMode {
        match &self.parts {
            ProcessBufferParts::InPlace(_) => ProcessBufferMode::InPlace,
            ProcessBufferParts::OutOfPlace { .. } => ProcessBufferMode::OutOfPlace,
        }
    }

    /// Shared channel count for input and output.
    pub fn channels(&self) -> usize {
        match &self.parts {
            ProcessBufferParts::InPlace(block) => block.channels(),
            ProcessBufferParts::OutOfPlace { input, .. } => input.channels(),
        }
    }

    /// Capture frame capacities before moving the buffers into a processor.
    pub fn capacity(&self) -> ProcessCapacity {
        match &self.parts {
            ProcessBufferParts::InPlace(block) => ProcessCapacity::in_place(block.frames()),
            ProcessBufferParts::OutOfPlace { input, output } => {
                ProcessCapacity::new(input.frames(), output.frames())
            }
        }
    }

    /// Consume the wrapper and expose the selected safe buffer shape.
    pub fn into_parts(self) -> ProcessBufferParts<'a> {
        self.parts
    }
}

/// Reason the caller should stop or continue driving a processor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// All currently usable input was consumed; provide another input block.
    NeedInput,
    /// Output capacity stopped progress; call again with writable output space.
    NeedOutput,
    /// End-of-stream processing is complete and further finish calls produce zero frames.
    Finished,
}

/// Consumption/production result for a streaming processor call.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessProgress {
    consumed_frames: usize,
    produced_frames: usize,
    state: ProcessState,
    bypassed: bool,
}

impl ProcessProgress {
    /// Construct an explicit progress result.
    pub const fn new(consumed_frames: usize, produced_frames: usize, state: ProcessState) -> Self {
        Self {
            consumed_frames,
            produced_frames,
            state,
            bypassed: false,
        }
    }

    /// Mark this result as a transparent bypass operation.
    pub const fn with_bypassed(mut self, bypassed: bool) -> Self {
        self.bypassed = bypassed;
        self
    }

    /// Construct a terminal finish result.
    pub const fn finished(produced_frames: usize) -> Self {
        Self::new(0, produced_frames, ProcessState::Finished)
    }

    pub const fn consumed_frames(self) -> usize {
        self.consumed_frames
    }

    pub const fn produced_frames(self) -> usize {
        self.produced_frames
    }

    pub const fn state(self) -> ProcessState {
        self.state
    }

    pub const fn is_bypassed(self) -> bool {
        self.bypassed
    }

    pub const fn made_progress(self) -> bool {
        self.consumed_frames > 0 || self.produced_frames > 0
    }
}

/// Frame capacities associated with one process/finish call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessCapacity {
    input_frames: usize,
    output_frames: usize,
    mode: ProcessBufferMode,
    finishing: bool,
}

impl ProcessCapacity {
    pub const fn new(input_frames: usize, output_frames: usize) -> Self {
        Self {
            input_frames,
            output_frames,
            mode: ProcessBufferMode::OutOfPlace,
            finishing: false,
        }
    }

    pub const fn in_place(frames: usize) -> Self {
        Self {
            input_frames: frames,
            output_frames: frames,
            mode: ProcessBufferMode::InPlace,
            finishing: false,
        }
    }

    pub const fn for_finish(output_frames: usize) -> Self {
        Self {
            input_frames: 0,
            output_frames,
            mode: ProcessBufferMode::OutOfPlace,
            finishing: true,
        }
    }

    pub const fn input_frames(self) -> usize {
        self.input_frames
    }

    pub const fn output_frames(self) -> usize {
        self.output_frames
    }

    pub const fn mode(self) -> ProcessBufferMode {
        self.mode
    }

    pub const fn is_finishing(self) -> bool {
        self.finishing
    }

    /// Verify bounds, caller-direction state, and forward progress.
    pub fn validate(
        self,
        processor: &'static str,
        progress: ProcessProgress,
    ) -> Result<ProcessProgress, ProcessError> {
        if progress.consumed_frames() > self.input_frames
            || progress.produced_frames() > self.output_frames
        {
            return Err(ProcessError::InvalidProgress {
                processor,
                consumed_frames: progress.consumed_frames(),
                produced_frames: progress.produced_frames(),
                input_capacity_frames: self.input_frames,
                output_capacity_frames: self.output_frames,
            });
        }

        if !progress.made_progress()
            && self.input_frames > 0
            && self.output_frames > 0
            && progress.state() != ProcessState::Finished
        {
            return Err(ProcessError::Stalled { processor });
        }

        let invalid_direction = if self.finishing {
            match progress.state() {
                ProcessState::NeedInput => true,
                ProcessState::NeedOutput => progress.produced_frames() != self.output_frames,
                ProcessState::Finished => progress.consumed_frames() != 0,
            }
        } else if self.mode == ProcessBufferMode::InPlace {
            progress.consumed_frames() != self.input_frames
                || progress.produced_frames() != self.output_frames
                || progress.state() != ProcessState::NeedInput
        } else {
            match progress.state() {
                ProcessState::NeedInput => progress.consumed_frames() != self.input_frames,
                ProcessState::NeedOutput => progress.produced_frames() != self.output_frames,
                ProcessState::Finished => true,
            }
        };
        if invalid_direction {
            return Err(ProcessError::InvalidProgress {
                processor,
                consumed_frames: progress.consumed_frames(),
                produced_frames: progress.produced_frames(),
                input_capacity_frames: self.input_frames,
                output_capacity_frames: self.output_frames,
            });
        }

        Ok(progress)
    }
}

/// Failure while constructing or rescaling frame-domain timing metadata.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum TimingError {
    #[error("sample rate must be greater than zero")]
    ZeroSampleRate,
    #[error("rescaled frame count does not fit in usize")]
    FrameCountOverflow,
}

/// Rounding policy used only after timing values reach a common sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRounding {
    Floor,
    /// Round to the nearest frame; exact half-frame ties round upward.
    Nearest,
    Ceil,
}

/// A frame count carrying the sample-rate domain in which it was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDuration {
    frames: usize,
    sample_rate_hz: Option<NonZeroU32>,
}

impl FrameDuration {
    /// Rate-independent zero duration used by processors with no latency/tail.
    pub const ZERO: Self = Self {
        frames: 0,
        sample_rate_hz: None,
    };

    /// Construct a frame duration in a non-zero sample-rate domain.
    pub fn new(frames: usize, sample_rate_hz: u32) -> Result<Self, TimingError> {
        let sample_rate_hz = NonZeroU32::new(sample_rate_hz).ok_or(TimingError::ZeroSampleRate)?;
        Ok(Self {
            frames,
            sample_rate_hz: Some(sample_rate_hz),
        })
    }

    pub const fn frames(self) -> usize {
        self.frames
    }

    /// `None` only for the rate-independent [`Self::ZERO`] value.
    pub const fn sample_rate_hz(self) -> Option<u32> {
        match self.sample_rate_hz {
            Some(rate) => Some(rate.get()),
            None => None,
        }
    }

    pub const fn is_zero(self) -> bool {
        self.frames == 0
    }

    /// Convert to a fractional frame count without rounding.
    ///
    /// A chain should convert every stage to the final rate, sum these values,
    /// and round the total once. The default offline policy uses nearest-frame
    /// rounding for accumulated latency and ceiling for accumulated finite tail.
    pub fn frames_at_rate_f64(self, target_sample_rate_hz: u32) -> Result<f64, TimingError> {
        let target = NonZeroU32::new(target_sample_rate_hz).ok_or(TimingError::ZeroSampleRate)?;
        let Some(source) = self.sample_rate_hz else {
            return Ok(0.0);
        };
        Ok(self.frames as f64 * target.get() as f64 / source.get() as f64)
    }

    /// Rescale and round one duration. Chain composition should prefer
    /// [`Self::frames_at_rate_f64`] and round only the final accumulated value.
    pub fn rounded_frames_at_rate(
        self,
        target_sample_rate_hz: u32,
        rounding: FrameRounding,
    ) -> Result<usize, TimingError> {
        let target = NonZeroU32::new(target_sample_rate_hz).ok_or(TimingError::ZeroSampleRate)?;
        let Some(source) = self.sample_rate_hz else {
            return Ok(0);
        };

        let numerator = (self.frames as u128) * (target.get() as u128);
        let denominator = source.get() as u128;
        let rounded = match rounding {
            FrameRounding::Floor => numerator / denominator,
            FrameRounding::Nearest => (numerator + denominator / 2) / denominator,
            FrameRounding::Ceil => numerator.div_ceil(denominator),
        };
        usize::try_from(rounded).map_err(|_| TimingError::FrameCountOverflow)
    }
}

/// Tail behavior with exact timing metadata for finite tails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailSpec {
    /// The processor produces no semantic effect tail.
    None,
    /// The processor produces exactly this duration after input ends.
    Finite(FrameDuration),
    /// The processor decays but cannot predict an exact terminal frame.
    Unknown,
    /// The processor may produce a non-decaying or intentionally infinite tail.
    Infinite,
}

impl TailSpec {
    /// Normalize a zero-length finite tail to [`TailSpec::None`].
    pub fn finite(frames: usize, sample_rate_hz: u32) -> Result<Self, TimingError> {
        if frames == 0 {
            if sample_rate_hz == 0 {
                return Err(TimingError::ZeroSampleRate);
            }
            Ok(Self::None)
        } else {
            Ok(Self::Finite(FrameDuration::new(frames, sample_rate_hz)?))
        }
    }

    pub const fn finite_duration(self) -> Option<FrameDuration> {
        match self {
            Self::None => Some(FrameDuration::ZERO),
            Self::Finite(duration) => Some(duration),
            Self::Unknown | Self::Infinite => None,
        }
    }
}

/// Typed failure from the unified streaming contract.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProcessError {
    #[error(transparent)]
    InvalidBlock(#[from] AudioBlockError),
    #[error("processor {processor} does not support {mode:?} processing")]
    UnsupportedBufferMode {
        processor: &'static str,
        mode: ProcessBufferMode,
    },
    #[error(
        "processor {processor} returned invalid progress: consumed {consumed_frames}/{input_capacity_frames} input frames, produced {produced_frames}/{output_capacity_frames} output frames"
    )]
    InvalidProgress {
        processor: &'static str,
        consumed_frames: usize,
        produced_frames: usize,
        input_capacity_frames: usize,
        output_capacity_frames: usize,
    },
    #[error("processor {processor} made no progress with non-empty input and output capacity")]
    Stalled { processor: &'static str },
    #[error("processor {processor} received input after end-of-stream; reset it first")]
    AlreadyFinished { processor: &'static str },
    #[error(
        "processor {processor} expected {expected_channels} channels but received {actual_channels}"
    )]
    ChannelCountMismatch {
        processor: &'static str,
        expected_channels: usize,
        actual_channels: usize,
    },
    #[error("processor {processor} received invalid sample rate {sample_rate_hz} Hz")]
    InvalidSampleRate {
        processor: &'static str,
        sample_rate_hz: u32,
    },
    /// Allocation-free backend diagnostic for realtime-capable processing.
    #[error("processor {processor} failed during {operation}: {message}")]
    Backend {
        processor: &'static str,
        operation: &'static str,
        message: &'static str,
    },
    /// Owned diagnostic accepted from existing setup/offline APIs.
    ///
    /// Realtime implementations must use [`Self::Backend`] so constructing an
    /// error never allocates on the callback thread.
    #[error("processor {processor} failed during {operation}: {message}")]
    Owned {
        processor: &'static str,
        operation: &'static str,
        message: String,
    },
}

/// Unified object-safe streaming DSP lifecycle.
///
/// [`Self::latency`] and finite [`Self::tail`] values carry their own sample-rate
/// domains. A chain converts every duration to its final output rate, sums the
/// fractional frame values, then rounds once: nearest for latency compensation
/// and ceiling for finite-tail preservation.
pub trait StreamingProcessor: Send {
    /// Stable processor name for diagnostics outside the realtime path.
    fn name(&self) -> &'static str;

    /// Consume input and produce output using caller-owned storage.
    ///
    /// Chain/direct drivers should call [`process_checked`] so progress bounds,
    /// in-place 1:1 behavior, and forward progress are centrally enforced.
    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError>;

    /// Produce remaining algorithm delay/effect tail after the final input.
    ///
    /// Implementations must be idempotent: once this returns `Finished`, all
    /// later calls produce zero frames and remain `Finished` until
    /// [`Self::reset`]. Processing new input before reset returns
    /// [`ProcessError::AlreadyFinished`]. Drivers should call [`finish_checked`].
    fn finish(&mut self, _output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        Ok(ProcessProgress::finished(0))
    }

    /// Clear all Rust and native backend state before a logically new stream.
    fn reset(&mut self) -> Result<(), ProcessError>;

    /// Algorithmic delay, excluding semantic effect tail.
    fn latency(&self) -> FrameDuration {
        FrameDuration::ZERO
    }

    /// Semantic effect tail after the last input frame.
    fn tail(&self) -> TailSpec {
        TailSpec::None
    }

    /// Whether signal processing is active.
    fn is_enabled(&self) -> bool;

    /// Enable or transparently bypass this processor.
    fn set_enabled(&mut self, enabled: bool);

    /// Map the current graph rate through this stage.
    ///
    /// Fixed-rate processors keep the input rate. A resampler overrides this
    /// method and returns its configured output rate.
    fn output_sample_rate_hz(&self, input_sample_rate_hz: u32) -> Result<u32, ProcessError> {
        if input_sample_rate_hz == 0 {
            return Err(ProcessError::InvalidSampleRate {
                processor: self.name(),
                sample_rate_hz: input_sample_rate_hz,
            });
        }
        Ok(input_sample_rate_hz)
    }

    /// Update sample rate and any dependent coefficients on a non-realtime path.
    fn set_sample_rate(&mut self, _sample_rate_hz: u32) -> Result<(), ProcessError> {
        Ok(())
    }
}

/// Drive one process call and enforce the shared progress invariants.
pub fn process_checked<P: StreamingProcessor + ?Sized>(
    processor: &mut P,
    buffers: ProcessBuffers<'_>,
) -> Result<ProcessProgress, ProcessError> {
    let capacity = buffers.capacity();
    let progress = processor.process(buffers)?;
    capacity.validate(processor.name(), progress)
}

/// Drive one finish call and enforce terminal lifecycle invariants.
pub fn finish_checked<P: StreamingProcessor + ?Sized>(
    processor: &mut P,
    output: AudioBlockMut<'_>,
) -> Result<ProcessProgress, ProcessError> {
    let capacity = ProcessCapacity::for_finish(output.frames());
    let progress = processor.finish(output)?;
    capacity.validate(processor.name(), progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StreamingGain {
        enabled: bool,
        gain: f64,
    }

    impl StreamingProcessor for StreamingGain {
        fn name(&self) -> &'static str {
            "StreamingGain"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            let progress = match buffers.into_parts() {
                ProcessBufferParts::InPlace(mut block) => {
                    if self.enabled {
                        for sample in block.samples_mut() {
                            *sample *= self.gain;
                        }
                    }
                    ProcessProgress::new(block.frames(), block.frames(), ProcessState::NeedInput)
                        .with_bypassed(!self.enabled)
                }
                ProcessBufferParts::OutOfPlace { input, mut output } => {
                    let frames = input.frames().min(output.frames());
                    let samples = frames * input.channels();
                    let input_samples = &input.samples()[..samples];
                    let output_samples = &mut output.samples_mut()[..samples];

                    if self.enabled {
                        for (source, destination) in
                            input_samples.iter().zip(output_samples.iter_mut())
                        {
                            *destination = *source * self.gain;
                        }
                    } else {
                        output_samples.copy_from_slice(input_samples);
                    }

                    let state = if frames < input.frames() {
                        ProcessState::NeedOutput
                    } else {
                        ProcessState::NeedInput
                    };
                    ProcessProgress::new(frames, frames, state).with_bypassed(!self.enabled)
                }
            };

            Ok(progress)
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

    struct FiniteTailProcessor {
        tail_frames: usize,
        remaining_frames: usize,
        sample_rate_hz: u32,
        finished: bool,
    }

    impl FiniteTailProcessor {
        fn new(tail_frames: usize, sample_rate_hz: u32) -> Self {
            Self {
                tail_frames,
                remaining_frames: tail_frames,
                sample_rate_hz,
                finished: false,
            }
        }
    }

    impl StreamingProcessor for FiniteTailProcessor {
        fn name(&self) -> &'static str {
            "FiniteTail"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            if self.finished {
                return Err(ProcessError::AlreadyFinished {
                    processor: self.name(),
                });
            }

            let progress = match buffers.into_parts() {
                ProcessBufferParts::InPlace(block) => {
                    ProcessProgress::new(block.frames(), block.frames(), ProcessState::NeedInput)
                }
                ProcessBufferParts::OutOfPlace { input, mut output } => {
                    let frames = input.frames().min(output.frames());
                    let samples = frames * input.channels();
                    output.samples_mut()[..samples].copy_from_slice(&input.samples()[..samples]);
                    let state = if frames < input.frames() {
                        ProcessState::NeedOutput
                    } else {
                        ProcessState::NeedInput
                    };
                    ProcessProgress::new(frames, frames, state)
                }
            };
            Ok(progress)
        }

        fn finish(
            &mut self,
            mut output: AudioBlockMut<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            if self.finished {
                return Ok(ProcessProgress::finished(0));
            }

            let produced = self.remaining_frames.min(output.frames());
            let produced_samples = produced * output.channels();
            output.samples_mut()[..produced_samples].fill(0.25);
            self.remaining_frames -= produced;

            if self.remaining_frames == 0 {
                self.finished = true;
                Ok(ProcessProgress::finished(produced))
            } else {
                Ok(ProcessProgress::new(0, produced, ProcessState::NeedOutput))
            }
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            self.remaining_frames = self.tail_frames;
            self.finished = false;
            Ok(())
        }

        fn tail(&self) -> TailSpec {
            TailSpec::finite(self.tail_frames, self.sample_rate_hz)
                .expect("test processor uses a non-zero sample rate")
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn set_enabled(&mut self, _enabled: bool) {}
    }

    #[test]
    fn audio_block_views_validate_complete_interleaved_frames() {
        let input = [1.0, 2.0, 3.0, 4.0];
        let block = AudioBlockRef::new(&input, 2).unwrap();
        assert_eq!(block.channels(), 2);
        assert_eq!(block.frames(), 2);
        assert_eq!(block.sample_count(), 4);
        assert_eq!(block.samples().as_ptr(), input.as_ptr());

        assert_eq!(
            AudioBlockRef::new(&input, 0).unwrap_err(),
            AudioBlockError::ZeroChannels
        );
        assert_eq!(
            AudioBlockRef::new(&input[..3], 2).unwrap_err(),
            AudioBlockError::IncompleteFrame {
                samples: 3,
                channels: 2,
            }
        );

        let mut mutable = input;
        assert_eq!(
            AudioBlockMut::new(&mut mutable, 0).unwrap_err(),
            AudioBlockError::ZeroChannels
        );
        assert_eq!(
            AudioBlockMut::new(&mut mutable[..3], 2).unwrap_err(),
            AudioBlockError::IncompleteFrame {
                samples: 3,
                channels: 2,
            }
        );
    }

    #[test]
    fn mutable_audio_block_is_zero_copy_and_reborrowable() {
        let mut samples = [1.0, 2.0, 3.0, 4.0];
        let original_ptr = samples.as_ptr();
        {
            let mut block = AudioBlockMut::new(&mut samples, 2).unwrap();
            assert_eq!(block.samples().as_ptr(), original_ptr);
            block.samples_mut()[1] = 8.0;

            let mut shorter = block.reborrow();
            shorter.samples_mut()[2] = 9.0;
            assert_eq!(shorter.as_ref().frames(), 2);
        }
        assert_eq!(samples, [1.0, 8.0, 9.0, 4.0]);
    }

    #[test]
    fn out_of_place_buffers_require_matching_channels() {
        let input = [1.0, 2.0, 3.0, 4.0];
        let mut output = [0.0; 4];
        let input = AudioBlockRef::new(&input, 2).unwrap();
        let output = AudioBlockMut::new(&mut output, 1).unwrap();

        assert_eq!(
            ProcessBuffers::out_of_place(input, output).unwrap_err(),
            AudioBlockError::ChannelMismatch {
                input_channels: 2,
                output_channels: 1,
            }
        );
    }

    #[test]
    fn process_capacity_rejects_overrun_wrong_direction_and_stall() {
        let capacity = ProcessCapacity::new(4, 4);
        let valid = ProcessProgress::new(4, 3, ProcessState::NeedInput);
        assert_eq!(capacity.validate("test", valid), Ok(valid));

        let overrun = ProcessProgress::new(5, 4, ProcessState::NeedInput);
        assert!(matches!(
            capacity.validate("test", overrun),
            Err(ProcessError::InvalidProgress { .. })
        ));

        let wrong_direction = ProcessProgress::new(3, 4, ProcessState::NeedInput);
        assert!(matches!(
            capacity.validate("test", wrong_direction),
            Err(ProcessError::InvalidProgress { .. })
        ));

        let partial_in_place = ProcessProgress::new(3, 4, ProcessState::NeedOutput);
        assert!(matches!(
            ProcessCapacity::in_place(4).validate("test", partial_in_place),
            Err(ProcessError::InvalidProgress { .. })
        ));

        let process_finished = ProcessProgress::new(4, 4, ProcessState::Finished);
        assert!(matches!(
            capacity.validate("test", process_finished),
            Err(ProcessError::InvalidProgress { .. })
        ));

        let stalled = ProcessProgress::new(0, 0, ProcessState::NeedInput);
        assert_eq!(
            capacity.validate("test", stalled),
            Err(ProcessError::Stalled { processor: "test" })
        );

        let finish_needs_input = ProcessProgress::new(0, 0, ProcessState::NeedInput);
        assert!(matches!(
            ProcessCapacity::for_finish(4).validate("test", finish_needs_input),
            Err(ProcessError::InvalidProgress { .. })
        ));
        let finish_has_more = ProcessProgress::new(0, 4, ProcessState::NeedOutput);
        assert_eq!(
            ProcessCapacity::for_finish(4).validate("test", finish_has_more),
            Ok(finish_has_more)
        );
        let finish_complete = ProcessProgress::finished(3);
        assert_eq!(
            ProcessCapacity::for_finish(4).validate("test", finish_complete),
            Ok(finish_complete)
        );
        assert!(ProcessCapacity::for_finish(4).is_finishing());
        assert_eq!(
            ProcessCapacity::in_place(4).mode(),
            ProcessBufferMode::InPlace
        );
    }

    #[test]
    fn streaming_processor_supports_in_place_out_of_place_and_bypass() {
        let mut processor = StreamingGain {
            enabled: true,
            gain: 0.5,
        };

        let mut in_place = [2.0, 4.0, 6.0, 8.0];
        let block = AudioBlockMut::new(&mut in_place, 2).unwrap();
        let progress = process_checked(&mut processor, ProcessBuffers::in_place(block)).unwrap();
        assert_eq!(progress.consumed_frames(), 2);
        assert_eq!(progress.produced_frames(), 2);
        assert!(!progress.is_bypassed());
        assert_eq!(in_place, [1.0, 2.0, 3.0, 4.0]);

        let input = [2.0, 4.0, 6.0, 8.0];
        let mut output = [0.0; 2];
        let buffers = ProcessBuffers::out_of_place(
            AudioBlockRef::new(&input, 2).unwrap(),
            AudioBlockMut::new(&mut output, 2).unwrap(),
        )
        .unwrap();
        let progress = process_checked(&mut processor, buffers).unwrap();
        assert_eq!(progress.state(), ProcessState::NeedOutput);
        assert_eq!(progress.consumed_frames(), 1);
        assert_eq!(output, [1.0, 2.0]);

        processor.set_enabled(false);
        let mut bypassed = [3.0, 5.0];
        let block = AudioBlockMut::new(&mut bypassed, 1).unwrap();
        let progress = process_checked(&mut processor, ProcessBuffers::in_place(block)).unwrap();
        assert!(progress.is_bypassed());
        assert_eq!(bypassed, [3.0, 5.0]);
    }

    #[test]
    fn default_finish_and_tail_contract_are_idempotent() {
        let mut processor = StreamingGain {
            enabled: true,
            gain: 0.5,
        };
        let mut output = [0.0; 4];

        for _ in 0..2 {
            let progress =
                finish_checked(&mut processor, AudioBlockMut::new(&mut output, 2).unwrap())
                    .unwrap();
            assert_eq!(progress, ProcessProgress::finished(0));
        }
        assert_eq!(processor.latency(), FrameDuration::ZERO);
        assert_eq!(processor.tail(), TailSpec::None);
        assert_eq!(TailSpec::finite(0, 48_000), Ok(TailSpec::None));
        assert_eq!(
            TailSpec::finite(12, 48_000)
                .unwrap()
                .finite_duration()
                .unwrap()
                .frames(),
            12
        );
        assert_eq!(TailSpec::Unknown.finite_duration(), None);
        assert_eq!(processor.output_sample_rate_hz(48_000), Ok(48_000));
        assert!(matches!(
            processor.output_sample_rate_hz(0),
            Err(ProcessError::InvalidSampleRate { .. })
        ));
    }

    #[test]
    fn frame_duration_carries_rate_and_uses_explicit_rounding() {
        let duration = FrameDuration::new(441, 44_100).unwrap();
        assert_eq!(duration.sample_rate_hz(), Some(44_100));
        assert_eq!(
            duration.rounded_frames_at_rate(48_000, FrameRounding::Nearest),
            Ok(480)
        );
        assert_eq!(duration.frames_at_rate_f64(48_000), Ok(480.0));

        let fractional = FrameDuration::new(1, 44_100).unwrap();
        assert_eq!(
            fractional.rounded_frames_at_rate(48_000, FrameRounding::Floor),
            Ok(1)
        );
        assert_eq!(
            fractional.rounded_frames_at_rate(48_000, FrameRounding::Nearest),
            Ok(1)
        );
        assert_eq!(
            fractional.rounded_frames_at_rate(48_000, FrameRounding::Ceil),
            Ok(2)
        );
        assert_eq!(
            fractional.rounded_frames_at_rate(0, FrameRounding::Nearest),
            Err(TimingError::ZeroSampleRate)
        );
        assert_eq!(FrameDuration::new(0, 0), Err(TimingError::ZeroSampleRate));
    }

    #[test]
    fn stateful_finish_drains_to_terminal_state_and_reset_rearms_stream() {
        let mut processor = FiniteTailProcessor::new(5, 48_000);
        let mut output = [0.0; 4];

        let first =
            finish_checked(&mut processor, AudioBlockMut::new(&mut output, 2).unwrap()).unwrap();
        assert_eq!(first, ProcessProgress::new(0, 2, ProcessState::NeedOutput));
        assert_eq!(output, [0.25; 4]);

        let second =
            finish_checked(&mut processor, AudioBlockMut::new(&mut output, 2).unwrap()).unwrap();
        assert_eq!(second, ProcessProgress::new(0, 2, ProcessState::NeedOutput));

        let third =
            finish_checked(&mut processor, AudioBlockMut::new(&mut output, 2).unwrap()).unwrap();
        assert_eq!(third, ProcessProgress::finished(1));

        for _ in 0..2 {
            let terminal =
                finish_checked(&mut processor, AudioBlockMut::new(&mut output, 2).unwrap())
                    .unwrap();
            assert_eq!(terminal, ProcessProgress::finished(0));
        }

        let mut input = [1.0, 2.0];
        let process_after_finish = process_checked(
            &mut processor,
            ProcessBuffers::in_place(AudioBlockMut::new(&mut input, 1).unwrap()),
        );
        assert_eq!(
            process_after_finish,
            Err(ProcessError::AlreadyFinished {
                processor: "FiniteTail"
            })
        );

        processor.reset().unwrap();
        let after_reset =
            finish_checked(&mut processor, AudioBlockMut::new(&mut output, 2).unwrap()).unwrap();
        assert_eq!(
            after_reset,
            ProcessProgress::new(0, 2, ProcessState::NeedOutput)
        );
        assert_eq!(
            processor.tail().finite_duration(),
            Some(FrameDuration::new(5, 48_000).unwrap())
        );
        assert_eq!(TailSpec::Unknown.finite_duration(), None);
        assert_eq!(TailSpec::Infinite.finite_duration(), None);
    }

    #[test]
    fn process_error_preserves_static_and_owned_backend_diagnostics() {
        let backend = ProcessError::Backend {
            processor: "resampler",
            operation: "drain",
            message: "native drain failed",
        };
        assert_eq!(
            backend.to_string(),
            "processor resampler failed during drain: native drain failed"
        );

        let owned = ProcessError::Owned {
            processor: "resampler",
            operation: "legacy process",
            message: String::from("channel 3 failed"),
        };
        assert_eq!(
            owned.to_string(),
            "processor resampler failed during legacy process: channel 3 failed"
        );
    }

    #[test]
    fn streaming_contract_hot_path_allocates_nothing() {
        let mut processor = StreamingGain {
            enabled: true,
            gain: 0.5,
        };
        let mut samples = [1.0; 8];
        let input = [1.0; 8];
        let mut output = [0.0; 8];
        let mut tail = FiniteTailProcessor::new(5, 48_000);
        let mut tail_output = [0.0; 4];

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..32 {
                let block = AudioBlockMut::new(&mut samples, 2).unwrap();
                let buffers = ProcessBuffers::in_place(block);
                let progress = process_checked(&mut processor, buffers).unwrap();
                assert_eq!(progress.produced_frames(), 4);

                let buffers = ProcessBuffers::out_of_place(
                    AudioBlockRef::new(&input, 2).unwrap(),
                    AudioBlockMut::new(&mut output, 2).unwrap(),
                )
                .unwrap();
                let progress = process_checked(&mut processor, buffers).unwrap();
                assert_eq!(progress.produced_frames(), 4);
            }

            assert_eq!(
                finish_checked(&mut tail, AudioBlockMut::new(&mut tail_output, 2).unwrap())
                    .unwrap()
                    .state(),
                ProcessState::NeedOutput
            );
            assert_eq!(
                finish_checked(&mut tail, AudioBlockMut::new(&mut tail_output, 2).unwrap())
                    .unwrap()
                    .state(),
                ProcessState::NeedOutput
            );
            assert_eq!(
                finish_checked(&mut tail, AudioBlockMut::new(&mut tail_output, 2).unwrap())
                    .unwrap()
                    .state(),
                ProcessState::Finished
            );
        });
    }

    #[test]
    fn streaming_processor_trait_is_object_safe() {
        fn exercise(processor: &mut dyn StreamingProcessor) {
            assert_eq!(processor.name(), "StreamingGain");
        }

        let mut processor = StreamingGain {
            enabled: true,
            gain: 1.0,
        };
        exercise(&mut processor);
    }
}
