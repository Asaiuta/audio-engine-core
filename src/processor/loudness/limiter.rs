//! Look-ahead peak limiter with selectable detection mode.
//!
//! Two modes share one look-ahead/release control path:
//!
//! - [`LimiterMode::TruePeak`] (default): per-channel 4x polyphase-FIR
//!   intersample-peak detection (libebur128-compatible, reused from the meter).
//!   The limiter keeps rendered output below the configured ceiling on
//!   intersample-peak stress material.
//! - [`LimiterMode::SamplePeak`]: classic sample-peak look-ahead. Cheaper, but
//!   only guarantees the ceiling against raw sample values, not reconstructed
//!   intersample peaks.
//!
//! Both modes are ring-buffered and allocation-free in the audio callback path.
//!
//! # Latency
//!
//! Output is delayed by `lookahead_frames` in sample-peak mode. In true-peak
//! mode the delay is `lookahead_frames + TRUE_PEAK_DELAY` (the extra
//! [`TRUE_PEAK_DELAY`] frames cover the FIR span so every output sample's full
//! intersample-peak contribution is known before it leaves the buffer).

use crate::analysis::{true_peak_fir, TruePeakDetector, TRUE_PEAK_DELAY};
use crate::audio_block::{validated_channel_count, AudioBlockMut};
use crate::dsp::{db_to_linear, linear_to_db};
use crate::processor::traits::{
    validate_processor_channels, validate_sample_rate_hz, ProcessError,
};

/// Peak detection strategy for [`PeakLimiter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LimiterMode {
    /// 4x-oversampled intersample (true) peak detection. Default.
    #[default]
    TruePeak,
    /// Sample-peak detection (legacy behavior; no intersample guarantee).
    SamplePeak,
}

/// Active output delay (frames) for a mode: true-peak pads by the FIR span so
/// an output sample's full intersample-peak contribution is inside its window.
#[inline]
fn delay_frames_for(mode: LimiterMode, lookahead_frames: usize) -> usize {
    match mode {
        LimiterMode::TruePeak => lookahead_frames + TRUE_PEAK_DELAY,
        LimiterMode::SamplePeak => lookahead_frames,
    }
}

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

/// Look-ahead peak limiter with selectable [`LimiterMode`].
///
/// # Design
///
/// - Look-ahead buffer for peak detection (10 ms default)
/// - -1.0 dBTP threshold (EBU R128 recommendation)
/// - Attack ramps linearly across the look-ahead, exponential release. A frame's
///   control peak is known `delay_frames - 1` frames before that frame is
///   output, so the descent is spread over the window instead of stepping in one
///   sample. The ramp deliberately converges a little *before* that deadline --
///   long enough to cover the true-peak reconstruction span -- which is what
///   preserves the closed ceiling guarantee.
/// - Fixed ring buffer avoids heap allocation in the audio callback
/// - True-peak mode adds per-channel 4x oversampled intersample detection
pub struct PeakLimiter {
    /// Linear threshold (e.g., 0.8913 for -1 dB)
    threshold: f64,
    /// Detection strategy.
    mode: LimiterMode,
    /// Genuine look-ahead window in frames. Used to recompute `delay_frames`
    /// when the mode switches at runtime.
    lookahead_frames: usize,
    /// Active output delay in frames: `lookahead_frames` in sample-peak mode, or
    /// `lookahead_frames + TRUE_PEAK_DELAY` in true-peak mode. Selects how much
    /// of the (worst-case-sized) delay buffer and peak queue are in use.
    delay_frames: usize,
    /// Fixed-size ring buffer, always sized for the worst case
    /// (`(lookahead_frames + TRUE_PEAK_DELAY) * channels`) so a runtime mode
    /// switch never reallocates on the audio thread.
    delay_buffer: Box<[f64]>,
    /// Per-channel intersample-peak detectors. Always allocated (even in
    /// sample-peak mode) so switching into true-peak mode is allocation-free.
    true_peak_detectors: Vec<TruePeakDetector>,
    /// Sliding maximum of per-frame peaks in the delay buffer
    peak_queue: MonotonicMaxQueue,
    /// Monotonic input frame index used by `peak_queue`
    global_frame: u64,
    /// Current write position in the ring buffer
    write_pos: usize,
    /// Current gain reduction (linear, < 1.0 when limiting)
    gain_reduction: f64,
    /// Frames the attack ramp is allowed to take. See [`attack_frames_for`].
    attack_frames: usize,
    /// Per-frame linear descent currently armed, or `0.0` when not attacking.
    /// Monotonically non-decreasing for the duration of a descent: a re-arm may
    /// only steepen it, never slow a pending one down, or a later louder frame
    /// could push the gain past an earlier frame's deadline.
    attack_step: f64,
    /// Release coefficient per sample (< 1.0, for multiplication)
    release_coeff: f64,
    /// Number of channels
    channels: usize,
    /// Sample rate (needed for in-place release_ms updates)
    sample_rate: f64,
}

/// Frames the attack ramp finishes *early*, before the frame that armed it is
/// output.
///
/// The ceiling is a true-peak ceiling, so what must stay under the threshold is
/// not the output sample but the FIR reconstruction around it, which mixes the
/// 12 input frames ending at that sample. A gain that is still moving across
/// that span mixes frames reduced by *different* amounts, and the earlier ones
/// (higher gain, because attack descends) push the reconstruction above the
/// value the outgoing sample alone implies. Instant attack never had this
/// problem: it dropped straight to the window floor and held it flat for the
/// whole span.
///
/// Settling `TRUE_PEAK_DELAY` frames early restores that flatness. It covers the
/// 12-tap intersample span with a frame to spare, so by the time the frame that
/// armed the ramp is output, the gain has been constant at its target across
/// that frame's entire reconstruction neighbourhood.
const RAMP_SETTLE_FRAMES: usize = TRUE_PEAK_DELAY;

