//! True-peak limiter with 10ms look-ahead and exponential release.
//!
//! Ring-buffered, allocation-free in the audio callback path.

use crate::processor::dsp::{db_to_linear, linear_to_db};

#[derive(Debug, Clone)]
struct MonotonicMaxQueue {
    indices: Box<[u64]>,
    peaks: Box<[f64]>,
    head: usize,
    tail: usize,
    len: usize,
}

impl MonotonicMaxQueue {
    fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            indices: vec![0; capacity].into_boxed_slice(),
            peaks: vec![0.0; capacity].into_boxed_slice(),
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    #[inline]
    fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.len = 0;
    }

    #[inline]
    fn current_peak(&self) -> f64 {
        if self.len == 0 {
            0.0
        } else {
            self.peaks[self.head]
        }
    }

    #[inline]
    fn push(&mut self, frame_index: u64, peak: f64) {
        while self.len > 0 && self.back_peak() <= peak {
            self.pop_back();
        }

        if self.len == self.indices.len() {
            self.pop_front();
        }

        self.indices[self.tail] = frame_index;
        self.peaks[self.tail] = peak;
        self.tail = (self.tail + 1) % self.indices.len();
        self.len += 1;
    }

    #[inline]
    fn expire_through(&mut self, max_expired_index: u64) {
        while self.len > 0 && self.indices[self.head] <= max_expired_index {
            self.pop_front();
        }
    }

    #[inline]
    fn back_peak(&self) -> f64 {
        let index = (self.tail + self.indices.len() - 1) % self.indices.len();
        self.peaks[index]
    }

    #[inline]
    fn pop_front(&mut self) {
        self.head = (self.head + 1) % self.indices.len();
        self.len -= 1;
    }

    #[inline]
    fn pop_back(&mut self) {
        self.tail = (self.tail + self.indices.len() - 1) % self.indices.len();
        self.len -= 1;
    }
}

/// True Peak Limiter with look-ahead and proper release behavior.
///
/// # Design
///
/// - 10ms look-ahead buffer for peak detection
/// - -1.0 dBTP threshold (EBU R128 recommendation)
/// - Proper release coefficient using exponential smoothing
/// - Fixed ring buffer avoids heap allocation in audio callback
pub struct PeakLimiter {
    /// Linear threshold (e.g., 0.8913 for -1 dB)
    threshold: f64,
    /// Look-ahead buffer size in frames
    lookahead_frames: usize,
    /// Fixed-size ring buffer (frames * channels)
    delay_buffer: Box<[f64]>,
    /// Sliding maximum of per-frame peaks in the delay buffer
    peak_queue: MonotonicMaxQueue,
    /// Monotonic input frame index used by `peak_queue`
    global_frame: u64,
    /// Current write position in the ring buffer
    write_pos: usize,
    /// Current gain reduction (linear, < 1.0 when limiting)
    gain_reduction: f64,
    /// Release coefficient per sample (< 1.0, for multiplication)
    release_coeff: f64,
    /// Number of channels
    channels: usize,
    /// Sample rate (needed for in-place release_ms updates)
    sample_rate: f64,
}

impl PeakLimiter {
    /// Create a new True Peak Limiter
    ///
    /// # Arguments
    /// * `channels` - Number of audio channels
    /// * `sample_rate` - Sample rate in Hz
    /// * `threshold_db` - Threshold in dBTP (default: -1.0)
    /// * `lookahead_ms` - Look-ahead time in ms (default: 10.0)
    /// * `release_ms` - Release time in ms (default: 100.0)
    pub fn new(
        channels: usize,
        sample_rate: u32,
        threshold_db: f64,
        lookahead_ms: f64,
        release_ms: f64,
    ) -> Self {
        let threshold = db_to_linear(threshold_db);
        let lookahead_frames = ((lookahead_ms / 1000.0) * sample_rate as f64).ceil() as usize;
        let lookahead_frames = lookahead_frames.max(1);

        // Release coefficient: exp(-1 / tau) where tau = release_samples
        // This gives us a coefficient < 1 for multiplication
        let release_samples = (release_ms / 1000.0) * sample_rate as f64;
        let release_coeff = (-1.0 / release_samples).exp();

        // Pre-allocate fixed-size buffer
        let buffer_size = lookahead_frames * channels;
        let delay_buffer = vec![0.0; buffer_size].into_boxed_slice();

        Self {
            threshold,
            lookahead_frames,
            delay_buffer,
            peak_queue: MonotonicMaxQueue::new(lookahead_frames),
            global_frame: 0,
            write_pos: 0,
            gain_reduction: 1.0,
            release_coeff,
            channels,
            sample_rate: sample_rate as f64,
        }
    }

