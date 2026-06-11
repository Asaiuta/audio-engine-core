//! Streaming Audio Pipeline
//!
//! Asynchronous audio processing pipeline for streaming decode and resample.
//! This eliminates the memory spike issue with 192kHz upsampling.

use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
// Channel imports unused in current implementation

use crate::config::ResampleQuality;
use crate::decoder::{DecoderError, StreamingDecoder};
use crate::processor::StreamingResampler;

/// Error returned when constructing an [`AudioPipeline`].
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// The underlying decoder could not open the source.
    #[error("failed to open decoder: {0}")]
    Decoder(#[from] DecoderError),
}

/// Ring buffer size in frames (per channel)
/// ~4MB for stereo f64 at 192kHz ≈ 0.5 seconds buffer
const RING_BUFFER_FRAMES: usize = 131072;

/// Status of the audio pipeline
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PipelineStatus {
    /// Pipeline is idle, waiting for data
    Idle,
    /// Pipeline is actively buffering/processing
    Buffering,
    /// Pipeline has finished processing all data
    Finished,
    /// Pipeline encountered an error
    Error,
}

/// Streaming audio pipeline that decodes and resamples in background
pub struct AudioPipeline {
    // Ring buffer for processed audio data
    ring_buffer: Arc<RwLock<RingBuffer>>,

    // Control flags
    is_running: Arc<AtomicBool>,
    is_finished: Arc<AtomicBool>,

    // Progress tracking
    buffered_frames: Arc<AtomicU64>,
    total_frames: Arc<AtomicU64>,
    current_read_pos: Arc<AtomicU64>,

    // Worker thread handle
    worker_handle: Option<JoinHandle<()>>,

    // Audio format info
    channels: usize,
    sample_rate: u32,
    original_sample_rate: u32,
}

/// Simple ring buffer for audio data
/// Uses monotonic counters (frames_written, frames_consumed) for clean overflow handling.
pub struct RingBuffer {
    data: Vec<f64>,
    capacity_frames: usize,
    channels: usize,
    /// Total frames written (monotonically increasing)
    frames_written: u64,
    /// Total frames consumed by readers (monotonically increasing)
    frames_consumed: u64,
    /// Number of overflow events
    overflow_count: u64,
}

impl RingBuffer {
    pub fn new(capacity_frames: usize, channels: usize) -> Self {
        Self {
            data: vec![0.0; capacity_frames * channels],
            capacity_frames,
            channels,
            frames_written: 0,
            frames_consumed: 0,
            overflow_count: 0,
        }
    }

    /// Write frames to the buffer, returns number of frames written
    /// If buffer would overflow, drops the oldest data (ring buffer behavior)
    /// Returns (frames_written, overflow_new_consumed) — overflow_new_consumed is
    /// the updated frames_consumed value that external read positions must respect.
    pub fn write(&mut self, samples: &[f64]) -> (usize, Option<u64>) {
        let frames_to_write = samples.len() / self.channels;
        let samples_to_write = frames_to_write * self.channels;

        if frames_to_write == 0 {
            return (0, None);
        }

        // Check for potential overflow
        let frames_in_buffer = self.frames_written.saturating_sub(self.frames_consumed);
        let available_space = self
            .capacity_frames
            .saturating_sub(frames_in_buffer as usize);

        let overflow_consumed = if frames_to_write > available_space {
            // Overflow detected - advance consumer position to make room
            // This effectively drops the oldest frames
            let overflow_frames = frames_to_write - available_space;
            self.frames_consumed = self.frames_consumed.saturating_add(overflow_frames as u64);
            self.overflow_count = self.overflow_count.saturating_add(1);
            log::warn!(
                "RingBuffer overflow: dropping {} frames (total overflows: {})",
                overflow_frames,
                self.overflow_count
            );
            // FIX for Defect 5: Return new consumed position so external read_pos can be updated
            Some(self.frames_consumed)
        } else {
            None
        };

        // Write samples using at most two contiguous copies split at the wrap boundary.
        let frames_to_copy = frames_to_write.min(self.capacity_frames);
        let source_frame_offset = frames_to_write - frames_to_copy;
        let source_sample_offset = source_frame_offset * self.channels;
        let write_frame =
            ((self.frames_written as usize) + source_frame_offset) % self.capacity_frames;
        self.copy_frames_from_slice(
            write_frame,
            &samples[source_sample_offset..samples_to_write],
            frames_to_copy,
        );

        self.frames_written += frames_to_write as u64;
        (frames_to_write, overflow_consumed)
    }