/// Frames the attack ramp may spend, given an active output delay.
///
/// A frame's control peak enters the sliding window `delay_frames - 1` frames
/// before that frame reaches the output, so that is the raw budget. The ramp
/// gives back [`RAMP_SETTLE_FRAMES`] of it so the descent has flattened before
/// the peak frame emerges. With too little lookahead to fund both the ramp
/// degenerates to a single frame, which is the historical instant attack, so a
/// short lookahead loses the smoothing rather than the ceiling.
fn attack_frames_for(delay_frames: usize) -> usize {
    delay_frames
        .saturating_sub(1)
        .saturating_sub(RAMP_SETTLE_FRAMES)
        .max(1)
}

/// Reject limiter construction values that would disable or destabilize the
/// limiter: a `NaN` threshold makes every peak comparison false (silent
/// bypass), a negative release time flips the gain recursion into exponential
/// growth, and a non-finite lookahead has no meaningful buffer size.
fn validate_limiter_values(
    threshold_db: f64,
    lookahead_ms: f64,
    release_ms: f64,
) -> Result<(), ProcessError> {
    if !threshold_db.is_finite() {
        return Err(ProcessError::InvalidParameter {
            processor: "PeakLimiter",
            parameter: "limiter threshold dB",
            message: "value must be finite",
        });
    }
    if !lookahead_ms.is_finite() || lookahead_ms < 0.0 {
        return Err(ProcessError::InvalidParameter {
            processor: "PeakLimiter",
            parameter: "limiter lookahead ms",
            message: "value must be finite and non-negative",
        });
    }
    if !release_ms.is_finite() || release_ms < 0.0 {
        return Err(ProcessError::InvalidParameter {
            processor: "PeakLimiter",
            parameter: "limiter release ms",
            message: "value must be finite and non-negative",
        });
    }
    Ok(())
}

impl PeakLimiter {
    /// Create a new peak limiter in the default [`LimiterMode::TruePeak`] mode.
    ///
    /// # Arguments
    /// * `channels` - Number of audio channels
    /// * `sample_rate` - Sample rate in Hz
    /// * `threshold_db` - Threshold in dBTP (default: -1.0)
    /// * `lookahead_ms` - Look-ahead time in ms (default: 10.0)
    /// * `release_ms` - Release time in ms (default: 100.0)
    ///
    /// A non-finite `threshold_db` and a non-finite or negative
    /// `lookahead_ms`/`release_ms` are rejected with
    /// [`ProcessError::InvalidParameter`]: a `NaN` threshold silently disables
    /// limiting, a negative release makes the gain recursion diverge, and a
    /// non-finite lookahead has no meaningful buffer size.
    pub fn new(
        channels: usize,
        sample_rate: u32,
        threshold_db: f64,
        lookahead_ms: f64,
        release_ms: f64,
    ) -> Result<Self, ProcessError> {
        validated_channel_count(channels)?;
        validate_sample_rate_hz("PeakLimiter", sample_rate)?;
        validate_limiter_values(threshold_db, lookahead_ms, release_ms)?;
        Ok(Self::new_validated(
            channels,
            sample_rate,
            threshold_db,
            lookahead_ms,
            release_ms,
        ))
    }

    pub(crate) fn new_validated(
        channels: usize,
        sample_rate: u32,
        threshold_db: f64,
        lookahead_ms: f64,
        release_ms: f64,
    ) -> Self {
        Self::with_mode_validated(
            channels,
            sample_rate,
            threshold_db,
            lookahead_ms,
            release_ms,
            LimiterMode::default(),
        )
    }

    /// Create a new peak limiter with an explicit detection [`LimiterMode`].
    ///
    /// Values are validated as in [`Self::new`].
    pub fn with_mode(
        channels: usize,
        sample_rate: u32,
        threshold_db: f64,
        lookahead_ms: f64,
        release_ms: f64,
        mode: LimiterMode,
    ) -> Result<Self, ProcessError> {
        validated_channel_count(channels)?;
        validate_sample_rate_hz("PeakLimiter", sample_rate)?;
        validate_limiter_values(threshold_db, lookahead_ms, release_ms)?;
        Ok(Self::with_mode_validated(
            channels,
            sample_rate,
            threshold_db,
            lookahead_ms,
            release_ms,
            mode,
        ))
    }

    pub(crate) fn with_mode_validated(
        channels: usize,
        sample_rate: u32,
        threshold_db: f64,
        lookahead_ms: f64,
        release_ms: f64,
        mode: LimiterMode,
    ) -> Self {
        let threshold = db_to_linear(threshold_db);
        let lookahead_frames = ((lookahead_ms / 1000.0) * sample_rate as f64).ceil() as usize;
        let lookahead_frames = lookahead_frames.max(1);

        // Active window depends on mode; the buffer/queue are sized for the
        // worst case (true-peak) so switching modes never reallocates.
        let delay_frames = delay_frames_for(mode, lookahead_frames);
        let max_delay_frames = lookahead_frames + TRUE_PEAK_DELAY;

        // Release coefficient: exp(-1 / tau) where tau = release_samples
        // This gives us a coefficient < 1 for multiplication. The same
        // one-sample floor as `set_release_ms` keeps the recursion contractive
        // (a coefficient above 1.0 would make released gain diverge) even for
        // a kernel caller that skips the public constructor validation.
        let release_samples = (release_ms / 1000.0) * sample_rate as f64;
        let release_coeff = (-1.0 / release_samples.max(1.0)).exp();

        // Warm the shared FIR table during setup, never on the first callback.
        let _ = true_peak_fir();

        // Pre-allocate fixed-size buffers for the worst case. Detectors are
        // always allocated so a switch into true-peak mode is allocation-free.
        let delay_buffer = vec![0.0; max_delay_frames * channels].into_boxed_slice();
        let true_peak_detectors = (0..channels).map(|_| TruePeakDetector::new()).collect();

        Self {
            threshold,
            mode,
            lookahead_frames,
            delay_frames,
            delay_buffer,
            true_peak_detectors,
            peak_queue: MonotonicMaxQueue::new(max_delay_frames),
            global_frame: 0,
            write_pos: 0,
            gain_reduction: 1.0,
            attack_frames: attack_frames_for(delay_frames),
            attack_step: 0.0,
            release_coeff,
            channels,
            sample_rate: sample_rate as f64,
        }
    }

