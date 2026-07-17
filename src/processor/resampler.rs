//! High-quality resampling using SoX VHQ Polyphase implementation

use crate::config::{PhaseResponse, ResampleQuality};
use rayon::prelude::*;
use soxr::{
    format::Mono,
    params::{QualityFlags, QualityRecipe, QualitySpec, Rolloff, RuntimeSpec},
    Soxr,
};

use super::traits::{
    AudioBlockMut, AudioBlockRef, FrameDuration, ProcessBufferMode, ProcessBufferParts,
    ProcessBuffers, ProcessError, ProcessProgress, ProcessState, StreamingProcessor, TailSpec,
};

/// Error type for resampler operations
#[derive(Debug, Clone)]
pub enum ResamplerError {
    /// Soxr initialization failed (e.g., invalid sample rate combination)
    InitializationFailed(String),
    /// Processing failed
    ProcessFailed(String),
}

impl std::fmt::Display for ResamplerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResamplerError::InitializationFailed(msg) => {
                write!(f, "Soxr initialization failed: {}", msg)
            }
            ResamplerError::ProcessFailed(msg) => write!(f, "Resampling process failed: {}", msg),
        }
    }
}

impl std::error::Error for ResamplerError {}

/// High-quality resampler using SoX (VHQ Polyphase implementation)
pub struct Resampler {
    channels: usize,
    from_rate: u32,
    to_rate: u32,
}

/// Convert ResampleQuality enum to SoX QualityRecipe
/// FIX for Defect 30: Actually use different quality levels
/// Note: QualityRecipe has Low variant, plus high() and very_high() constructor functions
fn quality_to_recipe(quality: ResampleQuality) -> QualityRecipe {
    match quality {
        ResampleQuality::Low => QualityRecipe::Low, // Fast, lower quality (enum variant)
        ResampleQuality::Standard => QualityRecipe::high(), // High quality (constructor)
        ResampleQuality::High => QualityRecipe::high(), // High quality (constructor)
        ResampleQuality::UltraHigh => QualityRecipe::very_high(), // VHQ, slowest (constructor)
    }
}

/// Create a QualitySpec with the given recipe and phase response
fn make_quality_spec(recipe: QualityRecipe, phase: PhaseResponse) -> QualitySpec {
    QualitySpec::configure(recipe, Rolloff::default(), QualityFlags::HighPrecisionClock)
        .with_phase_response(phase.to_soxr_value())
}

fn deinterleave_frame_major(
    input: &[f64],
    channels: usize,
    frames: usize,
    channel_inputs: &mut [Vec<f64>],
) {
    for frame in input[..frames * channels].chunks_exact(channels) {
        for (ch, &sample) in frame.iter().enumerate() {
            channel_inputs[ch].push(sample);
        }
    }
}

fn channel_outputs_have_frames(
    channel_outputs: &[Vec<f64>],
    channels: usize,
    frames: usize,
) -> bool {
    channel_outputs
        .iter()
        .take(channels)
        .all(|channel| channel.len() >= frames)
}

fn interleave_channel_outputs_to_vec(
    channel_outputs: &[Vec<f64>],
    channels: usize,
    output: &mut Vec<f64>,
) -> usize {
    if channel_outputs.is_empty() || channel_outputs[0].is_empty() {
        output.clear();
        return 0;
    }

    let out_frames = channel_outputs[0].len();
    output.clear();
    output.reserve(out_frames * channels);

    if channel_outputs_have_frames(channel_outputs, channels, out_frames) {
        for frame in 0..out_frames {
            for channel in channel_outputs.iter().take(channels) {
                output.push(channel[frame]);
            }
        }
    } else {
        for frame in 0..out_frames {
            for channel in channel_outputs.iter().take(channels) {
                output.push(channel.get(frame).copied().unwrap_or(0.0));
            }
        }
    }

    out_frames
}

fn interleave_channel_outputs_to_slice(
    channel_outputs: &[Vec<f64>],
    channels: usize,
    output: &mut [f64],
) -> usize {
    if channel_outputs.is_empty() || channel_outputs[0].is_empty() {
        return 0;
    }

    let out_frames = channel_outputs[0].len();

    if output.len() >= out_frames * channels
        && channel_outputs_have_frames(channel_outputs, channels, out_frames)
    {
        for (frame, out_frame) in output
            .chunks_exact_mut(channels)
            .take(out_frames)
            .enumerate()
        {
            for (dst, channel) in out_frame
                .iter_mut()
                .zip(channel_outputs.iter().take(channels))
            {
                *dst = channel[frame];
            }
        }
    } else {
        for frame in 0..out_frames {
            for (ch, channel) in channel_outputs.iter().take(channels).enumerate() {
                let idx = frame * channels + ch;
                if idx < output.len() {
                    output[idx] = channel.get(frame).copied().unwrap_or(0.0);
                }
            }
        }
    }

    out_frames
}

impl Resampler {
    pub fn new(channels: usize, from_rate: u32, to_rate: u32) -> Self {
        Self {
            channels,
            from_rate,
            to_rate,
        }
    }