    /// Process interleaved samples in-place
    ///
    /// This function is real-time safe:
    /// - No heap allocations
    /// - No system calls
    /// - O(n) complexity where n = number of samples
    pub fn process(&mut self, samples: &mut [f64]) {
        let total_samples = samples.len();
        let frames = total_samples / self.channels;
        if frames == 0 {
            return;
        }

        for frame in 0..frames {
            // Step 1: Read peak across all channels in the look-ahead window.
            // Query before writing the current input frame to preserve the
            // existing delay-buffer semantics exactly.
            let peak = self.peak_queue.current_peak();

            // Step 2: Calculate required gain reduction (instant attack)
            let target_gain = if peak > self.threshold {
                self.threshold / peak
            } else {
                1.0
            };

            // Step 3: Apply release smoothing (gain_reduction can only decrease or recover)
            // Instant attack: take minimum of current and target
            // Smooth release: recover towards 1.0 using multiplication
            if target_gain < self.gain_reduction {
                // Attack: instant
                self.gain_reduction = target_gain;
            } else {
                // Release: smooth recovery
                self.gain_reduction =
                    self.gain_reduction + (1.0 - self.gain_reduction) * (1.0 - self.release_coeff);
                // Ensure we don't exceed target
                self.gain_reduction = self.gain_reduction.min(target_gain);
            }

            // Step 4: Read from delay buffer, write new samples, apply gain
            let mut frame_peak = 0.0_f64;
            for ch in 0..self.channels {
                let input_idx = frame * self.channels + ch;
                let buffer_idx = self.write_pos * self.channels + ch;
                let input = samples[input_idx];
                frame_peak = frame_peak.max(input.abs());

                // Get delayed sample
                let delayed = self.delay_buffer[buffer_idx];

                // Store new sample in buffer
                self.delay_buffer[buffer_idx] = input;

                // Output delayed sample with gain reduction
                samples[input_idx] = delayed * self.gain_reduction;
            }

            self.push_frame_peak(frame_peak);

            // Advance write position
            self.write_pos = (self.write_pos + 1) % self.lookahead_frames;
        }
    }

    #[inline]
    fn push_frame_peak(&mut self, frame_peak: f64) {
        if self.global_frame >= self.lookahead_frames as u64 {
            self.peak_queue
                .expire_through(self.global_frame - self.lookahead_frames as u64);
        }
        self.peak_queue.push(self.global_frame, frame_peak);
        self.global_frame = self.global_frame.wrapping_add(1);
    }

    /// Set threshold in dB
    pub fn set_threshold_db(&mut self, threshold_db: f64) {
        self.threshold = db_to_linear(threshold_db);
    }

    /// Update threshold in-place without reallocating lookahead buffer.
    pub fn set_threshold(&mut self, threshold_db: f64) {
        self.threshold = db_to_linear(threshold_db);
    }

    /// Update release time in-place without reallocating lookahead buffer.
    pub fn set_release_ms(&mut self, release_ms: f64) {
        let release_samples = (release_ms / 1000.0) * self.sample_rate;
        self.release_coeff = (-1.0 / release_samples.max(1.0)).exp();
    }

    /// Check if limiter is conceptually enabled (always true for PeakLimiter)
    pub fn is_enabled(&self) -> bool {
        true
    }

    /// Get current gain reduction in dB (for metering)
    pub fn gain_reduction_db(&self) -> f64 {
        linear_to_db(self.gain_reduction)
    }

    /// Reset limiter state
    pub fn reset(&mut self) {
        for sample in self.delay_buffer.iter_mut() {
            *sample = 0.0;
        }
        self.peak_queue.clear();
        self.global_frame = 0;
        self.write_pos = 0;
        self.gain_reduction = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LegacyPeakLimiter {
        threshold: f64,
        lookahead_frames: usize,
        delay_buffer: Box<[f64]>,
        write_pos: usize,
        gain_reduction: f64,
        release_coeff: f64,
        channels: usize,
    }

    impl LegacyPeakLimiter {
        fn new(
            channels: usize,
            sample_rate: u32,
            threshold_db: f64,
            lookahead_ms: f64,
            release_ms: f64,
        ) -> Self {
            let threshold = db_to_linear(threshold_db);
            let lookahead_frames = ((lookahead_ms / 1000.0) * sample_rate as f64).ceil() as usize;
            let lookahead_frames = lookahead_frames.max(1);
            let release_samples = (release_ms / 1000.0) * sample_rate as f64;
            let release_coeff = (-1.0 / release_samples).exp();

            Self {
                threshold,
                lookahead_frames,
                delay_buffer: vec![0.0; lookahead_frames * channels].into_boxed_slice(),
                write_pos: 0,
                gain_reduction: 1.0,
                release_coeff,
                channels,
            }
        }

        fn process(&mut self, samples: &mut [f64]) {
            let frames = samples.len() / self.channels;
            if frames == 0 {
                return;
            }

            for frame in 0..frames {
                let peak = self.scan_lookahead_peak();
                let target_gain = if peak > self.threshold {
                    self.threshold / peak
                } else {
                    1.0
                };

                if target_gain < self.gain_reduction {
                    self.gain_reduction = target_gain;
                } else {
                    self.gain_reduction = self.gain_reduction
                        + (1.0 - self.gain_reduction) * (1.0 - self.release_coeff);
                    self.gain_reduction = self.gain_reduction.min(target_gain);
                }

                for ch in 0..self.channels {
                    let input_idx = frame * self.channels + ch;
                    let buffer_idx = self.write_pos * self.channels + ch;
                    let delayed = self.delay_buffer[buffer_idx];
                    self.delay_buffer[buffer_idx] = samples[input_idx];
                    samples[input_idx] = delayed * self.gain_reduction;
                }

                self.write_pos = (self.write_pos + 1) % self.lookahead_frames;
            }
        }

        fn scan_lookahead_peak(&self) -> f64 {
            let mut peak = 0.0_f64;
            for frame in 0..self.lookahead_frames {
                let pos = (self.write_pos + frame) % self.lookahead_frames;
                for ch in 0..self.channels {
                    let idx = pos * self.channels + ch;
                    peak = peak.max(self.delay_buffer[idx].abs());
                }
            }
            peak
        }
    }

    fn assert_samples_eq(left: &[f64], right: &[f64]) {
        assert_eq!(left.len(), right.len());
        for (index, (a, b)) in left.iter().zip(right.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "sample {index}: left={a}, right={b}"
            );
        }
    }

