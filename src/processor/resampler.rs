//! High-quality resampling using SoX VHQ Polyphase implementation

use crate::config::{PhaseResponse, ResampleQuality};
use rayon::prelude::*;
use soxr::{
    format::Mono,
    params::{QualityFlags, QualityRecipe, QualitySpec, Rolloff, RuntimeSpec},
    Soxr,
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

fn interleave_channel_outputs_to_vec_with_max_frames(
    channel_outputs: &[Vec<f64>],
    channels: usize,
    output: &mut Vec<f64>,
) -> usize {
    let out_frames = channel_outputs
        .iter()
        .take(channels)
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    output.clear();
    if out_frames == 0 {
        return 0;
    }

    output.reserve(out_frames * channels);
    for frame in 0..out_frames {
        for channel in channel_outputs.iter().take(channels) {
            output.push(channel.get(frame).copied().unwrap_or(0.0));
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
                let mut output_scratch = vec![0.0; (inner_chunk_size as f64 * 1.5) as usize]; // Spare room for resampling ratio

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
                    let processed = soxr.process(chunk, &mut output_scratch).map_err(|e| {
                        ResamplerError::ProcessFailed(format!(
                            "Channel {} chunk {}: {:?}",
                            ch_idx, i, e
                        ))
                    })?;

                    if processed.output_frames > 0 {
                        channel_output
                            .extend_from_slice(&output_scratch[..processed.output_frames]);
                    }

                    // Periodic log check (every ~10%)
                    if ch_idx == 0 && i > 0 && i % (total_chunks.max(10) / 10).max(1) == 0 {
                        log::debug!("Resampling progress: {}%", i * 100 / total_chunks);
                    }
                }

                // Flush the resampler (pass empty slice)
                let mut flush_scratch = vec![0.0; 4096];
                if let Ok(processed) = soxr.process(&[], &mut flush_scratch) {
                    if processed.output_frames > 0 {
                        channel_output.extend_from_slice(&flush_scratch[..processed.output_frames]);
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
/// FIX for Defect 33: Pre-allocate all buffers to avoid heap allocation in process_chunk
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
    /// Pre-allocated interleaved output buffer (Defect 33 fix)
    interleaved_output: Vec<f64>,
}

pub struct ResampleOutput<'a> {
    pub samples: &'a [f64],
    pub frames: usize,
}

impl StreamingResampler {
    pub fn from_rate(&self) -> u32 {
        self.from_rate
    }

    pub fn to_rate(&self) -> u32 {
        self.to_rate
    }

    /// Naive ratio upper bound, in interleaved samples, on the output a single
    /// `process_chunk_*` call can return.
    ///
    /// Per-call Soxr output is now capped (via slicing the pre-allocated scratch
    /// to this bound) inside `process_chunk_*`, so this is a *valid* single-call
    /// ceiling: any excess input is buffered by Soxr and recovered on later
    /// calls. Callers reserving an output/leftover buffer for one render-loop
    /// input chunk should size it with this method.
    pub fn max_output_len_for_input(&self, input_samples: usize) -> usize {
        if self.channels == 0 {
            return 0;
        }
        let input_frames = input_samples / self.channels;
        let ratio = self.to_rate as f64 / self.from_rate as f64;
        (input_frames as f64 * ratio).ceil() as usize * self.channels + self.channels * 64
    }

    /// Absolute hard upper bound, in interleaved samples, on the output a single
    /// `process_chunk_*` call can return for this resampler.
    ///
    /// This equals the pre-allocated `interleaved_output` capacity. With per-call
    /// output now capped to the naive ratio bound (see `max_output_len_for_input`),
    /// `max_output_len_for_input` is the right size for per-call reserves; this
    /// method remains the absolute ceiling tied to the internal buffer capacity.
    pub fn max_output_samples_per_chunk(&self) -> usize {
        self.interleaved_output.capacity()
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
    /// FIX for Defect 33: Pre-allocate all buffers to avoid heap allocation in process_chunk
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

        // Pre-allocate all buffers (Defect 33 fix)
        let max_input_frames = 16384; // Typical chunk size
                                      // True conversion ratio. Per-call Soxr output is now capped (via slicing)
                                      // to the naive ratio bound, so this sizes the scratch/output buffers to the
                                      // real worst case for this direction (downsample ratios < 1 no longer
                                      // over-reserve the old conservative 2.0).
        let max_ratio = to_rate as f64 / from_rate as f64;
        let max_output_per_channel = (max_input_frames as f64 * max_ratio).ceil() as usize + 64;

        // Pre-allocate channel buffers
        let channel_inputs: Vec<Vec<f64>> = (0..channels)
            .map(|_| Vec::with_capacity(max_input_frames))
            .collect();
        let channel_outputs: Vec<Vec<f64>> = (0..channels)
            .map(|_| Vec::with_capacity(max_output_per_channel))
            .collect();
        let interleaved_output = Vec::with_capacity(max_output_per_channel * channels);

        Ok(Self {
            soxr_instances,
            channels,
            from_rate,
            to_rate,
            output_scratch: vec![0.0; max_output_per_channel],
            channel_inputs,
            channel_outputs,
            interleaved_output,
        })
    }

    fn process_chunk_to_internal_output(&mut self, input: &[f64]) -> usize {
        // Clear and reuse pre-allocated channel input buffers (Defect 33 fix)
        for ch_buf in &mut self.channel_inputs {
            ch_buf.clear();
        }

        let input_frames = input.len() / self.channels;

        // De-interleave input into pre-allocated buffers. Trailing incomplete
        // frames are intentionally ignored, matching `input.len() / channels`.
        deinterleave_frame_major(input, self.channels, input_frames, &mut self.channel_inputs);

        // Clear and reuse pre-allocated channel output buffers (Defect 33 fix)
        for ch_buf in &mut self.channel_outputs {
            ch_buf.clear();
        }

        // Process each channel
        for (ch, channel_data) in self.channel_inputs.iter().enumerate() {
            // Naive ratio bound for this call. We never resize on the audio thread;
            // instead we cap Soxr's output by slicing the pre-allocated scratch.
            // Capping to the naive bound keeps `max_output_len_for_input` a valid
            // single-call ceiling. The `.min(len)` keeps it panic-safe for offline
            // callers feeding chunks larger than `max_input_frames` — Soxr buffers
            // the excess and recovers it on later process/flush calls.
            let expected_output = (channel_data.len() as f64 * self.to_rate as f64
                / self.from_rate as f64)
                .ceil() as usize
                + 64;
            let cap = expected_output.min(self.output_scratch.len());

            let processed = match self.soxr_instances[ch]
                .process(channel_data, &mut self.output_scratch[..cap])
            {
                Ok(p) => p,
                Err(e) => {
                    log::error!(
                        "Resampler process_chunk failed (ch={}, in_frames={}): {:?}",
                        ch,
                        channel_data.len(),
                        e
                    );
                    self.interleaved_output.clear();
                    return 0;
                }
            };

            self.channel_outputs[ch]
                .extend_from_slice(&self.output_scratch[..processed.output_frames]);
        }

        interleave_channel_outputs_to_vec(
            &self.channel_outputs,
            self.channels,
            &mut self.interleaved_output,
        )
    }

    /// Process a chunk of interleaved audio and borrow the resampler-owned output.
    ///
    /// Resampling processes only complete input frames; trailing samples where
    /// `input.len() % channels != 0` are ignored to preserve existing behavior.
    /// The equal-rate bypass returns the original input slice unchanged.
    /// The borrowed slice remains valid until the next mutable resampler call.
    pub fn process_chunk_borrowed<'a>(&'a mut self, input: &'a [f64]) -> ResampleOutput<'a> {
        if self.from_rate == self.to_rate {
            return ResampleOutput {
                samples: input,
                frames: input.len() / self.channels,
            };
        }

        let input_frames = input.len() / self.channels;
        if input_frames == 0 {
            self.interleaved_output.clear();
            return ResampleOutput {
                samples: &self.interleaved_output,
                frames: 0,
            };
        }

        let frames = self.process_chunk_to_internal_output(input);
        ResampleOutput {
            samples: &self.interleaved_output,
            frames,
        }
    }

    /// Process a chunk and append the result directly to a caller-owned buffer.
    pub fn process_chunk_append(&mut self, input: &[f64], output: &mut Vec<f64>) -> usize {
        let result = self.process_chunk_borrowed(input);
        output.extend_from_slice(result.samples);
        result.frames
    }

    /// Process a chunk into a pre-allocated output buffer (zero-allocation version)
    ///
    /// Returns the number of frames written to output.
    /// Output buffer must be large enough: output.len() >= input.len() * to_rate / from_rate + 64
    pub fn process_chunk_into(&mut self, input: &[f64], output: &mut [f64]) -> usize {
        if self.from_rate == self.to_rate {
            let copy_len = input.len().min(output.len());
            output[..copy_len].copy_from_slice(&input[..copy_len]);
            return copy_len / self.channels;
        }

        let input_frames = input.len() / self.channels;
        if input_frames == 0 {
            return 0;
        }

        // Clear and reuse pre-allocated buffers
        for ch_buf in &mut self.channel_inputs {
            ch_buf.clear();
        }

        // De-interleave complete frames only, preserving truncation semantics
        // for `input.len() % channels != 0`.
        deinterleave_frame_major(input, self.channels, input_frames, &mut self.channel_inputs);

        // Clear output buffers
        for ch_buf in &mut self.channel_outputs {
            ch_buf.clear();
        }

        // Process each channel
        for (ch, channel_data) in self.channel_inputs.iter().enumerate() {
            // See process_chunk_to_internal_output: cap Soxr output via slicing
            // instead of resizing on the audio thread.
            let expected_output = (channel_data.len() as f64 * self.to_rate as f64
                / self.from_rate as f64)
                .ceil() as usize
                + 64;
            let cap = expected_output.min(self.output_scratch.len());

            let processed = match self.soxr_instances[ch]
                .process(channel_data, &mut self.output_scratch[..cap])
            {
                Ok(p) => p,
                Err(e) => {
                    log::error!(
                        "Resampler process_chunk_into failed (ch={}, in_frames={}): {:?}",
                        ch,
                        channel_data.len(),
                        e
                    );
                    return 0;
                }
            };

            self.channel_outputs[ch]
                .extend_from_slice(&self.output_scratch[..processed.output_frames]);
        }

        interleave_channel_outputs_to_slice(&self.channel_outputs, self.channels, output)
    }

    pub fn reset(&mut self) {
        for ch_buf in &mut self.channel_inputs {
            ch_buf.clear();
        }
        for ch_buf in &mut self.channel_outputs {
            ch_buf.clear();
        }
        self.interleaved_output.clear();
    }

    /// Flush remaining samples and borrow the resampler-owned interleaved output.
    pub fn flush_borrowed(&mut self) -> ResampleOutput<'_> {
        for channel_output in &mut self.channel_outputs {
            channel_output.clear();
        }

        for ch in 0..self.channels {
            // Keep flushing until no more output
            loop {
                match self.soxr_instances[ch].process(&[], &mut self.output_scratch) {
                    Ok(processed) if processed.output_frames > 0 => {
                        self.channel_outputs[ch]
                            .extend_from_slice(&self.output_scratch[..processed.output_frames]);
                    }
                    _ => break,
                }
            }
        }

        let frames = interleave_channel_outputs_to_vec_with_max_frames(
            &self.channel_outputs,
            self.channels,
            &mut self.interleaved_output,
        );
        ResampleOutput {
            samples: &self.interleaved_output,
            frames,
        }
    }

    /// Flush any remaining samples directly into a caller-owned output buffer.
    pub fn flush_into(&mut self, output: &mut Vec<f64>) -> usize {
        let result = self.flush_borrowed();
        output.extend_from_slice(result.samples);
        result.frames
    }

    /// Flush any remaining samples in the resampler's internal buffers
    pub fn flush(&mut self) -> Vec<f64> {
        self.flush_borrowed().samples.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deinterleave_frame_major_preserves_order_and_truncates_partial_frame() {
        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0, 99.0];
        let mut channels = vec![Vec::new(), Vec::new()];

        deinterleave_frame_major(&input, 2, input.len() / 2, &mut channels);

        assert_eq!(channels[0], vec![1.0, 3.0, 5.0]);
        assert_eq!(channels[1], vec![2.0, 4.0, 99.0]);

        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let mut channels = vec![Vec::new(), Vec::new()];

        deinterleave_frame_major(&input, 2, input.len() / 2, &mut channels);

        assert_eq!(channels[0], vec![1.0, 3.0]);
        assert_eq!(channels[1], vec![2.0, 4.0]);
    }

    #[test]
    fn interleave_channel_outputs_fast_path_preserves_multichannel_order() {
        let channel_outputs = vec![
            vec![1.0, 4.0, 7.0],
            vec![2.0, 5.0, 8.0],
            vec![3.0, 6.0, 9.0],
        ];
        let mut output = Vec::new();

        let frames = interleave_channel_outputs_to_vec(&channel_outputs, 3, &mut output);

        assert_eq!(frames, 3);
        assert_eq!(output, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    }

    #[test]
    fn interleave_channel_outputs_pads_short_channels() {
        let channel_outputs = vec![vec![1.0, 3.0, 5.0], vec![2.0]];
        let mut output = Vec::new();

        let frames = interleave_channel_outputs_to_vec(&channel_outputs, 2, &mut output);

        assert_eq!(frames, 3);
        assert_eq!(output, vec![1.0, 2.0, 3.0, 0.0, 5.0, 0.0]);
    }

    #[test]
    fn interleave_channel_outputs_with_max_frames_pads_short_channels() {
        let channel_outputs = vec![vec![1.0], vec![2.0, 4.0, 6.0]];
        let mut output = Vec::new();

        let frames =
            interleave_channel_outputs_to_vec_with_max_frames(&channel_outputs, 2, &mut output);

        assert_eq!(frames, 3);
        assert_eq!(output, vec![1.0, 2.0, 0.0, 4.0, 0.0, 6.0]);
    }

    #[test]
    fn interleave_channel_outputs_to_slice_preserves_tail_when_output_is_longer() {
        let channel_outputs = vec![vec![1.0, 3.0], vec![2.0, 4.0]];
        let mut output = vec![42.0; 6];

        let frames = interleave_channel_outputs_to_slice(&channel_outputs, 2, &mut output);

        assert_eq!(frames, 2);
        assert_eq!(output, vec![1.0, 2.0, 3.0, 4.0, 42.0, 42.0]);
    }

    #[test]
    fn process_chunk_borrowed_equal_rate_reports_complete_frames_and_full_input() {
        let mut resampler = StreamingResampler::new(2, 48_000, 48_000).unwrap();
        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0];

        let result = resampler.process_chunk_borrowed(&input);

        assert_eq!(result.frames, 2);
        assert_eq!(result.samples, input.as_slice());
    }

    #[test]
    fn input_frames_for_output_frames_tracks_rate_ratio_with_margin() {
        let upsampler = StreamingResampler::new(2, 44_100, 384_000).unwrap();
        let expected_up = ((512.0_f64 * 44_100.0 / 384_000.0).ceil() as usize) + 64;
        assert_eq!(upsampler.input_frames_for_output_frames(512), expected_up);

        let downsampler = StreamingResampler::new(2, 96_000, 48_000).unwrap();
        assert_eq!(downsampler.input_frames_for_output_frames(2112), 4288);
        assert_eq!(downsampler.input_frames_for_output_frames(0), 0);
    }

    #[test]
    fn process_chunk_append_equal_rate_preserves_prefix_and_full_input() {
        let mut resampler = StreamingResampler::new(2, 48_000, 48_000).unwrap();
        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let mut output = vec![-1.0, -2.0];

        let frames = resampler.process_chunk_append(&input, &mut output);

        assert_eq!(frames, 2);
        assert_eq!(output, vec![-1.0, -2.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn process_chunk_append_matches_borrowed_for_resampling() {
        let input = (0..2048)
            .map(|sample| sample as f64 / 2048.0)
            .collect::<Vec<_>>();
        let mut borrowed_resampler = StreamingResampler::new(2, 44_100, 48_000).unwrap();
        let mut append_resampler = StreamingResampler::new(2, 44_100, 48_000).unwrap();
        let expected = borrowed_resampler
            .process_chunk_borrowed(&input)
            .samples
            .to_vec();
        let mut actual = vec![99.0];

        let frames = append_resampler.process_chunk_append(&input, &mut actual);

        assert_eq!(frames * 2, expected.len());
        assert_eq!(&actual[..1], &[99.0]);
        assert_eq!(&actual[1..], expected.as_slice());
    }

    #[test]
    fn process_chunk_into_reuses_internal_capacity_after_warmup() {
        let input = (0..4096)
            .map(|sample| sample as f64 / 4096.0)
            .collect::<Vec<_>>();
        let mut resampler = StreamingResampler::new(2, 44_100, 48_000).unwrap();
        let mut output = vec![0.0; resampler.max_output_len_for_input(input.len())];

        let _ = resampler.process_chunk_into(&input, &mut output);
        let warmed_input_caps = resampler
            .channel_inputs
            .iter()
            .map(Vec::capacity)
            .collect::<Vec<_>>();
        let warmed_output_caps = resampler
            .channel_outputs
            .iter()
            .map(Vec::capacity)
            .collect::<Vec<_>>();
        let warmed_interleaved_cap = resampler.interleaved_output.capacity();
        let warmed_scratch_len = resampler.output_scratch.len();

        let _ = resampler.process_chunk_into(&input, &mut output);

        assert_eq!(
            resampler
                .channel_inputs
                .iter()
                .map(Vec::capacity)
                .collect::<Vec<_>>(),
            warmed_input_caps
        );
        assert_eq!(
            resampler
                .channel_outputs
                .iter()
                .map(Vec::capacity)
                .collect::<Vec<_>>(),
            warmed_output_caps
        );
        assert_eq!(
            resampler.interleaved_output.capacity(),
            warmed_interleaved_cap
        );
        assert_eq!(resampler.output_scratch.len(), warmed_scratch_len);
    }

    #[test]
    fn flush_into_matches_flush_and_preserves_prefix() {
        let input = (0..2048)
            .map(|sample| sample as f64 / 2048.0)
            .collect::<Vec<_>>();
        let mut wrapper_resampler = StreamingResampler::new(2, 44_100, 48_000).unwrap();
        let mut append_resampler = StreamingResampler::new(2, 44_100, 48_000).unwrap();
        let mut scratch = Vec::new();
        let _ = wrapper_resampler.process_chunk_append(&input, &mut scratch);
        scratch.clear();
        let _ = append_resampler.process_chunk_append(&input, &mut scratch);
        let expected = wrapper_resampler.flush();
        let mut actual = vec![99.0];

        let frames = append_resampler.flush_into(&mut actual);

        assert_eq!(frames * 2, expected.len());
        assert_eq!(&actual[..1], &[99.0]);
        assert_eq!(&actual[1..], expected.as_slice());
    }

    #[test]
    fn flush_into_reuses_warmed_output_capacity() {
        let input = (0..4096)
            .map(|sample| sample as f64 / 4096.0)
            .collect::<Vec<_>>();
        let mut resampler = StreamingResampler::new(2, 44_100, 48_000).unwrap();
        let mut scratch = Vec::new();
        let _ = resampler.process_chunk_append(&input, &mut scratch);
        let mut output = Vec::with_capacity(resampler.max_output_len_for_input(input.len()));

        let _ = resampler.flush_into(&mut output);
        let warmed_capacity = output.capacity();
        output.clear();
        scratch.clear();
        let _ = resampler.process_chunk_append(&input, &mut scratch);
        let _ = resampler.flush_into(&mut output);

        assert_eq!(output.capacity(), warmed_capacity);
    }

    #[test]
    fn flush_into_reuses_internal_capacity_after_warmup() {
        let input = (0..4096)
            .map(|sample| sample as f64 / 4096.0)
            .collect::<Vec<_>>();
        let mut resampler = StreamingResampler::new(2, 44_100, 48_000).unwrap();
        let mut output = Vec::with_capacity(resampler.max_output_len_for_input(input.len()));
        let mut scratch = Vec::new();
        let _ = resampler.process_chunk_append(&input, &mut scratch);
        let _ = resampler.flush_into(&mut output);
        let warmed_channel_caps = resampler
            .channel_outputs
            .iter()
            .map(Vec::capacity)
            .collect::<Vec<_>>();
        let warmed_interleaved_cap = resampler.interleaved_output.capacity();
        let warmed_scratch_len = resampler.output_scratch.len();

        output.clear();
        scratch.clear();
        let _ = resampler.process_chunk_append(&input, &mut scratch);
        let _ = resampler.flush_into(&mut output);

        assert_eq!(
            resampler
                .channel_outputs
                .iter()
                .map(Vec::capacity)
                .collect::<Vec<_>>(),
            warmed_channel_caps
        );
        assert_eq!(
            resampler.interleaved_output.capacity(),
            warmed_interleaved_cap
        );
        assert_eq!(resampler.output_scratch.len(), warmed_scratch_len);
    }
}