    /// Read frames from the buffer at a given position
    pub fn read(&self, start_frame: u64, output: &mut [f64]) -> usize {
        let frames_to_read = output.len() / self.channels;
        let available = self.frames_written.saturating_sub(start_frame) as usize;
        let actual_frames = frames_to_read.min(available);

        if actual_frames == 0 {
            return 0;
        }

        let read_frame = (start_frame as usize) % self.capacity_frames;
        self.copy_frames_to_slice(
            read_frame,
            &mut output[..actual_frames * self.channels],
            actual_frames,
        );

        actual_frames
    }

    fn copy_frames_from_slice(&mut self, start_frame: usize, source: &[f64], frames: usize) {
        let first_frames = frames.min(self.capacity_frames - start_frame);
        let first_samples = first_frames * self.channels;
        let start_sample = start_frame * self.channels;

        self.data[start_sample..start_sample + first_samples]
            .copy_from_slice(&source[..first_samples]);

        let remaining_frames = frames - first_frames;
        if remaining_frames > 0 {
            let remaining_samples = remaining_frames * self.channels;
            self.data[..remaining_samples]
                .copy_from_slice(&source[first_samples..first_samples + remaining_samples]);
        }
    }

    fn copy_frames_to_slice(&self, start_frame: usize, output: &mut [f64], frames: usize) {
        let first_frames = frames.min(self.capacity_frames - start_frame);
        let first_samples = first_frames * self.channels;
        let start_sample = start_frame * self.channels;

        output[..first_samples]
            .copy_from_slice(&self.data[start_sample..start_sample + first_samples]);

        let remaining_frames = frames - first_frames;
        if remaining_frames > 0 {
            let remaining_samples = remaining_frames * self.channels;
            output[first_samples..first_samples + remaining_samples]
                .copy_from_slice(&self.data[..remaining_samples]);
        }
    }

    /// Update consumed position (call after reading)
    pub fn advance_read_pos(&mut self, frames: u64) {
        self.frames_consumed = self.frames_consumed.saturating_add(frames);
    }

    /// Get number of frames available for reading from a given position
    pub fn available_frames(&self, read_pos: u64) -> u64 {
        self.frames_written.saturating_sub(read_pos)
    }

    /// Get total frames written
    pub fn total_written(&self) -> u64 {
        self.frames_written
    }

    /// Get overflow count
    pub fn overflow_count(&self) -> u64 {
        self.overflow_count
    }
}

impl AudioPipeline {
    /// Create a new pipeline from a file path.
    ///
    /// This opens the decoder once to read the source format (sample rate,
    /// channel count, and frame count) so callers can query the pipeline before
    /// starting the background worker. Call [`AudioPipeline::start`] to begin
    /// decoding and resampling.
    pub fn new(
        path: &str,
        target_sample_rate: Option<u32>,
        _resample_quality: ResampleQuality,
    ) -> Result<Self, PipelineError> {
        let decoder = StreamingDecoder::open(path)?;

        let info = decoder.info.clone();
        let original_sr = info.sample_rate;
        let channels = info.channels;
        let total_source_frames = info.total_frames.unwrap_or(0);

        // Determine target sample rate
        let target_sr = target_sample_rate.unwrap_or(original_sr);

        // Calculate expected total frames after resampling
        let total_frames = if target_sr != original_sr {
            ((total_source_frames as f64) * (target_sr as f64) / (original_sr as f64)).ceil() as u64
        } else {
            total_source_frames
        };

        log::info!(
            "Creating audio pipeline: {}→{} Hz, {} ch, ~{} frames",
            original_sr,
            target_sr,
            channels,
            total_frames
        );

        let ring_buffer = Arc::new(RwLock::new(RingBuffer::new(RING_BUFFER_FRAMES, channels)));
        let is_running = Arc::new(AtomicBool::new(false));
        let is_finished = Arc::new(AtomicBool::new(false));
        let buffered_frames = Arc::new(AtomicU64::new(0));
        let total_frames_arc = Arc::new(AtomicU64::new(total_frames));
        let current_read_pos = Arc::new(AtomicU64::new(0));

        let pipeline = Self {
            ring_buffer: Arc::clone(&ring_buffer),
            is_running: Arc::clone(&is_running),
            is_finished: Arc::clone(&is_finished),
            buffered_frames: Arc::clone(&buffered_frames),
            total_frames: Arc::clone(&total_frames_arc),
            current_read_pos: Arc::clone(&current_read_pos),
            worker_handle: None,
            channels,
            sample_rate: target_sr,
            original_sample_rate: original_sr,
        };

        Ok(pipeline)
    }