    fn deterministic_transient_corpus(frames: usize, channels: usize) -> Vec<f64> {
        let mut samples = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            let base =
                ((frame as f64 * 0.037).sin() * 0.35) + ((frame as f64 * 0.011).cos() * 0.08);
            for ch in 0..channels {
                let mut sample = base * (1.0 - ch as f64 * 0.15);
                if matches!(frame, 32 | 257 | 513 | 1024) {
                    sample = if ch == 0 { 1.8 } else { -1.35 };
                }
                samples.push(sample);
            }
        }
        samples
    }

    #[test]
    fn monotonic_queue_matches_legacy_scan_for_transient_corpus() {
        let mut limiter = PeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0);
        let mut legacy = LegacyPeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = deterministic_transient_corpus(2_000, 2);
        let mut expected = samples.clone();

        limiter.process(&mut samples);
        legacy.process(&mut expected);

        assert_samples_eq(&samples, &expected);
    }

    #[test]
    fn monotonic_queue_preserves_cross_buffer_continuity() {
        let source = deterministic_transient_corpus(6_400, 2);
        let mut one_shot = source.clone();
        let mut chunked = source.clone();

        let mut one_shot_limiter = PeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0);
        let mut chunked_limiter = PeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0);

        one_shot_limiter.process(&mut one_shot);
        for chunk in chunked.chunks_mut(64 * 2) {
            chunked_limiter.process(chunk);
        }

        assert_samples_eq(&chunked, &one_shot);
    }

    #[test]
    fn monotonic_queue_handles_sustained_pre_clipping() {
        let mut limiter = PeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = vec![1.2; 2_000 * 2];

        limiter.process(&mut samples);

        let expected_gain = db_to_linear(-1.0) / 1.2;
        assert!((limiter.gain_reduction - expected_gain).abs() < 1e-12);
        assert!(samples
            .iter()
            .all(|sample| sample.abs() <= db_to_linear(-1.0) + 1e-12));
    }

    #[test]
    fn monotonic_queue_resets_state() {
        let mut limiter = PeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = deterministic_transient_corpus(1_000, 2);

        limiter.process(&mut samples);
        assert!(limiter.peak_queue.current_peak() > 0.0);

        limiter.reset();

        assert_eq!(limiter.peak_queue.current_peak(), 0.0);
        assert_eq!(limiter.global_frame, 0);
        assert_eq!(limiter.write_pos, 0);
        assert_eq!(limiter.gain_reduction, 1.0);
    }

    #[test]
    fn lookahead_one_frame_matches_legacy_scan() {
        let mut limiter = PeakLimiter::new(2, 1_000, -1.0, 1.0, 10.0);
        let mut legacy = LegacyPeakLimiter::new(2, 1_000, -1.0, 1.0, 10.0);
        let mut samples = deterministic_transient_corpus(128, 2);
        let mut expected = samples.clone();

        limiter.process(&mut samples);
        legacy.process(&mut expected);

        assert_samples_eq(&samples, &expected);
    }

    #[test]
    fn non_finite_samples_do_not_poison_queue_peak() {
        let mut limiter = PeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = vec![0.2; 64 * 2];
        samples[4] = f64::NAN;
        samples[9] = f64::INFINITY;

        limiter.process(&mut samples);

        assert!(limiter.peak_queue.current_peak().is_infinite());

        let mut finite_samples = vec![0.25; 600 * 2];
        limiter.process(&mut finite_samples);

        assert!(limiter.peak_queue.current_peak().is_finite());
        assert_eq!(limiter.peak_queue.current_peak(), 0.25);
    }

    #[test]
    fn process_is_steady_state_no_alloc() {
        let mut limiter = PeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = deterministic_transient_corpus(64, 2);

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..1_000 {
                limiter.process(&mut samples);
            }
        });
    }
}
