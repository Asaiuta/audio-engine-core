//! Audio ring-buffer building block.
//!
//! This module previously also exposed `AudioPipeline`, a background
//! decode/resample worker. It was removed: its worker had no backpressure
//! (after the ring filled, every write took the drop-oldest overflow path) and
//! its `read()` never released ring space, so any consumer slower than decode
//! speed was pushed through the track at decode speed. No consumer ever
//! constructed it. [`RingBuffer`] remains as a standalone primitive.

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
            Some(self.frames_consumed)
        } else {
            None
        };

        // Write samples using at most two contiguous copies split at the wrap boundary.
        let frames_to_copy = frames_to_write.min(self.capacity_frames);
        let source_frame_offset = frames_to_write - frames_to_copy;
        let source_sample_offset = source_frame_offset * self.channels;
        let write_frame = ((self.frames_written % self.capacity_frames as u64) as usize
            + source_frame_offset)
            % self.capacity_frames;
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

        let read_frame = (start_frame % self.capacity_frames as u64) as usize;
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
}
