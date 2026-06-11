//! High-level loudness normalizer wiring meter, limiter, and atomic state.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::config::{LoudnessConfig, NormalizationMode};

use super::atomic_state::AtomicLoudnessState;
use super::info::LoudnessInfo;
use super::limiter::PeakLimiter;
use super::meter::LoudnessMeter;

/// Loudness normalizer with EBU R128 compliance.
/// Supports track-based pre-analysis and real-time streaming modes.
pub struct LoudnessNormalizer {
    meter: LoudnessMeter,
    limiter: PeakLimiter,
    config: LoudnessConfig,
    atomic_state: Arc<AtomicLoudnessState>,

    // Track analysis results
    track_loudness: Option<f64>,
    track_gain: Option<f64>,

    channels: usize,
    sample_rate: u32,
}

impl LoudnessNormalizer {
    pub fn new(channels: usize, sample_rate: u32, config: LoudnessConfig) -> Self {
        let atomic_state = Arc::new(AtomicLoudnessState::new(
            config.smoothing_time_ms,
            sample_rate,
        ));

        Self {
            meter: LoudnessMeter::new(channels, sample_rate),
            limiter: PeakLimiter::new(
                channels,
                sample_rate,
                config.true_peak_limit_db,
                10.0,  // 10ms look-ahead
                100.0, // 100ms release
            ),
            config,
            atomic_state,
            track_loudness: None,
            track_gain: None,
            channels,
            sample_rate,
        }
    }

    pub fn atomic_state(&self) -> Arc<AtomicLoudnessState> {
        Arc::clone(&self.atomic_state)
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.atomic_state.set_enabled(enabled);
    }

    pub fn set_config(&mut self, config: LoudnessConfig) {
        self.config = config.clone();
        self.limiter.set_threshold_db(config.true_peak_limit_db);
        self.atomic_state
            .set_smoothing(config.smoothing_time_ms, self.sample_rate);

        if let Some(loudness) = self.track_loudness {
            let track_gain = self.config.target_lufs - loudness;
            self.track_gain = Some(track_gain);
            self.atomic_state.set_target_gain(track_gain);
        }
    }

    pub fn set_target_lufs(&mut self, target_lufs: f64) {
        self.config.target_lufs = target_lufs;
        if let Some(loudness) = self.track_loudness {
            let track_gain = target_lufs - loudness;
            self.track_gain = Some(track_gain);
            self.atomic_state.set_target_gain(track_gain);
        }
    }

    pub fn set_album_gain(&self, gain_db: f64) {
        self.atomic_state.set_album_gain(gain_db);
    }

    pub fn set_preamp_gain(&self, gain_db: f64) {
        self.atomic_state.set_preamp_gain(gain_db);
    }

    pub fn set_mode(&self, mode: NormalizationMode) {
        let mode_val = match mode {
            NormalizationMode::Track => 0,
            NormalizationMode::Album => 1,
            NormalizationMode::Streaming => 2,
            NormalizationMode::ReplayGainTrack => 3,
            NormalizationMode::ReplayGainAlbum => 4,
        };
        self.atomic_state.set_mode(mode_val);
    }

    /// Pre-analyze track loudness (call before streaming playback)
    ///
    /// FIX for Defect 39: Check loudness.is_finite() to prevent +inf gain
    /// when ebur128 returns -inf (silent or very short tracks <400ms).
    /// Invalid loudness values result in 0 dB gain (no normalization).
    pub fn analyze_track(&mut self, samples: &[f64]) -> f64 {
        self.meter.reset();
        self.meter.process(samples);
        let loudness = self.meter.integrated_loudness();

        // FIX for Defect 39: Validate loudness before computing gain
        if loudness.is_finite() {
            self.track_loudness = Some(loudness);
            let gain_db = self.config.target_lufs - loudness;
            self.track_gain = Some(gain_db);
            self.atomic_state.set_target_gain(gain_db);

            log::info!(
                "Track analysis: Integrated loudness = {:.2} LUFS, Target gain = {:.2} dB",
                loudness,
                gain_db
            );
        } else {
            // Invalid loudness (e.g., -inf for silent/very short tracks)
            // Keep 0 dB gain to avoid +inf/-inf multiplication in audio callback
            self.track_loudness = None;
            self.track_gain = Some(0.0);
            self.atomic_state.set_target_gain(0.0);

            log::warn!(
                "Track analysis: Invalid loudness ({:.2}), using 0 dB gain (no normalization)",
                loudness
            );
        }

        loudness
    }

