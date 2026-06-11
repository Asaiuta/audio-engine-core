//! Atomic loudness state for lock-free audio-thread access.
//!
//! The main thread mutates target gains / mode / preamp via the helpers below;
//! the audio thread reads them in `process_gain` with relaxed ordering. The reads
//! are intentionally independent rather than a single consistent snapshot: a mode
//! switch and its matching gains are not published atomically, so the callback can
//! briefly observe a mode from one update with a gain from another. That transient
//! is inaudible because the applied gain is exponentially smoothed toward its
//! target (~200 ms), so one block of a slightly-off target only nudges the smoother
//! and is corrected on the next read.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use atomic_float::AtomicF64;

use crate::config::NormalizationMode;
use crate::processor::dsp::db_to_linear;

use super::info::LoudnessInfo;

/// Atomic loudness state for lock-free audio thread access.
/// Uses AtomicF64 with Relaxed ordering (gains don't need strict synchronization).
pub struct AtomicLoudnessState {
    /// Target gain in dB (set by main thread, read by audio thread)
    pub target_gain_db: AtomicF64,
    /// Current smoothed gain in dB (updated by audio thread)
    pub current_gain_db: AtomicF64,
    /// Smoothing coefficient per sample (< 1.0, for multiplication)
    pub smoothing_coeff: AtomicF64,
    /// Album gain for Album mode (same for all tracks in album)
    pub album_gain_db: AtomicF64,
    /// Preamp gain for headroom adjustment (default -3 dB)
    pub preamp_gain_db: AtomicF64,
    /// Enable/disable normalization
    pub enabled: AtomicBool,
    /// Normalization mode: 0=Track, 1=Album, 2=Streaming
    pub mode: AtomicU8,
}

impl AtomicLoudnessState {
    pub fn new(smoothing_time_ms: f64, sample_rate: u32) -> Self {
        let smoothing_coeff = {
            let smoothing_samples = (smoothing_time_ms / 1000.0) * sample_rate as f64;
            (-1.0 / smoothing_samples).exp()
        };

        Self {
            target_gain_db: AtomicF64::new(0.0),
            current_gain_db: AtomicF64::new(0.0),
            smoothing_coeff: AtomicF64::new(smoothing_coeff),
            album_gain_db: AtomicF64::new(0.0),
            preamp_gain_db: AtomicF64::new(-1.0), // Reduced headroom for better dynamics
            enabled: AtomicBool::new(true),
            mode: AtomicU8::new(0),
        }
    }

    /// Set target gain.
    ///
    /// H-2 fix: Guards against NaN/Infinity values that could propagate through the
    /// audio path and produce corrupted output by coercing them to 0 dB (no gain).
    /// This is reachable from the audio thread in streaming mode
    /// (`LoudnessNormalizer::process`), so the non-finite branch must stay
    /// allocation- and log-free; non-realtime callers that want to report invalid
    /// loudness (e.g. `analyze_track`) log it on their own path.
    pub fn set_target_gain(&self, gain_db: f64) {
        let value = if gain_db.is_finite() { gain_db } else { 0.0 };
        self.target_gain_db.store(value, Ordering::Relaxed);
    }

    /// Set album gain (call from main thread)
    pub fn set_album_gain(&self, gain_db: f64) {
        self.album_gain_db.store(gain_db, Ordering::Relaxed);
    }

    /// Set preamp gain in dB (call from main thread)
    pub fn set_preamp_gain(&self, gain_db: f64) {
        self.preamp_gain_db.store(gain_db, Ordering::Relaxed);
    }

    /// Set enabled state
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Set mode: 0=Track, 1=Album, 2=Streaming, 3=ReplayGainTrack, 4=ReplayGainAlbum
    pub fn set_mode(&self, mode: u8) {
        self.mode.store(mode, Ordering::Relaxed);
    }

    /// Get normalization mode as enum
    pub fn get_mode(&self) -> NormalizationMode {
        match self.mode.load(Ordering::Relaxed) {
            0 => NormalizationMode::Track,
            1 => NormalizationMode::Album,
            2 => NormalizationMode::Streaming,
            3 => NormalizationMode::ReplayGainTrack,
            4 => NormalizationMode::ReplayGainAlbum,
            _ => NormalizationMode::Track,
        }
    }

    /// Update smoothing coefficient
    pub fn set_smoothing(&self, smoothing_time_ms: f64, sample_rate: u32) {
        let smoothing_samples = (smoothing_time_ms / 1000.0) * sample_rate as f64;
        let coeff = (-1.0 / smoothing_samples).exp();
        self.smoothing_coeff.store(coeff, Ordering::Relaxed);
    }

    /// Process gain for a chunk (call from audio thread - lock-free)
    /// Returns the linear gain to apply (includes preamp)
    ///
    /// All fields are read with Relaxed ordering. They are independent atomics, so
    /// this is deliberately not a consistent snapshot: a concurrent main-thread
    /// update can leave the mode and the gains momentarily mismatched for one block.
    /// That is acceptable because the resulting gain is exponentially smoothed (see
    /// `remaining_factor` below), so a single slightly-wrong target is inaudible and
    /// self-corrects on the next call.
    #[inline]
    pub fn process_gain(&self, frames: usize) -> f64 {
        if !self.enabled.load(Ordering::Relaxed) {
            return 1.0;
        }

        let mode = self.mode.load(Ordering::Relaxed);
        let target = self.target_gain_db.load(Ordering::Relaxed);
        let current = self.current_gain_db.load(Ordering::Relaxed);
        let coeff = self.smoothing_coeff.load(Ordering::Relaxed);
        let preamp = self.preamp_gain_db.load(Ordering::Relaxed);

        // Select gain based on mode
        let effective_target = match mode {
            1 => self.album_gain_db.load(Ordering::Relaxed), // Album mode
            _ => target,                                     // Track or Streaming mode
        };

        // Add preamp
        let effective_target = effective_target + preamp;

        // Smooth gain transition using exponential smoothing
        // FIX for Defect 27: Correct formula is coeff^frames, not (1-coeff)^frames
        // coeff^frames represents the proportion of gain difference remaining after N frames.
        // - When coeff ≈ 0.9999 (200ms smoothing): coeff^512 ≈ 0.95, gain moves 5% toward target
        // - When coeff = 0 (smoothing disabled): coeff^N = 0, gain jumps instantly to target
        // - When coeff = 1 (infinite smoothing): coeff^N = 1, gain never changes
        let remaining_factor = coeff.powi(frames as i32);
        let new_gain = current + (effective_target - current) * (1.0 - remaining_factor);

        self.current_gain_db.store(new_gain, Ordering::Relaxed);

        // Convert dB to linear
        db_to_linear(new_gain)
    }

    /// Get current loudness info (for API responses)
    pub fn get_info(&self) -> LoudnessInfo {
        LoudnessInfo {
            integrated_lufs: -70.0,
            short_term_lufs: -70.0,
            momentary_lufs: -70.0,
            loudness_range: 0.0,
            true_peak_dbtp: -70.0,
            current_gain_db: self.current_gain_db.load(Ordering::Relaxed),
            target_gain_db: self.target_gain_db.load(Ordering::Relaxed),
            preamp_db: self.preamp_gain_db.load(Ordering::Relaxed),
        }
    }
}

impl Default for AtomicLoudnessState {
    fn default() -> Self {
        Self::new(200.0, 44100)
    }
}