    /// Detection mode in effect.
    pub fn mode(&self) -> LimiterMode {
        self.mode
    }

    /// Active algorithmic output delay in frames.
    pub fn delay_frames(&self) -> usize {
        self.delay_frames
    }

    /// Switch the detection [`LimiterMode`] in place.
    ///
    /// Real-time safe: no allocation (buffers/detectors are pre-sized for the
    /// worst case at construction) and no locks. Changing the active window
    /// invalidates in-flight delay/queue state, so this resets the limiter; do
    /// not call it mid-stream if a glitch-free transition is required.
    pub fn set_mode(&mut self, mode: LimiterMode) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        self.delay_frames = delay_frames_for(mode, self.lookahead_frames);
        self.attack_frames = attack_frames_for(self.delay_frames);
        self.reset();
    }

    /// Process interleaved samples in-place
    ///
    /// This function is real-time safe:
    /// - No heap allocations
    /// - No system calls
    /// - O(n) complexity where n = number of samples
    pub fn process(&mut self, samples: &mut [f64], channels: usize) -> Result<(), ProcessError> {
        let block = AudioBlockMut::new(samples, channels)?;
        validate_processor_channels("PeakLimiter", Some(self.channels), channels)?;
        self.process_validated(block.into_samples());
        Ok(())
    }

    pub(crate) fn process_validated(&mut self, samples: &mut [f64]) {
        let total_samples = samples.len();
        let frames = total_samples / self.channels;
        if frames == 0 {
            return;
        }

        let fir = match self.mode {
            LimiterMode::TruePeak => Some(true_peak_fir()),
            LimiterMode::SamplePeak => None,
        };

        for frame in 0..frames {
            // Step 1: Read peak across the look-ahead window. Query before
            // writing the current input frame to preserve delay-buffer
            // semantics exactly.
            let peak = self.peak_queue.current_peak();

            // Step 2: Calculate required gain reduction for the whole window.
            let target_gain = if peak > self.threshold {
                self.threshold / peak
            } else {
                1.0
            };

            // Step 3: Move toward the target. Attack descends linearly across
            // the lookahead instead of stepping in one sample; release keeps its
            // existing exponential recovery.
            if target_gain < self.gain_reduction {
                // Attack: arm (or steepen) a ramp that closes the remaining gap
                // within `attack_frames`. Re-arming may only steepen, because a
                // gentler schedule adopted mid-descent could overshoot the
                // deadline of a frame already inside the window.
                let step = (self.gain_reduction - target_gain) / self.attack_frames as f64;
                self.attack_step = self.attack_step.max(step);
                self.gain_reduction = (self.gain_reduction - self.attack_step).max(target_gain);
            } else {
                // Release: smooth recovery
                self.attack_step = 0.0;
                self.gain_reduction =
                    self.gain_reduction + (1.0 - self.gain_reduction) * (1.0 - self.release_coeff);
                // Ensure we don't exceed target
                self.gain_reduction = self.gain_reduction.min(target_gain);
            }

            // Step 4: Read from delay buffer, write new samples, apply gain.
            // The per-frame control peak is either the sample peak across
            // channels or the max intersample peak from the FIR detectors.
            let mut frame_peak = 0.0_f64;
            for ch in 0..self.channels {
                let input_idx = frame * self.channels + ch;
                let buffer_idx = self.write_pos * self.channels + ch;
                let input = samples[input_idx];

                frame_peak = match fir {
                    Some(fir) => {
                        frame_peak.max(self.true_peak_detectors[ch].intersample_peak(input, fir))
                    }
                    None => frame_peak.max(input.abs()),
                };

                // Get delayed sample
                let delayed = self.delay_buffer[buffer_idx];

                // Store new sample in buffer
                self.delay_buffer[buffer_idx] = input;

                // Output delayed sample with gain reduction
                samples[input_idx] = delayed * self.gain_reduction;
            }

            self.push_frame_peak(frame_peak);

            // Advance write position
            self.write_pos = (self.write_pos + 1) % self.delay_frames;
        }
    }

    #[inline]
    fn push_frame_peak(&mut self, frame_peak: f64) {
        if self.global_frame >= self.delay_frames as u64 {
            self.peak_queue
                .expire_through(self.global_frame - self.delay_frames as u64);
        }
        self.peak_queue.push(self.global_frame, frame_peak);
        self.global_frame = self.global_frame.wrapping_add(1);
    }

    /// Set threshold in dB.
    ///
    /// A non-finite value is dropped: a `NaN` threshold makes every gain
    /// comparison false, so the limiter would silently stop limiting.
    ///
    /// No published-range clamp is applied here. The facade owns
    /// [`LIMITER_THRESHOLD_DB_MIN`](crate::processor::LIMITER_THRESHOLD_DB_MIN)..=[`LIMITER_THRESHOLD_DB_MAX`](crate::processor::LIMITER_THRESHOLD_DB_MAX)
    /// for what a
    /// *user* may ask for; the adapter then drives this core below that range
    /// on purpose, because the intersample-peak guard subtracts its additive
    /// bound from the user's ceiling before it reaches the limiter.
    pub fn set_threshold_db(&mut self, threshold_db: f64) {
        self.set_threshold(threshold_db);
    }

    /// Update threshold in-place without reallocating lookahead buffer.
    /// Non-finite input is dropped, as in [`Self::set_threshold_db`].
    pub fn set_threshold(&mut self, threshold_db: f64) {
        if threshold_db.is_finite() {
            self.threshold = db_to_linear(threshold_db);
        }
    }

    /// Update release time in-place without reallocating lookahead buffer.
    ///
    /// Non-finite input is dropped; the existing `max(1.0)` floor keeps the
    /// coefficient finite for any remaining value.
    pub fn set_release_ms(&mut self, release_ms: f64) {
        if !release_ms.is_finite() {
            return;
        }
        let release_samples = (release_ms / 1000.0) * self.sample_rate;
        self.release_coeff = (-1.0 / release_samples.max(1.0)).exp();
    }

    /// Always `true`: a `PeakLimiter` has no bypass of its own.
    ///
    /// Bypass is owned one level up. `PeakLimiterProcessor` gates its call into
    /// this limiter on its own cached `enabled` flag, so a disabled stage never
    /// reaches this type at all.
    ///
    /// Consequently this method cannot be used to detect an enable/disable
    /// transition — comparing it against a desired state yields "is the desired
    /// state disabled", not "did the state change". Track the previous published
    /// value instead.
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
        for detector in &mut self.true_peak_detectors {
            detector.reset();
        }
        self.peak_queue.clear();
        self.global_frame = 0;
        self.write_pos = 0;
        self.gain_reduction = 1.0;
        self.attack_step = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processor::lockfree_params::LIMITER_THRESHOLD_DB_MIN;
    use crate::processor::traits::AudioBlockError;

    /// The 2026-08 review: public construction accepted a negative release
    /// (whose `exp(-1/negative) > 1` coefficient makes released gain diverge
    /// without bound after the first limiting event) and a `NaN` threshold
    /// (which silently disables limiting). Both must fail loudly at the
    /// build-time boundary, matching the crate's strict-constructor policy.
    #[test]
    fn constructors_reject_non_finite_or_negative_values() {
        let cases: [(f64, f64, f64, &str); 7] = [
            (f64::NAN, 10.0, 100.0, "limiter threshold dB"),
            (f64::INFINITY, 10.0, 100.0, "limiter threshold dB"),
            (-1.0, f64::NAN, 100.0, "limiter lookahead ms"),
            (-1.0, f64::INFINITY, 100.0, "limiter lookahead ms"),
            (-1.0, -5.0, 100.0, "limiter lookahead ms"),
            (-1.0, 10.0, f64::NAN, "limiter release ms"),
            (-1.0, 10.0, -100.0, "limiter release ms"),
        ];
        for (threshold_db, lookahead_ms, release_ms, parameter) in cases {
            for result in [
                PeakLimiter::new(2, 48_000, threshold_db, lookahead_ms, release_ms).err(),
                PeakLimiter::with_mode(
                    2,
                    48_000,
                    threshold_db,
                    lookahead_ms,
                    release_ms,
                    LimiterMode::SamplePeak,
                )
                .err(),
            ] {
                assert_eq!(
                    result,
                    Some(ProcessError::InvalidParameter {
                        processor: "PeakLimiter",
                        parameter,
                        message: if parameter == "limiter threshold dB" {
                            "value must be finite"
                        } else {
                            "value must be finite and non-negative"
                        },
                    }),
                    "({threshold_db}, {lookahead_ms}, {release_ms}) must be rejected"
                );
            }
        }

        // The documented defaults still construct.
        assert!(PeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0).is_ok());
        // Zero lookahead/release are meaningful degenerate values: one-frame
        // lookahead and the same one-sample release floor as `set_release_ms`.
        assert!(PeakLimiter::new(2, 48_000, -1.0, 0.0, 0.0).is_ok());
    }

    /// Released gain must decay back toward unity; with the pre-fix negative
    /// release this recursion diverged instead.
    #[test]
    fn release_recursion_is_contractive_for_every_accepted_release() {
        for release_ms in [0.0, 1.0, 100.0, 5_000.0] {
            let limiter = PeakLimiter::new(1, 48_000, -1.0, 10.0, release_ms).unwrap();
            assert!(
                limiter.release_coeff >= 0.0 && limiter.release_coeff < 1.0,
                "release {release_ms} ms produced non-contractive coefficient {}",
                limiter.release_coeff
            );
        }
    }

    /// A `NaN` threshold makes every `peak > threshold` comparison false, so
    /// the limiter would silently stop limiting rather than fail loudly.
    #[test]
    fn limiter_setters_drop_non_finite_writes() {
        let mut limiter = PeakLimiter::new(2, 48_000, -3.0, 5.0, 120.0).unwrap();
        let threshold = limiter.threshold;
        let release_coeff = limiter.release_coeff;

        for poison in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            limiter.set_threshold_db(poison);
            limiter.set_threshold(poison);
            limiter.set_release_ms(poison);
            assert_eq!(limiter.threshold, threshold, "threshold survived {poison}");
            assert_eq!(
                limiter.release_coeff, release_coeff,
                "release survived {poison}"
            );
        }
    }

    /// The core must stay drivable outside the facade's published range: the
    /// intersample-peak guard in `PeakLimiterProcessor` subtracts its additive
    /// bound from the user's ceiling, so a user ceiling at the published
    /// minimum reaches this core *below* that minimum. Clamping here would
    /// silently disable that guard.
    #[test]
    fn limiter_threshold_is_drivable_below_the_published_user_range() {
        let mut limiter = PeakLimiter::new(2, 48_000, -3.0, 5.0, 120.0).unwrap();
        let guarded_db = LIMITER_THRESHOLD_DB_MIN - 0.25;

        limiter.set_threshold(guarded_db);

        assert!((limiter.threshold - db_to_linear(guarded_db)).abs() < 1e-12);
        assert!(limiter.threshold < db_to_linear(LIMITER_THRESHOLD_DB_MIN));
        assert!(limiter.threshold > 0.0);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct PeakLimiterState {
        delay_buffer: Vec<u64>,
        detector_peaks: Vec<u64>,
        queue_indices: Vec<u64>,
        queue_peaks: Vec<u64>,
        queue_head: usize,
        queue_tail: usize,
        queue_len: usize,
        global_frame: u64,
        write_pos: usize,
        gain_reduction: u64,
        attack_step: u64,
    }

    fn peak_limiter_state(limiter: &PeakLimiter) -> PeakLimiterState {
        PeakLimiterState {
            delay_buffer: limiter
                .delay_buffer
                .iter()
                .map(|value| value.to_bits())
                .collect(),
            detector_peaks: limiter
                .true_peak_detectors
                .iter()
                .map(|detector| detector.max_true_peak().to_bits())
                .collect(),
            queue_indices: limiter.peak_queue.indices.to_vec(),
            queue_peaks: limiter
                .peak_queue
                .peaks
                .iter()
                .map(|value| value.to_bits())
                .collect(),
            queue_head: limiter.peak_queue.head,
            queue_tail: limiter.peak_queue.tail,
            queue_len: limiter.peak_queue.len,
            global_frame: limiter.global_frame,
            write_pos: limiter.write_pos,
            gain_reduction: limiter.gain_reduction.to_bits(),
            attack_step: limiter.attack_step.to_bits(),
        }
    }

    /// Brute-force O(N·L) reference for the monotonic max queue.
    ///
    /// This exists to prove the queue's sliding maximum, not the gain shape, so
    /// it mirrors the attack ramp and the outgoing-frame ceiling clamp exactly.
    /// If it kept the historical instant attack the differential tests would
    /// compare two different limiters and fail for a reason unrelated to the
    /// queue.
    struct LegacyPeakLimiter {
        threshold: f64,
        lookahead_frames: usize,
        delay_buffer: Box<[f64]>,
        write_pos: usize,
        gain_reduction: f64,
        attack_frames: usize,
        attack_step: f64,
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
                attack_frames: attack_frames_for(lookahead_frames),
                attack_step: 0.0,
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
                    let step = (self.gain_reduction - target_gain) / self.attack_frames as f64;
                    self.attack_step = self.attack_step.max(step);
                    self.gain_reduction = (self.gain_reduction - self.attack_step).max(target_gain);
                } else {
                    self.attack_step = 0.0;
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
        let mut limiter =
            PeakLimiter::with_mode_validated(2, 48_000, -1.0, 10.0, 100.0, LimiterMode::SamplePeak);
        let mut legacy = LegacyPeakLimiter::new(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = deterministic_transient_corpus(2_000, 2);
        let mut expected = samples.clone();

        limiter.process_validated(&mut samples);
        legacy.process(&mut expected);

        assert_samples_eq(&samples, &expected);
    }

    #[test]
    fn monotonic_queue_preserves_cross_buffer_continuity() {
        let source = deterministic_transient_corpus(6_400, 2);
        let mut one_shot = source.clone();
        let mut chunked = source.clone();

        let mut one_shot_limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        let mut chunked_limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);

        one_shot_limiter.process_validated(&mut one_shot);
        for chunk in chunked.chunks_mut(64 * 2) {
            chunked_limiter.process_validated(chunk);
        }

        assert_samples_eq(&chunked, &one_shot);
    }

    /// D2 (2026-08-11 review): the attack branch used to assign
    /// `gain_reduction = target_gain` in a single sample. A transient 6 dB over
    /// the ceiling snapped the gain 1.0 -> 0.5 on one sample and applied that
    /// step to sustained program material already sitting in the delay line,
    /// an audible thud ~10 ms before the transient.
    ///
    /// The ramp now spends the lookahead. Bound: the steepest legal descent
    /// closes the largest possible gap (1.0 down to `threshold/peak`) across
    /// `attack_frames`, so no single frame may move the gain by more than
    /// `(1.0 - threshold/peak) / attack_frames`. The pre-fix code violated this
    /// by a factor of `attack_frames`.
    #[test]
    fn attack_ramps_across_the_lookahead_instead_of_stepping() {
        let sample_rate = 48_000;
        let lookahead_ms = 10.0;
        let threshold_db = -1.0;
        let mut limiter = PeakLimiter::with_mode_validated(
            2,
            sample_rate,
            threshold_db,
            lookahead_ms,
            100.0,
            LimiterMode::SamplePeak,
        );

        // Quiet sustained bed, then a transient 6 dB over the ceiling. The bed
        // is what the old hard step was audibly ducking.
        //
        // The transient goes on channel 0 only. Gain is computed from the peak
        // across channels but applied to both, so channel 1 stays a constant bed
        // and `out / bed` recovers the applied gain at every frame with no
        // contaminated samples and no delay-offset arithmetic to get wrong.
        let bed = db_to_linear(-20.0);
        let transient = db_to_linear(threshold_db + 6.0);
        let frames = 4_000;
        let mut samples = vec![bed; frames * 2];
        samples[2_000 * 2] = transient;

        let lookahead_frames = ((lookahead_ms / 1000.0) * sample_rate as f64).ceil() as usize;
        let attack_frames = attack_frames_for(lookahead_frames);
        let threshold = db_to_linear(threshold_db);
        let max_step = (1.0 - threshold / transient) / attack_frames as f64;

        limiter.process_validated(&mut samples);

        let mut previous = 1.0_f64;
        let mut worst_step = 0.0_f64;
        let mut ramp_frames = 0usize;
        // Skip the priming window: output is the zero-filled delay line there.
        for frame in lookahead_frames..frames {
            let gain = samples[frame * 2 + 1] / bed;
            let step = previous - gain;
            if step > 1e-12 {
                worst_step = worst_step.max(step);
                ramp_frames += 1;
            }
            previous = gain;
        }

        assert!(
            worst_step <= max_step * (1.0 + 1e-9),
            "single-frame gain step {worst_step} exceeds the derived bound {max_step} \
             (attack_frames={attack_frames})"
        );
        assert!(
            ramp_frames > 1,
            "attack collapsed to {ramp_frames} frame(s); the ramp is not engaging"
        );
    }

    /// The ramp is a smoothing device and must not become a correctness hole:
    /// the ceiling holds even while the gain is still descending. This is the
    /// guarantee the 2026-08-11 review derived and closed, and
    /// [`RAMP_SETTLE_FRAMES`] is what preserves it.
    ///
    /// Measured as a **true** peak, not `sample.abs()`. In true-peak mode the
    /// ceiling the limiter promises is on the reconstruction, and a gain still
    /// moving across the FIR span breaches that while every individual sample
    /// still looks compliant -- an earlier draft of this test measured sample
    /// peaks, passed, and let a ~0.01 dB true-peak overshoot through to
    /// `output_chain`'s guard test.
    #[test]
    fn attack_ramp_never_lets_a_sample_exceed_the_ceiling() {
        let threshold_db = -1.0;
        let threshold = db_to_linear(threshold_db);

        for mode in [LimiterMode::SamplePeak, LimiterMode::TruePeak] {
            let mut limiter =
                PeakLimiter::with_mode_validated(2, 48_000, threshold_db, 10.0, 100.0, mode);
            // Staircase of escalating over-ceiling transients: each one re-arms
            // the ramp mid-descent, which is the case a naive ramp gets wrong.
            let mut samples = vec![db_to_linear(-20.0); 6_000 * 2];
            for (step, over_db) in [3.0, 6.0, 9.0, 12.0, 18.0].iter().enumerate() {
                let amplitude = db_to_linear(threshold_db + over_db);
                let base = (500 + step * 900) * 2;
                for ch in 0..2 {
                    samples[base + ch] = amplitude;
                    samples[base + 2 + ch] = amplitude;
                }
            }

            limiter.process_validated(&mut samples);

            let worst_sample = samples.iter().fold(0.0_f64, |acc, s| acc.max(s.abs()));
            assert!(
                worst_sample <= threshold + 1e-12,
                "{mode:?}: output sample peak {worst_sample} exceeded ceiling {threshold}"
            );

            // Re-measure the rendered output the way a compliance meter would.
            // Only true-peak mode promises anything about the reconstruction --
            // sample-peak mode deliberately does not, which is the whole point
            // of `sample_peak_mode_misses_what_true_peak_catches`.
            if mode == LimiterMode::SamplePeak {
                continue;
            }
            let mut worst_true = 0.0_f64;
            for ch in 0..2 {
                let mut detector = TruePeakDetector::new();
                detector.process_strided(&samples, ch, 2);
                detector.process(&[0.0; 16]);
                worst_true = worst_true.max(detector.max_true_peak());
            }
            assert!(
                worst_true <= threshold + 1e-9,
                "{mode:?}: output TRUE peak {worst_true} ({:.6} dBTP) exceeded ceiling \
                 {threshold} ({threshold_db:.6} dBTP); the ramp is still moving across the \
                 reconstruction span",
                linear_to_db(worst_true)
            );
        }
    }

    #[test]
    fn monotonic_queue_handles_sustained_pre_clipping() {
        let mut limiter =
            PeakLimiter::with_mode_validated(2, 48_000, -1.0, 10.0, 100.0, LimiterMode::SamplePeak);
        let mut samples = vec![1.2; 2_000 * 2];

        limiter.process_validated(&mut samples);

        let expected_gain = db_to_linear(-1.0) / 1.2;
        assert!((limiter.gain_reduction - expected_gain).abs() < 1e-12);
        assert!(samples
            .iter()
            .all(|sample| sample.abs() <= db_to_linear(-1.0) + 1e-12));
    }

    #[test]
    fn monotonic_queue_resets_state() {
        let mut limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = deterministic_transient_corpus(1_000, 2);

        limiter.process_validated(&mut samples);
        assert!(limiter.peak_queue.current_peak() > 0.0);

        limiter.reset();

        assert_eq!(limiter.peak_queue.current_peak(), 0.0);
        assert_eq!(limiter.global_frame, 0);
        assert_eq!(limiter.write_pos, 0);
        assert_eq!(limiter.gain_reduction, 1.0);
        assert_eq!(limiter.attack_step, 0.0);
    }

    #[test]
    fn lookahead_one_frame_matches_legacy_scan() {
        let mut limiter =
            PeakLimiter::with_mode_validated(2, 1_000, -1.0, 1.0, 10.0, LimiterMode::SamplePeak);
        let mut legacy = LegacyPeakLimiter::new(2, 1_000, -1.0, 1.0, 10.0);
        let mut samples = deterministic_transient_corpus(128, 2);
        let mut expected = samples.clone();

        limiter.process_validated(&mut samples);
        legacy.process(&mut expected);

        assert_samples_eq(&samples, &expected);
    }

    #[test]
    fn non_finite_samples_do_not_poison_queue_peak() {
        let mut limiter =
            PeakLimiter::with_mode_validated(2, 48_000, -1.0, 10.0, 100.0, LimiterMode::SamplePeak);
        let mut samples = vec![0.2; 64 * 2];
        samples[4] = f64::NAN;
        samples[9] = f64::INFINITY;

        limiter.process_validated(&mut samples);

        assert!(limiter.peak_queue.current_peak().is_infinite());

        let mut finite_samples = vec![0.25; 600 * 2];
        limiter.process_validated(&mut finite_samples);

        assert!(limiter.peak_queue.current_peak().is_finite());
        assert_eq!(limiter.peak_queue.current_peak(), 0.25);
    }

    #[test]
    fn process_is_steady_state_no_alloc() {
        let mut limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = deterministic_transient_corpus(64, 2);

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..1_000 {
                limiter.process_validated(&mut samples);
            }
        });
    }

    // --- True-peak mode -----------------------------------------------------

    use crate::analysis::TruePeakDetector;

    /// Sine at Fs/4 sampled 45° off-peak: every sample sits at ±amplitude·√½
    /// (well below a -1 dBTP ceiling) while the reconstructed intersample peak
    /// reaches `amplitude`. This is the canonical case a sample-peak limiter
    /// misses and a true-peak limiter must catch.
    fn intersample_stress(frames: usize, channels: usize, amplitude: f64) -> Vec<f64> {
        let mut samples = Vec::with_capacity(frames * channels);
        for n in 0..frames {
            let value = amplitude * (std::f64::consts::PI * (n as f64 + 0.5) / 2.0).sin();
            for _ in 0..channels {
                samples.push(value);
            }
        }
        samples
    }

    /// Measure the interleaved buffer's true peak (linear) with the same 4x FIR
    /// the limiter detects with, so the assertion reflects the actual guarantee.
    fn measure_true_peak(samples: &[f64], channels: usize) -> f64 {
        let mut detectors: Vec<TruePeakDetector> =
            (0..channels).map(|_| TruePeakDetector::new()).collect();
        for (ch, detector) in detectors.iter_mut().enumerate() {
            detector.process_strided(samples, ch, channels);
        }
        detectors
            .iter()
            .map(|d| d.max_true_peak())
            .fold(0.0_f64, f64::max)
    }

    fn sample_peak(samples: &[f64]) -> f64 {
        samples.iter().map(|s| s.abs()).fold(0.0_f64, f64::max)
    }

    #[test]
    fn default_mode_is_true_peak() {
        let limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        assert_eq!(limiter.mode(), LimiterMode::TruePeak);
    }

    #[test]
    fn true_peak_mode_limits_intersample_stress_below_ceiling() {
        let ceiling = db_to_linear(-1.0);
        let mut samples = intersample_stress(4_000, 2, 1.0);

        // Sample peak is below the ceiling, so the input alone looks "safe".
        assert!(sample_peak(&samples) < ceiling);

        let mut limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        limiter.process_validated(&mut samples);

        // Ignore the leading delay (zeros) when judging steady-state output.
        let steady = &samples[2 * limiter.delay_frames..];
        let out_tp = measure_true_peak(steady, 2);
        let tol = db_to_linear(0.1); // measured with the same FIR as detection
        assert!(
            out_tp <= ceiling * tol,
            "true-peak output {out_tp} exceeds ceiling {ceiling}"
        );
    }

    #[test]
    fn sample_peak_mode_misses_what_true_peak_catches() {
        let ceiling = db_to_linear(-1.0);
        let mut sp = intersample_stress(4_000, 2, 1.0);
        let mut tp = sp.clone();

        PeakLimiter::with_mode_validated(2, 48_000, -1.0, 10.0, 100.0, LimiterMode::SamplePeak)
            .process_validated(&mut sp);
        PeakLimiter::with_mode_validated(2, 48_000, -1.0, 10.0, 100.0, LimiterMode::TruePeak)
            .process_validated(&mut tp);

        // Sample-peak mode never engages (sample peak < ceiling) and leaves the
        // intersample peak above the ceiling; true-peak mode pulls it under.
        assert!(measure_true_peak(&sp, 2) > ceiling);
        assert!(measure_true_peak(&tp[2 * 490..], 2) <= ceiling * db_to_linear(0.1));
    }

    #[test]
    fn true_peak_covers_mono_stereo_and_multichannel() {
        let ceiling = db_to_linear(-1.0);
        for channels in [1usize, 2, 6] {
            let mut samples = intersample_stress(4_000, channels, 1.0);
            let mut limiter = PeakLimiter::new_validated(channels, 48_000, -1.0, 10.0, 100.0);
            limiter.process_validated(&mut samples);

            let steady = &samples[channels * limiter.delay_frames..];
            let out_tp = measure_true_peak(steady, channels);
            assert!(
                out_tp <= ceiling * db_to_linear(0.1),
                "channels={channels}: true-peak output {out_tp} exceeds ceiling {ceiling}"
            );
        }
    }

    #[test]
    fn true_peak_preserves_cross_buffer_continuity() {
        let source = intersample_stress(6_400, 2, 1.0);
        let mut one_shot = source.clone();
        let mut chunked = source;

        let mut one_shot_limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        let mut chunked_limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);

        one_shot_limiter.process_validated(&mut one_shot);
        for chunk in chunked.chunks_mut(64 * 2) {
            chunked_limiter.process_validated(chunk);
        }

        assert_samples_eq(&chunked, &one_shot);
    }

    #[test]
    fn true_peak_resets_all_state() {
        let mut limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = intersample_stress(1_000, 2, 1.0);

        limiter.process_validated(&mut samples);
        assert!(limiter.peak_queue.current_peak() > 0.0);
        assert!(limiter.gain_reduction < 1.0);

        limiter.reset();

        assert_eq!(limiter.peak_queue.current_peak(), 0.0);
        assert_eq!(limiter.global_frame, 0);
        assert_eq!(limiter.write_pos, 0);
        assert_eq!(limiter.gain_reduction, 1.0);
        for detector in &limiter.true_peak_detectors {
            assert_eq!(detector.max_true_peak(), 0.0);
        }
    }

    #[test]
    fn true_peak_silence_stays_silent_and_unity_gain() {
        let mut limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = vec![0.0; 2_000 * 2];

        limiter.process_validated(&mut samples);

        assert!(samples.iter().all(|s| *s == 0.0));
        assert_eq!(limiter.gain_reduction, 1.0);
    }

    #[test]
    fn true_peak_bounds_sustained_over_threshold_material() {
        let ceiling = db_to_linear(-1.0);
        let mut samples = intersample_stress(4_000, 2, 1.5);
        // Both sample peak and true peak are over the ceiling here.
        assert!(sample_peak(&samples) > ceiling);

        let mut limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        limiter.process_validated(&mut samples);

        let steady = &samples[2 * limiter.delay_frames..];
        assert!(measure_true_peak(steady, 2) <= ceiling * db_to_linear(0.1));
    }

    #[test]
    fn true_peak_recovers_after_non_finite_samples() {
        let mut limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = vec![0.2; 64 * 2];
        samples[4] = f64::NAN;
        samples[9] = f64::INFINITY;

        // Policy: non-finite input is not sanitized, but must not panic and the
        // detector/queue must recover to a finite state once it flushes through.
        limiter.process_validated(&mut samples);

        let mut finite = vec![0.25; 4_000 * 2];
        limiter.process_validated(&mut finite);

        assert!(limiter.peak_queue.current_peak().is_finite());
        assert!(limiter.gain_reduction.is_finite());
    }

    #[test]
    fn true_peak_process_is_steady_state_no_alloc() {
        let mut limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        let mut samples = intersample_stress(64, 2, 1.0);

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..1_000 {
                limiter.process_validated(&mut samples);
            }
        });
    }

    #[test]
    fn set_mode_is_allocation_free_and_switches_active_delay() {
        let mut limiter = PeakLimiter::new_validated(2, 48_000, -1.0, 10.0, 100.0);
        assert_eq!(limiter.mode(), LimiterMode::TruePeak);
        let true_peak_delay = limiter.delay_frames;

        let mut samples = intersample_stress(64, 2, 1.0);

        // Switching modes and processing both ways must not allocate: buffers
        // and detectors are pre-sized for the worst case at construction.
        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..100 {
                limiter.set_mode(LimiterMode::SamplePeak);
                limiter.process_validated(&mut samples);
                limiter.set_mode(LimiterMode::TruePeak);
                limiter.process_validated(&mut samples);
            }
        });

        // Active delay tracks the mode (sample-peak is shorter by the FIR span).
        limiter.set_mode(LimiterMode::SamplePeak);
        assert_eq!(limiter.delay_frames + TRUE_PEAK_DELAY, true_peak_delay);
        // Idempotent: re-selecting the current mode is a no-op (no reset churn).
        let before = limiter.delay_frames;
        limiter.set_mode(LimiterMode::SamplePeak);
        assert_eq!(limiter.delay_frames, before);
    }

    #[test]
    fn raw_peak_limiter_rejects_invalid_setup_and_block_geometry_atomically() {
        assert!(matches!(
            PeakLimiter::new(0, 48_000, -1.0, 10.0, 100.0),
            Err(ProcessError::InvalidBlock(AudioBlockError::ZeroChannels))
        ));
        assert!(matches!(
            PeakLimiter::with_mode(2, 0, -1.0, 10.0, 100.0, LimiterMode::SamplePeak),
            Err(ProcessError::InvalidSampleRate {
                processor: "PeakLimiter",
                sample_rate_hz: 0,
            })
        ));

        let mut limiter =
            PeakLimiter::with_mode(2, 48_000, -1.0, 10.0, 100.0, LimiterMode::SamplePeak).unwrap();
        let mut warm = [0.25; 16];
        limiter.process(&mut warm, 2).unwrap();
        let state = peak_limiter_state(&limiter);

        let mut zero_channels = [0.25; 4];
        let zero_channels_before = zero_channels;
        let mut incomplete = [0.25; 3];
        let incomplete_before = incomplete;
        let mut mismatch = [0.25; 4];
        let mismatch_before = mismatch;
        assert_no_alloc::assert_no_alloc(|| {
            assert_eq!(
                limiter.process(&mut zero_channels, 0),
                Err(ProcessError::InvalidBlock(AudioBlockError::ZeroChannels))
            );
            assert_eq!(
                limiter.process(&mut incomplete, 2),
                Err(ProcessError::InvalidBlock(
                    AudioBlockError::IncompleteFrame {
                        samples: 3,
                        channels: 2,
                    }
                ))
            );
            assert_eq!(
                limiter.process(&mut mismatch, 1),
                Err(ProcessError::ChannelCountMismatch {
                    processor: "PeakLimiter",
                    expected_channels: 2,
                    actual_channels: 1,
                })
            );
        });

        assert_eq!(zero_channels, zero_channels_before);
        assert_eq!(incomplete, incomplete_before);
        assert_eq!(mismatch, mismatch_before);
        assert_eq!(peak_limiter_state(&limiter), state);
    }
}