    /// Resample audio data using SoX VHQ polyphase filter.
    ///
    /// Input and output are interleaved f64 samples for Hi-Fi transparency.
    ///
    /// Optimised for multi-channel parallelism:
    /// - De-interleaves channels
    /// - Processes each channel on a separate thread (Rayon)
    /// - Re-interleaves result
    ///
    /// This avoids phase discontinuities from time-chunking while maintaining high performance.
    ///
    /// Returns Err if Soxr initialization fails (e.g., invalid sample rate combination).
    pub fn resample_parallel(
        &self,
        input: &[f64],
        phase: PhaseResponse,
        quality: ResampleQuality,
    ) -> Result<Vec<f64>, ResamplerError> {
        if self.from_rate == self.to_rate {
            return Ok(input.to_vec());
        }

        // Validate sample rates
        if self.from_rate == 0 || self.to_rate == 0 {
            return Err(ResamplerError::InitializationFailed(format!(
                "Invalid sample rate: from_rate={}, to_rate={}",
                self.from_rate, self.to_rate
            )));
        }

        // Guard channels==0: a zero channel count would divide by zero below.
        if self.channels == 0 {
            return Err(ResamplerError::InitializationFailed(
                "channel count must be >= 1".to_string(),
            ));
        }

        // 1. De-interleave
        let frames = input.len() / self.channels;
        let mut plan_channels: Vec<Vec<f64>> = vec![Vec::with_capacity(frames); self.channels];
        deinterleave_frame_major(input, self.channels, frames, &mut plan_channels);

        // 2. Process channels in parallel
        let resampled_channels: Result<Vec<Vec<f64>>, ResamplerError> = plan_channels
            .into_par_iter()
            .enumerate()
            .map(|(ch_idx, channel_data)| {
                // Configure SoX for this channel with phase response and quality
                // FIX for Defect 30: Use quality parameter instead of hardcoded very_high
                let quality_spec = make_quality_spec(quality_to_recipe(quality), phase);

                let runtime_spec = RuntimeSpec::new(1); // 1 channel per thread

                let mut soxr = Soxr::<Mono<f64>>::new_with_params(
                    self.from_rate as f64,
                    self.to_rate as f64,
                    quality_spec,
                    runtime_spec,
                )
                .map_err(|e| {
                    ResamplerError::InitializationFailed(format!("Channel {}: {:?}", ch_idx, e))
                })?;

                // Output estimation
                let expected_frames = (channel_data.len() as f64 * self.to_rate as f64
                    / self.from_rate as f64)
                    .ceil() as usize
                    + 100;
                let mut channel_output = Vec::with_capacity(expected_frames);

                // Chunked processing to avoid massive single-pass overhead
                // 8192 frames is a good balance for cache usage
                let inner_chunk_size = 8192;
                // Size scratch by the actual conversion ratio, not a fixed 1.5x.
                // Upsampling ratios above 1.5 (e.g. 44.1->96k = 2.18, 44.1->192k
                // = 4.35) produce more output than input per chunk; the old 1.5x
                // buffer silently truncated the surplus on every chunk. A margin
                // above the nominal ratio absorbs soxr's per-call batching, and
                // the flush below drains any residual backlog in a loop.
                let ratio = self.to_rate as f64 / self.from_rate as f64;
                let scratch_frames = (inner_chunk_size as f64 * ratio).ceil() as usize + 64;
                let mut output_scratch = vec![0.0; scratch_frames];

                let total_chunks = channel_data.len() / inner_chunk_size + 1;

                // Log only for first channel to avoid spam
                if ch_idx == 0 {
                    log::info!(
                        "Starting resampling on thread. Total chunks: {}, Phase: {:?}",
                        total_chunks,
                        phase
                    );
                }

                for (i, chunk) in channel_data.chunks(inner_chunk_size).enumerate() {
                    let mut input_offset = 0;
                    while input_offset < chunk.len() {
                        let processed = soxr
                            .process(&chunk[input_offset..], &mut output_scratch)
                            .map_err(|e| {
                            ResamplerError::ProcessFailed(format!(
                                "Channel {} chunk {}: {:?}",
                                ch_idx, i, e
                            ))
                        })?;

                        if processed.input_frames > chunk.len() - input_offset
                            || processed.output_frames > output_scratch.len()
                        {
                            return Err(ResamplerError::ProcessFailed(format!(
                                "Channel {} chunk {} returned out-of-bounds progress",
                                ch_idx, i
                            )));
                        }

                        if processed.input_frames == 0 && processed.output_frames == 0 {
                            return Err(ResamplerError::ProcessFailed(format!(
                                "Channel {} chunk {} made no progress",
                                ch_idx, i
                            )));
                        }
                        input_offset += processed.input_frames;
                        channel_output
                            .extend_from_slice(&output_scratch[..processed.output_frames]);
                    }

                    // Periodic log check (every ~10%)
                    if ch_idx == 0 && i > 0 && i % (total_chunks.max(10) / 10).max(1) == 0 {
                        log::debug!("Resampling progress: {}%", i * 100 / total_chunks);
                    }
                }

                // Native drain is the only end-of-stream operation guaranteed
                // by Soxr. Keep calling it until it reports terminal zero.
                loop {
                    match soxr.drain(&mut output_scratch) {
                        Ok(output_frames) if output_frames > 0 => {
                            if output_frames > output_scratch.len() {
                                return Err(ResamplerError::ProcessFailed(format!(
                                    "Channel {} drain returned out-of-bounds progress",
                                    ch_idx
                                )));
                            }
                            channel_output.extend_from_slice(&output_scratch[..output_frames]);
                        }
                        Ok(_) => break,
                        Err(e) => {
                            return Err(ResamplerError::ProcessFailed(format!(
                                "Channel {} drain: {:?}",
                                ch_idx, e
                            )));
                        }
                    }
                }

                Ok(channel_output)
            })
            .collect();

        let resampled_channels = resampled_channels?;

        // 3. Re-interleave
        if resampled_channels.is_empty() {
            return Ok(Vec::new());
        }

        let mut final_output = Vec::with_capacity(resampled_channels[0].len() * self.channels);
        interleave_channel_outputs_to_vec(&resampled_channels, self.channels, &mut final_output);

        Ok(final_output)
    }
}