    /// Calculate track gain without updating atomic state (for gapless preload)
    /// Returns the target gain in dB that should be applied after buffer swap.
    /// This prevents premature gain update during the last seconds of current track.
    ///
    /// FIX for Defect 39: Check loudness.is_finite() to prevent +inf gain
    /// when ebur128 returns -inf (silent or very short tracks <400ms).
    pub fn calculate_gain(&mut self, samples: &[f64]) -> f64 {
        self.meter.reset();
        self.meter.process(samples);
        let loudness = self.meter.integrated_loudness();

        // FIX for Defect 39: Validate loudness before computing gain
        if loudness.is_finite() {
            let gain_db = self.config.target_lufs - loudness;

            log::info!(
                "Gapless preload analysis: Integrated loudness = {:.2} LUFS, Pending gain = {:.2} dB",
                loudness, gain_db
            );

            gain_db
        } else {
            log::warn!(
                "Gapless preload analysis: Invalid loudness ({:.2}), using 0 dB gain",
                loudness
            );
            0.0
        }
    }

    /// Calculate gain for gapless preload with mode awareness (Bug-4 fix)
    ///
    /// For ReplayGain modes, reads gain from metadata tags instead of EBU R128 analysis.
    /// Falls back to EBU R128 if tags are missing.
    pub fn calculate_gain_with_mode(
        &mut self,
        samples: &[f64],
        mode: NormalizationMode,
        metadata: &crate::decoder::TrackMetadata,
    ) -> f64 {
        match mode {
            NormalizationMode::ReplayGainTrack => {
                // Use ReplayGain track gain from tag
                if let Some(rg_gain) = metadata.rg_track_gain {
                    // Convert ReplayGain tag gain to current target LUFS using configurable reference
                    let gain_db =
                        rg_gain + (self.config.target_lufs - self.config.replaygain_reference_lufs);
                    log::info!(
                        "Gapless preload: Using ReplayGain track tag: {:.2} dB -> target gain: {:.2} dB",
                        rg_gain, gain_db
                    );
                    return gain_db;
                }
                // Fallback to EBU R128 if no tag
                log::warn!("Gapless preload: No ReplayGain track tag, falling back to EBU R128");
                self.calculate_gain(samples)
            }
            NormalizationMode::ReplayGainAlbum => {
                // Use ReplayGain album gain (fallback to track)
                let rg_gain = metadata.rg_album_gain.or(metadata.rg_track_gain);
                if let Some(gain) = rg_gain {
                    let gain_db =
                        gain + (self.config.target_lufs - self.config.replaygain_reference_lufs);
                    log::info!(
                        "Gapless preload: Using ReplayGain album tag: {:.2} dB -> target gain: {:.2} dB",
                        gain, gain_db
                    );
                    return gain_db;
                }
                log::warn!(
                    "Gapless preload: No ReplayGain album/track tag, falling back to EBU R128"
                );
                self.calculate_gain(samples)
            }
            _ => {
                // Track/Album/Streaming modes: use EBU R128 analysis
                self.calculate_gain(samples)
            }
        }
    }

    pub fn reset(&mut self) {
        self.meter.reset();
        self.limiter.reset();
        self.atomic_state
            .target_gain_db
            .store(0.0, Ordering::Relaxed);
        self.atomic_state
            .current_gain_db
            .store(0.0, Ordering::Relaxed);
        self.track_loudness = None;
        self.track_gain = None;
    }

    /// Process interleaved f64 samples in-place
    pub fn process(&mut self, samples: &mut [f64]) {
        if !self.atomic_state.enabled.load(Ordering::Relaxed) {
            return;
        }

        let frames = samples.len() / self.channels;
        if frames == 0 {
            return;
        }

        // For streaming mode, measure in real-time
        if self.config.mode == NormalizationMode::Streaming {
            self.meter.process(samples);

            if self.meter.has_reliable_measurement() {
                let current_loudness = self.meter.short_term_loudness();
                if current_loudness > -70.0 {
                    let target_gain = self.config.target_lufs - current_loudness;
                    self.atomic_state
                        .set_target_gain(target_gain.clamp(-20.0, 20.0));
                }
            }
        }

        // Apply gain using atomic state
        let linear_gain = self.atomic_state.process_gain(frames);
        for sample in samples.iter_mut() {
            *sample *= linear_gain;
        }

        // Apply peak limiting
        self.limiter.process(samples);
    }

    pub fn get_loudness_info(&self) -> LoudnessInfo {
        LoudnessInfo {
            integrated_lufs: self.meter.integrated_loudness(),
            short_term_lufs: self.meter.short_term_loudness(),
            momentary_lufs: self.meter.momentary_loudness(),
            loudness_range: self.meter.loudness_range(),
            true_peak_dbtp: self.meter.true_peak(),
            current_gain_db: self.atomic_state.current_gain_db.load(Ordering::Relaxed),
            target_gain_db: self.atomic_state.target_gain_db.load(Ordering::Relaxed),
            preamp_db: self.atomic_state.preamp_gain_db.load(Ordering::Relaxed),
        }
    }

    pub fn track_loudness(&self) -> Option<f64> {
        self.track_loudness
    }
    pub fn is_analyzed(&self) -> bool {
        self.track_loudness.is_some()
    }
}