    /// Start the background processing thread
    pub fn start(
        &mut self,
        path: String,
        target_sample_rate: Option<u32>,
        quality: ResampleQuality,
    ) {
        if self.is_running.load(Ordering::Relaxed) {
            return;
        }

        self.is_running.store(true, Ordering::Relaxed);
        self.is_finished.store(false, Ordering::Relaxed);

        let ring_buffer = Arc::clone(&self.ring_buffer);
        let is_running = Arc::clone(&self.is_running);
        let is_finished = Arc::clone(&self.is_finished);
        let buffered_frames = Arc::clone(&self.buffered_frames);
        let total_frames = Arc::clone(&self.total_frames);
        let current_read_pos = Arc::clone(&self.current_read_pos);
        let channels = self.channels;
        let original_sr = self.original_sample_rate;
        let target_sr = target_sample_rate.unwrap_or(original_sr);

        let handle = thread::spawn(move || {
            Self::worker_loop(
                path,
                channels,
                original_sr,
                target_sr,
                quality,
                ring_buffer,
                is_running,
                is_finished,
                buffered_frames,
                total_frames,
                current_read_pos,
            );
        });

        self.worker_handle = Some(handle);
    }

    /// Background worker that decodes and resamples
    #[allow(clippy::too_many_arguments)]
    fn worker_loop(
        path: String,
        channels: usize,
        original_sr: u32,
        target_sr: u32,
        quality: ResampleQuality,
        ring_buffer: Arc<RwLock<RingBuffer>>,
        is_running: Arc<AtomicBool>,
        is_finished: Arc<AtomicBool>,
        buffered_frames: Arc<AtomicU64>,
        total_frames: Arc<AtomicU64>,
        current_read_pos: Arc<AtomicU64>,
    ) {
        log::info!("Pipeline worker started for: {}", path);

        // Open decoder
        let mut decoder = match StreamingDecoder::open(&path) {
            Ok(d) => d,
            Err(e) => {
                log::error!("Failed to open decoder in worker: {}", e);
                is_finished.store(true, Ordering::Relaxed);
                return;
            }
        };

        // Create resampler if needed
        let mut resampler = if target_sr != original_sr {
            match StreamingResampler::with_quality(
                channels,
                original_sr,
                target_sr,
                crate::config::PhaseResponse::default(),
                quality,
            ) {
                Ok(rs) => Some(rs),
                Err(e) => {
                    log::error!("Failed to create pipeline resampler: {}", e);
                    return;
                }
            }
        } else {
            None
        };

        let mut total_output_frames: u64 = 0;

        // Process loop
        while is_running.load(Ordering::Relaxed) {
            // Decode next chunk
            let decoded = match decoder.decode_next() {
                Ok(Some(samples)) => samples,
                Ok(None) => {
                    // EOF - flush resampler if present
                    if let Some(ref mut rs) = resampler {
                        let flushed = rs.flush_borrowed();
                        if !flushed.samples.is_empty() {
                            let (_, overflow) = ring_buffer.write().write(flushed.samples);
                            // FIX for Defect 5: Sync external read position on overflow
                            if let Some(min_pos) = overflow {
                                current_read_pos.fetch_max(min_pos, Ordering::Relaxed);
                            }
                            total_output_frames += flushed.frames as u64;
                            buffered_frames.store(total_output_frames, Ordering::Relaxed);
                        }
                    }
                    break;
                }
                Err(e) => {
                    log::error!("Decode error in pipeline: {}", e);
                    break;
                }
            };

            // Resample if needed. Borrow resampler-owned output directly so the
            // pipeline worker does not allocate a temporary Vec per chunk.
            let (output, frames) = if let Some(ref mut rs) = resampler {
                let resampled = rs.process_chunk_borrowed(&decoded);
                (resampled.samples, resampled.frames)
            } else {
                let frames = decoded.len() / channels;
                (decoded.as_slice(), frames)
            };

            if !output.is_empty() {
                let (_, overflow) = ring_buffer.write().write(output);
                // FIX for Defect 5: Sync external read position on overflow
                if let Some(min_pos) = overflow {
                    current_read_pos.fetch_max(min_pos, Ordering::Relaxed);
                }
                total_output_frames += frames as u64;
                buffered_frames.store(total_output_frames, Ordering::Relaxed);
            }
        }

        // Update final total frames (may differ from estimate)
        total_frames.store(total_output_frames, Ordering::Relaxed);
        is_finished.store(true, Ordering::Relaxed);
        is_running.store(false, Ordering::Relaxed);

        log::info!(
            "Pipeline worker finished. Total frames: {}",
            total_output_frames
        );
    }