/// Stateful streaming resampler that maintains SoX instances across chunks.
/// This is used by AudioPipeline for memory-efficient streaming resampling.
///
/// FIX for Defect 33: Pre-allocate all buffers to avoid heap allocation in process.
pub struct StreamingResampler {
    soxr_instances: Vec<Soxr<Mono<f64>>>,
    channels: usize,
    from_rate: u32,
    to_rate: u32,
    /// Pre-allocated output scratch buffer (per channel, reused)
    output_scratch: Vec<f64>,
    /// Pre-allocated channel input buffers (Defect 33 fix)
    channel_inputs: Vec<Vec<f64>>,
    /// Pre-allocated channel output buffers (Defect 33 fix)
    channel_outputs: Vec<Vec<f64>>,
    /// End-of-stream has been signalled, but native drain may still have data.
    finishing: bool,
    /// Native drain reached terminal zero. Stable until reset.
    finished: bool,
}

const STREAMING_MAX_INPUT_FRAMES: usize = 16_384;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StreamingBufferLayout {
    max_output_per_channel: usize,
    pcm_bytes: usize,
}

fn streaming_buffer_layout(
    channels: usize,
    from_rate: u32,
    to_rate: u32,
) -> Result<StreamingBufferLayout, ResamplerError> {
    if channels == 0 || from_rate == 0 || to_rate == 0 {
        return Err(ResamplerError::InitializationFailed(format!(
            "Invalid streaming resampler geometry: channels={}, from_rate={}, to_rate={}",
            channels, from_rate, to_rate
        )));
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let max_output_per_channel = (STREAMING_MAX_INPUT_FRAMES as f64 * ratio).ceil() as usize + 64;
    let input_samples = channels
        .checked_mul(STREAMING_MAX_INPUT_FRAMES)
        .ok_or_else(|| ResamplerError::InitializationFailed("input capacity overflow".into()))?;
    let channel_output_samples = channels
        .checked_mul(max_output_per_channel)
        .ok_or_else(|| ResamplerError::InitializationFailed("output capacity overflow".into()))?;
    let total_samples = max_output_per_channel
        .checked_add(input_samples)
        .and_then(|samples| samples.checked_add(channel_output_samples))
        .ok_or_else(|| ResamplerError::InitializationFailed("working-set overflow".into()))?;
    let pcm_bytes = total_samples
        .checked_mul(std::mem::size_of::<f64>())
        .ok_or_else(|| ResamplerError::InitializationFailed("working-set byte overflow".into()))?;
    Ok(StreamingBufferLayout {
        max_output_per_channel,
        pcm_bytes,
    })
}

impl StreamingResampler {
    /// Exact `f64` capacity bytes allocated by the reusable streaming buffers.
    /// Callers can reserve this amount before constructing the resampler.
    pub fn working_buffer_bytes(
        channels: usize,
        from_rate: u32,
        to_rate: u32,
    ) -> Result<usize, ResamplerError> {
        Ok(streaming_buffer_layout(channels, from_rate, to_rate)?.pcm_bytes)
    }

    pub fn from_rate(&self) -> u32 {
        self.from_rate
    }

    pub fn to_rate(&self) -> u32 {
        self.to_rate
    }

    /// Conservative interleaved output capacity for one caller input block.
    /// Backpressure remains authoritative; callers must still advance using
    /// [`ProcessProgress`] rather than assuming the estimate consumes all input.
    pub fn max_output_len_for_input(&self, input_samples: usize) -> usize {
        if self.channels == 0 {
            return 0;
        }
        let input_frames = input_samples / self.channels;
        let ratio = self.to_rate as f64 / self.from_rate as f64;
        (input_frames as f64 * ratio).ceil() as usize * self.channels + self.channels * 64
    }

    /// Maximum interleaved output produced by one internal Soxr step.
    pub fn max_output_samples_per_chunk(&self) -> usize {
        self.output_scratch.len() * self.channels
    }

    pub fn input_frames_for_output_frames(&self, output_frames: usize) -> usize {
        if output_frames == 0 || self.to_rate == 0 {
            return 0;
        }

        let ratio = self.from_rate as f64 / self.to_rate as f64;
        (output_frames as f64 * ratio).ceil() as usize + 64
    }

    /// Create a new streaming resampler with default (linear) phase and High quality
    pub fn new(channels: usize, from_rate: u32, to_rate: u32) -> Result<Self, ResamplerError> {
        Self::with_phase(channels, from_rate, to_rate, PhaseResponse::default())
    }

    /// Create a new streaming resampler with specified phase response (High quality)
    ///
    /// Returns Err if Soxr initialization fails (e.g., invalid sample rates like 0 Hz)
    pub fn with_phase(
        channels: usize,
        from_rate: u32,
        to_rate: u32,
        phase: PhaseResponse,
    ) -> Result<Self, ResamplerError> {
        Self::with_quality(channels, from_rate, to_rate, phase, ResampleQuality::High)
    }

    /// Create a new streaming resampler with specified phase response and quality level
    ///
    /// FIX for Defect 30: Allow quality configuration
    /// FIX for Defect 33: Pre-allocate all buffers to avoid heap allocation in process.
    ///
    /// Returns Err if Soxr initialization fails (e.g., invalid sample rates like 0 Hz)
    pub fn with_quality(
        channels: usize,
        from_rate: u32,
        to_rate: u32,
        phase: PhaseResponse,
        quality: ResampleQuality,
    ) -> Result<Self, ResamplerError> {
        // Validate sample rates before creating Soxr instances
        if from_rate == 0 || to_rate == 0 {
            return Err(ResamplerError::InitializationFailed(format!(
                "Invalid sample rate: from_rate={}, to_rate={}",
                from_rate, to_rate
            )));
        }

        // Guard channels==0: interleaved frame handling requires a non-zero divisor.
        if channels == 0 {
            return Err(ResamplerError::InitializationFailed(
                "channel count must be >= 1".to_string(),
            ));
        }

        let mut soxr_instances = Vec::with_capacity(channels);
        for ch_idx in 0..channels {
            // Create params for each channel with phase response and quality
            // FIX for Defect 30: Use quality parameter
            let quality_spec = make_quality_spec(quality_to_recipe(quality), phase);
            let runtime_spec = RuntimeSpec::new(1);

            match Soxr::<Mono<f64>>::new_with_params(
                from_rate as f64,
                to_rate as f64,
                quality_spec,
                runtime_spec,
            ) {
                Ok(soxr) => soxr_instances.push(soxr),
                Err(e) => {
                    return Err(ResamplerError::InitializationFailed(format!(
                        "Soxr failed for channel {}: {:?} (from={}Hz, to={}Hz)",
                        ch_idx, e, from_rate, to_rate
                    )));
                }
            }
        }

        // Pre-allocate all buffers from the same checked layout exposed to
        // callers for reservation-before-allocation accounting.
        let layout = streaming_buffer_layout(channels, from_rate, to_rate)?;
        let max_output_per_channel = layout.max_output_per_channel;

        // Pre-allocate channel buffers
        let channel_inputs: Vec<Vec<f64>> = (0..channels)
            .map(|_| Vec::with_capacity(STREAMING_MAX_INPUT_FRAMES))
            .collect();
        let channel_outputs: Vec<Vec<f64>> = (0..channels)
            .map(|_| Vec::with_capacity(max_output_per_channel))
            .collect();
        Ok(Self {
            soxr_instances,
            channels,
            from_rate,
            to_rate,
            output_scratch: vec![0.0; max_output_per_channel],
            channel_inputs,
            channel_outputs,
            finishing: false,
            finished: false,
        })
    }

    fn clear_work_buffers(&mut self) {
        for ch_buf in &mut self.channel_inputs {
            ch_buf.clear();
        }
        for ch_buf in &mut self.channel_outputs {
            ch_buf.clear();
        }
    }

    fn validate_channels(&self, actual_channels: usize) -> Result<(), ProcessError> {
        if actual_channels == self.channels {
            Ok(())
        } else {
            Err(ProcessError::ChannelCountMismatch {
                processor: "StreamingResampler",
                expected_channels: self.channels,
                actual_channels,
            })
        }
    }

    fn ensure_processing(&self) -> Result<(), ProcessError> {
        if self.finishing || self.finished {
            Err(ProcessError::AlreadyFinished {
                processor: "StreamingResampler",
            })
        } else {
            Ok(())
        }
    }

    fn process_out_of_place(
        &mut self,
        input: AudioBlockRef<'_>,
        mut output: AudioBlockMut<'_>,
    ) -> Result<ProcessProgress, ProcessError> {
        let channels = self.channels;
        let input_frames = input.frames();
        let output_frames = output.frames();

        if self.from_rate == self.to_rate {
            let frames = input_frames.min(output_frames);
            let samples = frames * channels;
            output.samples_mut()[..samples].copy_from_slice(&input.samples()[..samples]);
            let state = if frames < input_frames {
                ProcessState::NeedOutput
            } else {
                ProcessState::NeedInput
            };
            return Ok(ProcessProgress::new(frames, frames, state).with_bypassed(true));
        }

        let input_samples = input.samples();
        let output_samples = output.samples_mut();
        let mut consumed_frames = 0;
        let mut produced_frames = 0;

        while consumed_frames < input_frames && produced_frames < output_frames {
            let input_step_frames =
                (input_frames - consumed_frames).min(STREAMING_MAX_INPUT_FRAMES);
            let output_step_capacity =
                (output_frames - produced_frames).min(self.output_scratch.len());

            self.clear_work_buffers();
            let input_start = consumed_frames * channels;
            let input_end = input_start + input_step_frames * channels;
            deinterleave_frame_major(
                &input_samples[input_start..input_end],
                channels,
                input_step_frames,
                &mut self.channel_inputs,
            );

            let mut shared_progress = None;
            for channel in 0..channels {
                let processed = self.soxr_instances[channel]
                    .process(
                        &self.channel_inputs[channel],
                        &mut self.output_scratch[..output_step_capacity],
                    )
                    .map_err(|_| ProcessError::Backend {
                        processor: "StreamingResampler",
                        operation: "process",
                        message: "soxr process failed",
                    })?;

                if processed.input_frames > input_step_frames
                    || processed.output_frames > output_step_capacity
                {
                    return Err(ProcessError::Backend {
                        processor: "StreamingResampler",
                        operation: "process",
                        message: "soxr returned out-of-bounds progress",
                    });
                }

                match shared_progress {
                    None => {
                        shared_progress = Some((processed.input_frames, processed.output_frames));
                    }
                    Some(expected)
                        if expected != (processed.input_frames, processed.output_frames) =>
                    {
                        return Err(ProcessError::Backend {
                            processor: "StreamingResampler",
                            operation: "process",
                            message: "soxr channel progress diverged",
                        });
                    }
                    Some(_) => {}
                }

                self.channel_outputs[channel]
                    .extend_from_slice(&self.output_scratch[..processed.output_frames]);
            }

            let Some((step_consumed, step_produced)) = shared_progress else {
                return Err(ProcessError::Backend {
                    processor: "StreamingResampler",
                    operation: "process",
                    message: "soxr channel set was empty",
                });
            };
            if step_consumed == 0 && step_produced == 0 {
                return Err(ProcessError::Backend {
                    processor: "StreamingResampler",
                    operation: "process",
                    message: "soxr made no progress",
                });
            }

            let output_start = produced_frames * channels;
            let output_end = output_start + step_produced * channels;
            let written = interleave_channel_outputs_to_slice(
                &self.channel_outputs,
                channels,
                &mut output_samples[output_start..output_end],
            );
            if written != step_produced {
                return Err(ProcessError::Backend {
                    processor: "StreamingResampler",
                    operation: "process",
                    message: "failed to interleave complete soxr output",
                });
            }

            consumed_frames += step_consumed;
            produced_frames += step_produced;
        }

        let state = if consumed_frames == input_frames {
            ProcessState::NeedInput
        } else if produced_frames == output_frames {
            ProcessState::NeedOutput
        } else {
            return Err(ProcessError::Backend {
                processor: "StreamingResampler",
                operation: "process",
                message: "soxr stopped before reaching an input/output boundary",
            });
        };
        Ok(ProcessProgress::new(
            consumed_frames,
            produced_frames,
            state,
        ))
    }

    fn drain_into(
        &mut self,
        mut output: AudioBlockMut<'_>,
    ) -> Result<ProcessProgress, ProcessError> {
        if self.finished {
            return Ok(ProcessProgress::finished(0));
        }
        self.finishing = true;

        let channels = self.channels;
        let output_frames = output.frames();
        if output_frames == 0 {
            return Ok(ProcessProgress::new(0, 0, ProcessState::NeedOutput));
        }

        let output_samples = output.samples_mut();
        let mut produced_frames = 0;
        while produced_frames < output_frames {
            let output_step_capacity =
                (output_frames - produced_frames).min(self.output_scratch.len());
            for channel_output in &mut self.channel_outputs {
                channel_output.clear();
            }

            let mut shared_output_frames = None;
            for channel in 0..channels {
                let channel_frames = self.soxr_instances[channel]
                    .drain(&mut self.output_scratch[..output_step_capacity])
                    .map_err(|_| ProcessError::Backend {
                        processor: "StreamingResampler",
                        operation: "finish",
                        message: "soxr drain failed",
                    })?;
                if channel_frames > output_step_capacity {
                    return Err(ProcessError::Backend {
                        processor: "StreamingResampler",
                        operation: "finish",
                        message: "soxr drain returned out-of-bounds progress",
                    });
                }
                match shared_output_frames {
                    None => shared_output_frames = Some(channel_frames),
                    Some(expected) if expected != channel_frames => {
                        return Err(ProcessError::Backend {
                            processor: "StreamingResampler",
                            operation: "finish",
                            message: "soxr channel drain diverged",
                        });
                    }
                    Some(_) => {}
                }
                self.channel_outputs[channel]
                    .extend_from_slice(&self.output_scratch[..channel_frames]);
            }

            let Some(step_produced) = shared_output_frames else {
                return Err(ProcessError::Backend {
                    processor: "StreamingResampler",
                    operation: "finish",
                    message: "soxr channel set was empty",
                });
            };
            if step_produced == 0 {
                self.finished = true;
                return Ok(ProcessProgress::finished(produced_frames));
            }

            let output_start = produced_frames * channels;
            let output_end = output_start + step_produced * channels;
            let written = interleave_channel_outputs_to_slice(
                &self.channel_outputs,
                channels,
                &mut output_samples[output_start..output_end],
            );
            if written != step_produced {
                return Err(ProcessError::Backend {
                    processor: "StreamingResampler",
                    operation: "finish",
                    message: "failed to interleave complete soxr drain output",
                });
            }
            produced_frames += step_produced;
        }

        Ok(ProcessProgress::new(
            0,
            produced_frames,
            ProcessState::NeedOutput,
        ))
    }
}

impl StreamingProcessor for StreamingResampler {
    fn name(&self) -> &'static str {
        "StreamingResampler"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.ensure_processing()?;
        self.validate_channels(buffers.channels())?;

        match buffers.into_parts() {
            ProcessBufferParts::InPlace(block) if self.from_rate == self.to_rate => Ok(
                ProcessProgress::new(block.frames(), block.frames(), ProcessState::NeedInput)
                    .with_bypassed(true),
            ),
            ProcessBufferParts::InPlace(_) => Err(ProcessError::UnsupportedBufferMode {
                processor: "StreamingResampler",
                mode: ProcessBufferMode::InPlace,
            }),
            ProcessBufferParts::OutOfPlace { input, output } => {
                self.process_out_of_place(input, output)
            }
        }
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        self.validate_channels(output.channels())?;
        if self.from_rate == self.to_rate {
            self.finishing = true;
            self.finished = true;
            return Ok(ProcessProgress::finished(0));
        }
        self.drain_into(output)
    }

    fn reset(&mut self) -> Result<(), ProcessError> {
        let mut first_error = None;
        for soxr in &mut self.soxr_instances {
            if soxr.clear().is_err() && first_error.is_none() {
                first_error = Some(ProcessError::Backend {
                    processor: "StreamingResampler",
                    operation: "reset",
                    message: "soxr clear failed",
                });
            }
        }
        self.clear_work_buffers();
        self.finishing = false;
        self.finished = false;
        first_error.map_or(Ok(()), Err)
    }

    fn latency(&self) -> FrameDuration {
        // Soxr with native drain returns a duration-aligned sample sequence;
        // its internal scheduling delay does not add leading frames to that
        // sequence and therefore requires no offline timeline crop.
        FrameDuration::ZERO
    }

    fn tail(&self) -> TailSpec {
        TailSpec::None
    }

    fn is_enabled(&self) -> bool {
        self.from_rate != self.to_rate
    }

    fn set_enabled(&mut self, _enabled: bool) {
        // Rate conversion is graph geometry rather than a bypassable effect.
    }

    fn output_sample_rate_hz(&self, input_sample_rate_hz: u32) -> Result<u32, ProcessError> {
        if input_sample_rate_hz == 0 {
            return Err(ProcessError::InvalidSampleRate {
                processor: "StreamingResampler",
                sample_rate_hz: input_sample_rate_hz,
            });
        }
        if input_sample_rate_hz != self.from_rate {
            return Err(ProcessError::SampleRateMismatch {
                processor: "StreamingResampler",
                expected_sample_rate_hz: self.from_rate,
                actual_sample_rate_hz: input_sample_rate_hz,
            });
        }
        Ok(self.to_rate)
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        if sample_rate_hz != self.from_rate {
            return Err(ProcessError::SampleRateMismatch {
                processor: "StreamingResampler",
                expected_sample_rate_hz: self.from_rate,
                actual_sample_rate_hz: sample_rate_hz,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processor::traits::{finish_checked, process_checked, AudioBlockError};

    fn fixture(frames: usize, channels: usize) -> Vec<f64> {
        (0..frames * channels)
            .map(|sample| {
                let frame = sample / channels;
                let channel = sample % channels;
                ((frame as f64 * 0.017 + channel as f64 * 0.31).sin() * 0.5)
                    + ((frame as f64 * 0.003).cos() * 0.1)
            })
            .collect()
    }

    fn render_with_chunks(
        resampler: &mut StreamingResampler,
        input: &[f64],
        input_chunks: &[usize],
        output_block_frames: usize,
    ) -> Result<Vec<f64>, ProcessError> {
        let channels = resampler.channels;
        let total_frames = AudioBlockRef::new(input, channels)?.frames();
        let mut scratch = vec![0.0; output_block_frames * channels];
        let mut output = Vec::new();
        let mut frame = 0;
        let mut chunk_index = 0;

        while frame < total_frames {
            let chunk_frames = input_chunks[chunk_index % input_chunks.len()].max(1);
            let chunk_end = (frame + chunk_frames).min(total_frames);
            let mut chunk_offset = frame;
            while chunk_offset < chunk_end {
                let input_block = AudioBlockRef::new(
                    &input[chunk_offset * channels..chunk_end * channels],
                    channels,
                )?;
                let output_block = AudioBlockMut::new(&mut scratch, channels)?;
                let buffers = ProcessBuffers::out_of_place(input_block, output_block)?;
                let progress = process_checked(resampler, buffers)?;
                output.extend_from_slice(
                    &scratch[..progress.produced_frames().saturating_mul(channels)],
                );
                chunk_offset += progress.consumed_frames();
            }
            frame = chunk_end;
            chunk_index += 1;
        }

        loop {
            let output_block = AudioBlockMut::new(&mut scratch, channels)?;
            let progress = finish_checked(resampler, output_block)?;
            output
                .extend_from_slice(&scratch[..progress.produced_frames().saturating_mul(channels)]);
            if progress.state() == ProcessState::Finished {
                break;
            }
        }
        Ok(output)
    }

    #[test]
    fn streaming_resampler_rejects_invalid_geometry() {
        for (channels, from_rate, to_rate) in [(0, 48_000, 96_000), (2, 0, 96_000), (2, 48_000, 0)]
        {
            assert!(matches!(
                StreamingResampler::new(channels, from_rate, to_rate),
                Err(ResamplerError::InitializationFailed(_))
            ));
        }
    }

    #[test]
    fn working_buffer_bytes_matches_internal_capacities() {
        let resampler = StreamingResampler::new(6, 44_100, 192_000).unwrap();
        let actual_samples = resampler.output_scratch.capacity()
            + resampler
                .channel_inputs
                .iter()
                .map(Vec::capacity)
                .sum::<usize>()
            + resampler
                .channel_outputs
                .iter()
                .map(Vec::capacity)
                .sum::<usize>();
        assert_eq!(
            StreamingResampler::working_buffer_bytes(6, 44_100, 192_000).unwrap(),
            actual_samples * std::mem::size_of::<f64>()
        );
    }

    #[test]
    fn equal_rate_supports_in_place_bypass_and_lifecycle() {
        let mut resampler = StreamingResampler::new(2, 48_000, 48_000).unwrap();
        let mut samples = fixture(32, 2);
        let expected = samples.clone();
        let progress = process_checked(
            &mut resampler,
            ProcessBuffers::in_place(AudioBlockMut::new(&mut samples, 2).unwrap()),
        )
        .unwrap();
        assert!(progress.is_bypassed());
        assert_eq!(samples, expected);

        let mut output = [0.0; 8];
        assert_eq!(
            finish_checked(&mut resampler, AudioBlockMut::new(&mut output, 2).unwrap()).unwrap(),
            ProcessProgress::finished(0)
        );
        assert!(matches!(
            process_checked(
                &mut resampler,
                ProcessBuffers::in_place(AudioBlockMut::new(&mut samples, 2).unwrap())
            ),
            Err(ProcessError::AlreadyFinished { .. })
        ));
        resampler.reset().unwrap();
        let _ = process_checked(
            &mut resampler,
            ProcessBuffers::in_place(AudioBlockMut::new(&mut samples, 2).unwrap()),
        )
        .unwrap();
    }

    #[test]
    fn variable_rate_rejects_in_place_and_channel_mismatch() {
        let mut resampler = StreamingResampler::new(2, 48_000, 96_000).unwrap();
        let mut samples = fixture(16, 2);
        assert!(matches!(
            process_checked(
                &mut resampler,
                ProcessBuffers::in_place(AudioBlockMut::new(&mut samples, 2).unwrap())
            ),
            Err(ProcessError::UnsupportedBufferMode { .. })
        ));

        let input = AudioBlockRef::new(&samples, 2).unwrap();
        let mut output = [0.0; 16];
        assert_eq!(
            ProcessBuffers::out_of_place(input, AudioBlockMut::new(&mut output, 1).unwrap())
                .unwrap_err(),
            AudioBlockError::ChannelMismatch {
                input_channels: 2,
                output_channels: 1,
            }
        );
    }

    #[test]
    fn short_and_long_integer_upsampling_consume_every_frame() {
        for frames in [512, 100_000] {
            let input = fixture(frames, 2);
            let mut resampler = StreamingResampler::new(2, 48_000, 96_000).unwrap();
            let output = render_with_chunks(&mut resampler, &input, &[frames], 257).unwrap();
            assert_eq!(output.len() / 2, frames * 2);
        }
    }

    #[test]
    fn random_input_chunking_matches_single_feed() {
        let input = fixture(32_777, 2);
        let mut whole = StreamingResampler::new(2, 48_000, 96_000).unwrap();
        let mut chunked = StreamingResampler::new(2, 48_000, 96_000).unwrap();
        let expected = render_with_chunks(&mut whole, &input, &[input.len() / 2], 509).unwrap();
        let actual =
            render_with_chunks(&mut chunked, &input, &[1, 17, 3, 251, 64, 5, 1024, 7], 509)
                .unwrap();
        assert_eq!(actual.len(), expected.len());
        let max_error = actual
            .iter()
            .zip(&expected)
            .map(|(left, right)| (left - right).abs())
            .fold(0.0_f64, f64::max);
        assert!(max_error <= 1.0e-12, "chunking error {max_error:e}");
    }

    #[test]
    fn native_drain_returns_a_duration_aligned_impulse_sequence() {
        let input_frames = 4_096;
        let impulse_frame = 1_024;
        let mut input = vec![0.0; input_frames];
        input[impulse_frame] = 1.0;
        let mut resampler = StreamingResampler::new(1, 48_000, 96_000).unwrap();
        let output = render_with_chunks(&mut resampler, &input, &[127, 509], 257).unwrap();
        let peak_frame = output
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| {
                left.abs()
                    .partial_cmp(&right.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(frame, _)| frame)
            .unwrap();

        assert_eq!(output.len(), input_frames * 2);
        assert!(
            peak_frame.abs_diff(impulse_frame * 2) <= 1,
            "resampled impulse peak landed at {peak_frame}, expected {}",
            impulse_frame * 2
        );
        assert_eq!(resampler.latency(), FrameDuration::ZERO);
    }

    #[test]
    fn finish_is_terminal_idempotent_and_reset_clears_native_history() {
        let impulse = {
            let mut samples = vec![0.0; 2_048 * 2];
            samples[0] = 0.8;
            samples[1] = -0.8;
            samples
        };
        let silence = vec![0.0; 2_048 * 2];
        let mut reused = StreamingResampler::new(2, 48_000, 96_000).unwrap();
        let _ = render_with_chunks(&mut reused, &impulse, &[127], 257).unwrap();

        let mut terminal_buffer = vec![0.0; 64 * 2];
        let terminal = finish_checked(
            &mut reused,
            AudioBlockMut::new(&mut terminal_buffer, 2).unwrap(),
        )
        .unwrap();
        assert_eq!(terminal, ProcessProgress::finished(0));

        reused.reset().unwrap();
        let actual = render_with_chunks(&mut reused, &silence, &[31, 257], 257).unwrap();
        let mut fresh = StreamingResampler::new(2, 48_000, 96_000).unwrap();
        let expected = render_with_chunks(&mut fresh, &silence, &[31, 257], 257).unwrap();
        assert_eq!(actual.len(), expected.len());
        assert!(actual
            .iter()
            .zip(expected)
            .all(|(left, right)| left.to_bits() == right.to_bits()));
    }

    #[test]
    fn streaming_process_and_finish_do_not_allocate_after_setup() {
        let input = fixture(512, 2);
        let mut output = vec![0.0; 2_048 * 2];
        let mut resampler = StreamingResampler::new(2, 48_000, 96_000).unwrap();

        assert_no_alloc::assert_no_alloc(|| {
            let input_block = AudioBlockRef::new(&input, 2).unwrap();
            let output_block = AudioBlockMut::new(&mut output, 2).unwrap();
            let progress = process_checked(
                &mut resampler,
                ProcessBuffers::out_of_place(input_block, output_block).unwrap(),
            )
            .unwrap();
            assert_eq!(progress.consumed_frames(), 512);

            loop {
                let output_block = AudioBlockMut::new(&mut output, 2).unwrap();
                let progress = finish_checked(&mut resampler, output_block).unwrap();
                if progress.state() == ProcessState::Finished {
                    break;
                }
            }
        });
    }

    #[test]
    fn one_shot_parallel_matches_unified_streaming_length() {
        let input = fixture(24_000, 2);
        let parallel = Resampler::new(2, 44_100, 192_000)
            .resample_parallel(&input, PhaseResponse::Linear, ResampleQuality::High)
            .unwrap();
        let mut streaming = StreamingResampler::with_quality(
            2,
            44_100,
            192_000,
            PhaseResponse::Linear,
            ResampleQuality::High,
        )
        .unwrap();
        let streamed = render_with_chunks(&mut streaming, &input, &[8_192], 1_023).unwrap();
        assert_eq!(parallel.len(), streamed.len());
    }

    #[test]
    fn output_rate_mapping_is_explicit() {
        let resampler = StreamingResampler::new(2, 44_100, 48_000).unwrap();
        assert_eq!(resampler.output_sample_rate_hz(44_100), Ok(48_000));
        assert!(matches!(
            resampler.output_sample_rate_hz(48_000),
            Err(ProcessError::SampleRateMismatch { .. })
        ));
    }
}
