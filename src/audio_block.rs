//! Borrowed interleaved audio-block geometry shared by analysis and processing.
//!
//! This module owns validation and zero-copy views for complete interleaved
//! `f64` blocks. It contains no processor lifecycle, callback state, or sample
//! transformation logic. Historical `crate::processor::traits::*` paths
//! re-export these types for compatibility.

use std::num::NonZeroUsize;

use thiserror::Error;

/// Validation failure for a borrowed interleaved audio block.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioBlockError {
    /// An interleaved block cannot describe frames without at least one channel.
    #[error("audio block channel count must be greater than zero")]
    ZeroChannels,
    /// The sample slice ends with an incomplete interleaved frame.
    #[error("interleaved sample count {samples} is not divisible by channel count {channels}")]
    IncompleteFrame {
        /// Number of interleaved samples supplied by the caller.
        samples: usize,
        /// Channel count used to validate the interleaved block.
        channels: usize,
    },
    /// An out-of-place call requires the same channel count on both sides.
    /// The caller supplied an interleaved buffer with invalid frame geometry.
    #[error(
        "input/output channel mismatch: input has {input_channels}, output has {output_channels}"
    )]
    ChannelMismatch {
        /// Channel count declared by the input view.
        input_channels: usize,
        /// Channel count declared by the output view.
        output_channels: usize,
    },
}

/// Validate a channel count and retain its non-zero invariant for block views.
pub(crate) fn validated_channel_count(channels: usize) -> Result<NonZeroUsize, AudioBlockError> {
    NonZeroUsize::new(channels).ok_or(AudioBlockError::ZeroChannels)
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
        let channels = validated_channel_count(channels)?;
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
        let channels = validated_channel_count(channels)?;
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