    /// Stop the pipeline
    pub fn stop(&mut self) {
        self.is_running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }

    /// Read audio data from the pipeline
    /// Returns number of frames actually read
    pub fn read(&self, output: &mut [f64]) -> usize {
        let read_pos = self.current_read_pos.load(Ordering::Relaxed);
        let Some(buffer) = self.ring_buffer.try_read() else {
            return 0;
        };
        let frames_read = buffer.read(read_pos, output);

        if frames_read > 0 {
            self.current_read_pos
                .fetch_add(frames_read as u64, Ordering::Relaxed);
        }

        frames_read
    }

    /// Get current read position in frames
    pub fn read_position(&self) -> u64 {
        self.current_read_pos.load(Ordering::Relaxed)
    }

    /// Set read position (for seeking)
    pub fn set_read_position(&self, frame: u64) {
        self.current_read_pos.store(frame, Ordering::Relaxed);
    }

    /// Get total frames
    pub fn total_frames(&self) -> u64 {
        self.total_frames.load(Ordering::Relaxed)
    }

    /// Get buffered frames
    pub fn buffered_frames(&self) -> u64 {
        self.buffered_frames.load(Ordering::Relaxed)
    }

    /// Check if pipeline has finished processing
    pub fn is_finished(&self) -> bool {
        self.is_finished.load(Ordering::Relaxed)
    }

    /// Check if pipeline is running
    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }

    /// Get buffering ratio (0.0 - 1.0)
    pub fn buffer_ratio(&self) -> f32 {
        let total = self.total_frames.load(Ordering::Relaxed);
        let buffered = self.buffered_frames.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        (buffered as f32 / total as f32).min(1.0)
    }

    /// Get available frames from current read position
    pub fn available_frames(&self) -> u64 {
        let read_pos = self.current_read_pos.load(Ordering::Relaxed);
        self.buffered_frames
            .load(Ordering::Relaxed)
            .saturating_sub(read_pos)
    }

    /// Number of channels in the output stream.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Output (post-resample) sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Source (pre-resample) sample rate in Hz.
    pub fn original_sample_rate(&self) -> u32 {
        self.original_sample_rate
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(frames: usize, channels: usize, start: f64) -> Vec<f64> {
        (0..frames * channels).map(|i| start + i as f64).collect()
    }

    #[test]
    fn ring_buffer_reads_back_exact_capacity() {
        let mut buffer = RingBuffer::new(4, 2);
        let input = samples(4, 2, 1.0);
        let mut output = vec![0.0; input.len()];

        assert_eq!(buffer.write(&input), (4, None));
        assert_eq!(buffer.read(0, &mut output), 4);
        assert_eq!(output, input);
    }

    #[test]
    fn ring_buffer_write_and_read_wrap_preserve_order() {
        let mut buffer = RingBuffer::new(4, 2);
        let first = samples(3, 2, 1.0);
        let second = samples(3, 2, 101.0);

        assert_eq!(buffer.write(&first), (3, None));
        buffer.advance_read_pos(2);
        assert_eq!(buffer.write(&second), (3, None));

        let mut output = vec![0.0; 4 * 2];
        assert_eq!(buffer.read(2, &mut output), 4);

        let mut expected = first[2 * 2..].to_vec();
        expected.extend_from_slice(&second);
        assert_eq!(output, expected);
    }

    #[test]
    fn ring_buffer_overflow_keeps_newest_frames_and_reports_consumed_position() {
        let mut buffer = RingBuffer::new(4, 2);
        let input = samples(6, 2, 1.0);
        let mut output = vec![0.0; 4 * 2];

        assert_eq!(buffer.write(&input), (6, Some(2)));
        assert_eq!(buffer.overflow_count(), 1);
        assert_eq!(buffer.read(2, &mut output), 4);
        assert_eq!(output, input[2 * 2..].to_vec());
    }

    #[test]
    fn ring_buffer_empty_read_leaves_output_untouched() {
        let buffer = RingBuffer::new(4, 2);
        let mut output = vec![42.0; 4];

        assert_eq!(buffer.read(0, &mut output), 0);
        assert_eq!(output, vec![42.0; 4]);
    }

    #[test]
    fn ring_buffer_partial_read_only_copies_available_frames() {
        let mut buffer = RingBuffer::new(8, 2);
        let input = samples(2, 2, 1.0);
        let mut output = vec![42.0; 4 * 2];

        assert_eq!(buffer.write(&input), (2, None));
        assert_eq!(buffer.read(0, &mut output), 2);
        assert_eq!(&output[..4], &input[..]);
        assert_eq!(&output[4..], &[42.0; 4]);
    }

    #[test]
    fn ring_buffer_wrap_preserves_multichannel_interleaving() {
        let channels = 6;
        let mut buffer = RingBuffer::new(4, channels);
        let first = samples(3, channels, 1.0);
        let second = samples(3, channels, 101.0);

        assert_eq!(buffer.write(&first), (3, None));
        buffer.advance_read_pos(2);
        assert_eq!(buffer.write(&second), (3, None));

        let mut output = vec![0.0; 4 * channels];
        assert_eq!(buffer.read(2, &mut output), 4);

        let mut expected = first[2 * channels..].to_vec();
        expected.extend_from_slice(&second);
        assert_eq!(output, expected);
    }

    #[test]
    fn audio_pipeline_read_returns_zero_when_ring_buffer_locked() {
        let ring_buffer = Arc::new(RwLock::new(RingBuffer::new(4, 2)));
        let pipeline = AudioPipeline {
            ring_buffer: Arc::clone(&ring_buffer),
            is_running: Arc::new(AtomicBool::new(false)),
            is_finished: Arc::new(AtomicBool::new(false)),
            buffered_frames: Arc::new(AtomicU64::new(0)),
            total_frames: Arc::new(AtomicU64::new(0)),
            current_read_pos: Arc::new(AtomicU64::new(0)),
            worker_handle: None,
            channels: 2,
            sample_rate: 48_000,
            original_sample_rate: 48_000,
        };

        let _write_guard = ring_buffer.write();
        let mut output = vec![42.0; 4];

        assert_eq!(pipeline.read(&mut output), 0);
        assert_eq!(pipeline.read_position(), 0);
        assert_eq!(output, vec![42.0; 4]);
    }

    #[test]
    fn audio_pipeline_read_advances_by_frames_actually_read() {
        let ring_buffer = Arc::new(RwLock::new(RingBuffer::new(8, 2)));
        ring_buffer.write().write(&samples(3, 2, 1.0));
        let pipeline = AudioPipeline {
            ring_buffer,
            is_running: Arc::new(AtomicBool::new(false)),
            is_finished: Arc::new(AtomicBool::new(false)),
            buffered_frames: Arc::new(AtomicU64::new(3)),
            total_frames: Arc::new(AtomicU64::new(3)),
            current_read_pos: Arc::new(AtomicU64::new(0)),
            worker_handle: None,
            channels: 2,
            sample_rate: 48_000,
            original_sample_rate: 48_000,
        };

        let mut first = vec![0.0; 2 * 2];
        assert_eq!(pipeline.read(&mut first), 2);
        assert_eq!(pipeline.read_position(), 2);

        let mut second = vec![0.0; 2 * 2];
        assert_eq!(pipeline.read(&mut second), 1);
        assert_eq!(pipeline.read_position(), 3);

        let mut empty = vec![42.0; 2 * 2];
        assert_eq!(pipeline.read(&mut empty), 0);
        assert_eq!(pipeline.read_position(), 3);
        assert_eq!(empty, vec![42.0; 4]);
    }
}
