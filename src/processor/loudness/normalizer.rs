//! High-level loudness normalizer wiring meter, limiter, and atomic state.

use std::sync::Arc;

use crate::analysis::LoudnessMeter;
use crate::audio_block::{validated_channel_count, AudioBlockMut, AudioBlockRef};
use crate::config::{LoudnessConfig, NormalizationMode};
use crate::processor::lockfree_params::{LIMITER_THRESHOLD_DB_MAX, LIMITER_THRESHOLD_DB_MIN};
use crate::processor::loudness::LoudnessInfo;
use crate::processor::traits::{
    validate_processor_channels, validate_sample_rate_hz, ProcessError,
};

use super::atomic_state::AtomicLoudnessState;
use super::limiter::PeakLimiter;

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

    /// Whether the previous `process_validated` call saw the stage enabled.
    ///
    /// The enabled flag lives in the shared [`AtomicLoudnessState`], which
    /// callers can hold and flip directly, so the false-to-true edge is detected
    /// here on the processing path rather than in `set_enabled`. That is the only
    /// place every route into the stage passes through.
    was_enabled: bool,

    channels: usize,
    sample_rate: u32,
}

impl LoudnessNormalizer {
    /// Create a normalizer for the given geometry and prevalidated config.
    ///
    /// Rejects zero channels/rate, invalid config fields, and EBU R128 backend
    /// failure atomically before any state exists.
    pub fn new(
        channels: usize,
        sample_rate: u32,
        config: LoudnessConfig,
    ) -> Result<Self, ProcessError> {
        validated_channel_count(channels)?;
        validate_sample_rate_hz("LoudnessNormalizer", sample_rate)?;
        validate_config(&config)?;
        let meter = LoudnessMeter::new(channels, sample_rate)?;
        let atomic_state = Arc::new(AtomicLoudnessState::new(
            config.smoothing_time_ms,
            sample_rate,
        )?);
        atomic_state.set_enabled(config.enabled);
        atomic_state.set_normalization_mode(config.mode);

        Ok(Self {
            meter,
            limiter: PeakLimiter::new_validated(
                channels,
                sample_rate,
                config.true_peak_limit_db,
                10.0,  // 10ms look-ahead
                100.0, // 100ms release
            ),
            was_enabled: config.enabled,
            config,
            atomic_state,
            track_loudness: None,
            track_gain: None,
            channels,
            sample_rate,
        })
    }

    /// Share the normalized control state with a caller-owned handle.
    pub fn atomic_state(&self) -> Arc<AtomicLoudnessState> {
        Arc::clone(&self.atomic_state)
    }

    /// Enable or bypass normalization on the next block.
    ///
    /// # Latency
    ///
    /// An enabled normalizer runs an internal look-ahead limiter and therefore
    /// delays the signal by its look-ahead (10 ms plus the true-peak
    /// reconstruction span; [`PeakLimiter::delay_frames`] is the exact figure).
    /// A bypassed one passes samples straight through with no delay at all.
    ///
    /// Toggling this mid-stream therefore changes the latency of the stage, which
    /// is a timing discontinuity as well as a level one: enabling swallows one
    /// delay's worth of audio while the line refills, and disabling drops
    /// whatever was still inside it. Set it once for a playback session rather
    /// than automating it, or accept the seam.
    ///
    /// # Bypass is a hard switch
    ///
    /// Disabling takes effect on the very next block. The alternative -- draining
    /// the limiter for one delay before going quiet -- was considered and
    /// rejected: it would keep a stage that the caller has switched off inside
    /// the graph, still consuming CPU and still emitting audio, which is a
    /// scheduling decision rather than a smoothing one, and it would make a
    /// bypassed normalizer non-transparent for 10 ms after every toggle. The
    /// cost of the hard switch is the discarded delay-line contents described
    /// above.
    ///
    /// Re-enabling resets the limiter, so no pre-bypass audio can survive the gap
    /// and replay.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
        self.atomic_state.set_enabled(enabled);
    }

    /// Replace the config after validating every field.
    ///
    /// On rejection the previous config, limiter, meter, and gain state are
    /// left unchanged.
    pub fn set_config(&mut self, config: LoudnessConfig) -> Result<(), ProcessError> {
        validate_config(&config)?;
        if let Some(loudness) = self.track_loudness {
            checked_gain(config.target_lufs - loudness, "target LUFS")?;
        }

        self.limiter.set_threshold_db(config.true_peak_limit_db);
        self.atomic_state
            .set_smoothing(config.smoothing_time_ms, self.sample_rate);
        self.atomic_state.set_enabled(config.enabled);
        self.atomic_state.set_normalization_mode(config.mode);
        self.config = config;

        if let Some(loudness) = self.track_loudness {
            let track_gain = self.config.target_lufs - loudness;
            self.track_gain = Some(track_gain);
            self.atomic_state.set_target_gain(track_gain);
        }
        Ok(())
    }

    /// Change the target loudness; rejects non-finite or out-of-domain values.
    pub fn set_target_lufs(&mut self, target_lufs: f64) -> Result<(), ProcessError> {
        validate_finite("target LUFS", target_lufs)?;
        if let Some(loudness) = self.track_loudness {
            checked_gain(target_lufs - loudness, "target LUFS")?;
        }
        self.config.target_lufs = target_lufs;
        if let Some(loudness) = self.track_loudness {
            let track_gain = target_lufs - loudness;
            self.track_gain = Some(track_gain);
            self.atomic_state.set_target_gain(track_gain);
        }
        Ok(())
    }

    /// Override the album gain offset; rejects non-finite values.
    pub fn set_album_gain(&self, gain_db: f64) -> Result<(), ProcessError> {
        validate_finite("album gain", gain_db)?;
        self.atomic_state.set_album_gain(gain_db);
        Ok(())
    }

    /// Override the preamp gain offset; rejects non-finite values.
    pub fn set_preamp_gain(&self, gain_db: f64) -> Result<(), ProcessError> {
        validate_finite("preamp gain", gain_db)?;
        self.atomic_state.set_preamp_gain(gain_db);
        Ok(())
    }

    /// Switch the normalization mode (track/album/streaming/ReplayGain).
    pub fn set_mode(&mut self, mode: NormalizationMode) {
        self.config.mode = mode;
        self.atomic_state.set_normalization_mode(mode);
    }

    /// Pre-analyze track loudness (call before streaming playback)
    ///
    /// FIX for Defect 39: Check loudness.is_finite() to prevent +inf gain
    /// when ebur128 returns -inf (silent or very short tracks <400ms).
    /// Invalid loudness values result in 0 dB gain (no normalization).
    pub fn analyze_track(&mut self, samples: &[f64]) -> Result<f64, ProcessError> {
        AudioBlockRef::new(samples, self.channels)?;
        self.meter.reset();
        self.meter.process(samples)?;
        let loudness = self.meter.integrated_loudness();

        // FIX for Defect 39: Validate loudness before computing gain
        if loudness.is_finite() {
            let gain_db = checked_gain(self.config.target_lufs - loudness, "target LUFS")?;
            self.track_loudness = Some(loudness);
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

        Ok(loudness)
    }

    /// Calculate track gain without updating atomic state (for gapless preload)
    /// Returns the target gain in dB that should be applied after buffer swap.
    /// This prevents premature gain update during the last seconds of current track.
    ///
    /// FIX for Defect 39: Check loudness.is_finite() to prevent +inf gain
    /// when ebur128 returns -inf (silent or very short tracks <400ms).
    pub fn calculate_gain(&mut self, samples: &[f64]) -> Result<f64, ProcessError> {
        AudioBlockRef::new(samples, self.channels)?;
        self.meter.reset();
        self.meter.process(samples)?;
        let loudness = self.meter.integrated_loudness();

        // FIX for Defect 39: Validate loudness before computing gain
        if loudness.is_finite() {
            let gain_db = self.config.target_lufs - loudness;

            log::info!(
                "Gapless preload analysis: Integrated loudness = {:.2} LUFS, Pending gain = {:.2} dB",
                loudness, gain_db
            );

            checked_gain(gain_db, "target LUFS")
        } else {
            log::warn!(
                "Gapless preload analysis: Invalid loudness ({:.2}), using 0 dB gain",
                loudness
            );
            Ok(0.0)
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
    ) -> Result<f64, ProcessError> {
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
                    return checked_gain(gain_db, "ReplayGain track gain");
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
                    return checked_gain(gain_db, "ReplayGain album gain");
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

    /// Reset meter, limiter, gain state, and cached track analysis.
    pub fn reset(&mut self) {
        self.meter.reset();
        self.limiter.reset();
        self.atomic_state.reset_gain();
        self.track_loudness = None;
        self.track_gain = None;
    }

    /// Process interleaved f64 samples in-place
    pub fn process(&mut self, samples: &mut [f64], channels: usize) -> Result<(), ProcessError> {
        let block = AudioBlockMut::new(samples, channels)?;
        validate_processor_channels("LoudnessNormalizer", Some(self.channels), channels)?;
        self.process_validated(block.into_samples())
    }

    fn process_validated(&mut self, samples: &mut [f64]) -> Result<(), ProcessError> {
        if !self.atomic_state.enabled() {
            self.was_enabled = false;
            return Ok(());
        }

        // Re-arming after a bypass. The limiter kept running its delay line right
        // up to the moment the stage went idle, so it still holds the last ~10 ms
        // of pre-bypass audio; without this the first block back would play that
        // out, ahead of the audio actually being handed in. Zeroing in place, so
        // this stays allocation-free on the audio thread.
        if !self.was_enabled {
            self.limiter.reset();
            self.was_enabled = true;
        }

        let frames = samples.len() / self.channels;
        if frames == 0 {
            return Ok(());
        }

        // For streaming mode, measure in real-time
        if self.config.mode == NormalizationMode::Streaming {
            self.meter.process(samples)?;

            if self.meter.has_reliable_measurement() {
                let current_loudness = self.meter.short_term_loudness();
                if current_loudness > -70.0 {
                    let target_gain = self.config.target_lufs - current_loudness;
                    self.atomic_state
                        .set_target_gain(target_gain.clamp(-20.0, 20.0));
                }
            }
        }

        // Apply gain using atomic state, interpolated across the block.
        //
        // The smoother advances once per block, so applying its endpoint to every
        // sample turns a smooth trajectory into a staircase with one tread per
        // block: a 20 dB track change at 512-frame blocks lands as ~1.04 dB
        // jumps, which is audible zipper. Spreading the same movement linearly
        // over the block keeps the endpoint -- and therefore the long-term
        // trajectory -- identical while cutting the per-sample step by roughly
        // the block length.
        let (start_gain, end_gain) = self.atomic_state.process_gain_ramp(frames);
        if start_gain == end_gain {
            for sample in samples.iter_mut() {
                *sample *= end_gain;
            }
        } else {
            // Divided by `frames`, not `frames - 1`: the last sample lands one
            // step short of `end_gain`, which is exactly where the next block
            // starts, so the rate stays uniform across the boundary.
            let step = (end_gain - start_gain) / frames as f64;
            for frame in 0..frames {
                let gain = start_gain + step * frame as f64;
                let base = frame * self.channels;
                for sample in &mut samples[base..base + self.channels] {
                    *sample *= gain;
                }
            }
        }

        // Apply peak limiting
        self.limiter.process_validated(samples);
        Ok(())
    }

    /// Current measurement/gain readout for UI or status reporting.
    pub fn get_loudness_info(&self) -> LoudnessInfo {
        LoudnessInfo {
            integrated_lufs: self.meter.integrated_loudness(),
            short_term_lufs: self.meter.short_term_loudness(),
            momentary_lufs: self.meter.momentary_loudness(),
            loudness_range: self.meter.loudness_range(),
            true_peak_dbtp: self.meter.true_peak(),
            current_gain_db: self.atomic_state.current_gain_db(),
            target_gain_db: self.atomic_state.target_gain_db(),
            preamp_db: self.atomic_state.preamp_gain_db(),
        }
    }

    /// Analyzed track loudness in LUFS, when pre-analysis ran.
    pub fn track_loudness(&self) -> Option<f64> {
        self.track_loudness
    }
    /// Whether track-level analysis has provided a loudness measurement.
    pub fn is_analyzed(&self) -> bool {
        self.track_loudness.is_some()
    }
}

fn validate_config(config: &LoudnessConfig) -> Result<(), ProcessError> {
    validate_finite("target LUFS", config.target_lufs)?;
    validate_finite("true-peak limit", config.true_peak_limit_db)?;
    if !(LIMITER_THRESHOLD_DB_MIN..=LIMITER_THRESHOLD_DB_MAX).contains(&config.true_peak_limit_db) {
        return Err(ProcessError::InvalidParameter {
            processor: "LoudnessNormalizer",
            parameter: "true-peak limit",
            message: "value must be inside the published limiter threshold range",
        });
    }
    if !config.smoothing_time_ms.is_finite() || config.smoothing_time_ms < 0.0 {
        return Err(ProcessError::InvalidParameter {
            processor: "LoudnessNormalizer",
            parameter: "smoothing time",
            message: "value must be finite and non-negative",
        });
    }
    validate_finite(
        "ReplayGain reference LUFS",
        config.replaygain_reference_lufs,
    )
}

fn validate_finite(parameter: &'static str, value: f64) -> Result<(), ProcessError> {
    if value.is_finite() {
        return Ok(());
    }
    Err(ProcessError::InvalidParameter {
        processor: "LoudnessNormalizer",
        parameter,
        message: "value must be finite",
    })
}

fn checked_gain(value: f64, parameter: &'static str) -> Result<f64, ProcessError> {
    validate_finite(parameter, value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_block::AudioBlockError;
    use crate::dsp::linear_to_db;

    fn loudness_info_bits(info: &LoudnessInfo) -> [u64; 8] {
        [
            info.integrated_lufs.to_bits(),
            info.short_term_lufs.to_bits(),
            info.momentary_lufs.to_bits(),
            info.loudness_range.to_bits(),
            info.true_peak_dbtp.to_bits(),
            info.current_gain_db.to_bits(),
            info.target_gain_db.to_bits(),
            info.preamp_db.to_bits(),
        ]
    }

    const MODES: [NormalizationMode; 5] = [
        NormalizationMode::Track,
        NormalizationMode::Album,
        NormalizationMode::Streaming,
        NormalizationMode::ReplayGainTrack,
        NormalizationMode::ReplayGainAlbum,
    ];

    #[test]
    fn constructor_publishes_disabled_album_config_and_bypasses() {
        let config = LoudnessConfig {
            enabled: false,
            mode: NormalizationMode::Album,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();
        let state = normalizer.atomic_state();

        assert!(!state.enabled());
        assert_eq!(state.get_mode(), NormalizationMode::Album);

        let mut samples = vec![0.25; 128 * 2];
        let expected = samples.clone();
        normalizer.process_validated(&mut samples).unwrap();
        assert_eq!(samples, expected);
    }

    #[test]
    fn config_and_explicit_setters_round_trip_all_modes() {
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, LoudnessConfig::default()).unwrap();

        for (index, mode) in MODES.into_iter().enumerate() {
            let enabled = index % 2 == 0;
            let config = LoudnessConfig {
                enabled,
                mode,
                ..LoudnessConfig::default()
            };
            normalizer.set_config(config).unwrap();
            assert_eq!(normalizer.atomic_state.enabled(), enabled);
            assert_eq!(normalizer.atomic_state.get_mode(), mode);
            assert_eq!(normalizer.config.enabled, enabled);
            assert_eq!(normalizer.config.mode, mode);
        }

        normalizer.set_enabled(false);
        normalizer.set_mode(NormalizationMode::ReplayGainAlbum);
        assert!(!normalizer.config.enabled);
        assert_eq!(normalizer.config.mode, NormalizationMode::ReplayGainAlbum);
        assert!(!normalizer.atomic_state.enabled());
        assert_eq!(
            normalizer.atomic_state.get_mode(),
            NormalizationMode::ReplayGainAlbum
        );
    }

    #[test]
    fn invalid_config_and_setters_reject_before_mutation() {
        for config in [
            LoudnessConfig {
                target_lufs: f64::NAN,
                ..LoudnessConfig::default()
            },
            LoudnessConfig {
                true_peak_limit_db: LIMITER_THRESHOLD_DB_MIN - 0.1,
                ..LoudnessConfig::default()
            },
            LoudnessConfig {
                smoothing_time_ms: -1.0,
                ..LoudnessConfig::default()
            },
            LoudnessConfig {
                replaygain_reference_lufs: f64::INFINITY,
                ..LoudnessConfig::default()
            },
        ] {
            assert!(matches!(
                LoudnessNormalizer::new(2, 48_000, config),
                Err(ProcessError::InvalidParameter { .. })
            ));
        }

        let mut normalizer = LoudnessNormalizer::new(2, 48_000, LoudnessConfig::default()).unwrap();
        let mut reference = LoudnessNormalizer::new(2, 48_000, LoudnessConfig::default()).unwrap();
        normalizer.set_album_gain(-2.0).unwrap();
        reference.set_album_gain(-2.0).unwrap();
        normalizer.set_preamp_gain(-1.5).unwrap();
        reference.set_preamp_gain(-1.5).unwrap();

        let before_config = normalizer.config.clone();
        let state = normalizer.atomic_state();
        let before_state = [
            state.target_gain_db().to_bits(),
            state.current_gain_db().to_bits(),
            state.smoothing_coefficient().to_bits(),
            state.album_gain_db().to_bits(),
            state.preamp_gain_db().to_bits(),
        ];
        let invalid = LoudnessConfig {
            target_lufs: -30.0,
            true_peak_limit_db: -10.0,
            smoothing_time_ms: f64::NAN,
            mode: NormalizationMode::Album,
            enabled: false,
            replaygain_reference_lufs: -23.0,
        };

        assert!(matches!(
            normalizer.set_config(invalid),
            Err(ProcessError::InvalidParameter {
                parameter: "smoothing time",
                ..
            })
        ));
        assert_eq!(
            [
                state.target_gain_db().to_bits(),
                state.current_gain_db().to_bits(),
                state.smoothing_coefficient().to_bits(),
                state.album_gain_db().to_bits(),
                state.preamp_gain_db().to_bits(),
            ],
            before_state
        );
        assert_eq!(
            normalizer.config.target_lufs.to_bits(),
            before_config.target_lufs.to_bits()
        );
        assert_eq!(
            normalizer.config.true_peak_limit_db.to_bits(),
            before_config.true_peak_limit_db.to_bits()
        );
        assert_eq!(normalizer.config.mode, before_config.mode);
        assert_eq!(normalizer.config.enabled, before_config.enabled);

        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(normalizer.set_target_lufs(value).is_err());
            assert!(normalizer.set_album_gain(value).is_err());
            assert!(normalizer.set_preamp_gain(value).is_err());
        }
        assert_eq!(
            normalizer.config.target_lufs.to_bits(),
            before_config.target_lufs.to_bits()
        );
        assert_eq!(state.album_gain_db().to_bits(), (-2.0_f64).to_bits());
        assert_eq!(state.preamp_gain_db().to_bits(), (-1.5_f64).to_bits());

        let mut samples = vec![1.0; 2_048 * 2];
        let mut reference_samples = samples.clone();
        normalizer.process(&mut samples, 2).unwrap();
        reference.process(&mut reference_samples, 2).unwrap();
        assert_eq!(samples, reference_samples);
    }

    #[test]
    fn zero_smoothing_is_valid_and_streaming_process_stays_no_alloc() {
        let config = LoudnessConfig {
            smoothing_time_ms: 0.0,
            mode: NormalizationMode::Streaming,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();
        assert_eq!(normalizer.atomic_state.smoothing_coefficient(), 0.0);
        let mut samples = [0.125; 64 * 2];

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..1_000 {
                assert_eq!(normalizer.process(&mut samples, 2), Ok(()));
            }
        });
    }

    #[test]
    fn raw_normalizer_rejects_invalid_setup_and_block_geometry_atomically() {
        assert!(matches!(
            LoudnessNormalizer::new(0, 48_000, LoudnessConfig::default()),
            Err(ProcessError::InvalidBlock(AudioBlockError::ZeroChannels))
        ));
        assert!(matches!(
            LoudnessNormalizer::new(2, 0, LoudnessConfig::default()),
            Err(ProcessError::InvalidSampleRate {
                processor: "LoudnessNormalizer",
                sample_rate_hz: 0,
            })
        ));

        let config = LoudnessConfig {
            mode: NormalizationMode::Streaming,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config.clone()).unwrap();
        let mut reference = LoudnessNormalizer::new(2, 48_000, config).unwrap();
        let mut warm = [0.25; 128];
        let mut reference_warm = warm;
        normalizer.process(&mut warm, 2).unwrap();
        reference.process(&mut reference_warm, 2).unwrap();
        assert_eq!(warm, reference_warm);
        let state = loudness_info_bits(&normalizer.get_loudness_info());

        let mut zero_channels = [0.25; 4];
        let zero_channels_before = zero_channels;
        let mut incomplete = [0.25; 3];
        let incomplete_before = incomplete;
        let mut mismatch = [0.25; 4];
        let mismatch_before = mismatch;
        assert_no_alloc::assert_no_alloc(|| {
            assert_eq!(
                normalizer.process(&mut zero_channels, 0),
                Err(ProcessError::InvalidBlock(AudioBlockError::ZeroChannels))
            );
            assert_eq!(
                normalizer.process(&mut incomplete, 2),
                Err(ProcessError::InvalidBlock(
                    AudioBlockError::IncompleteFrame {
                        samples: 3,
                        channels: 2,
                    }
                ))
            );
            assert_eq!(
                normalizer.process(&mut mismatch, 1),
                Err(ProcessError::ChannelCountMismatch {
                    processor: "LoudnessNormalizer",
                    expected_channels: 2,
                    actual_channels: 1,
                })
            );
        });

        assert_eq!(zero_channels, zero_channels_before);
        assert_eq!(incomplete, incomplete_before);
        assert_eq!(mismatch, mismatch_before);
        assert_eq!(loudness_info_bits(&normalizer.get_loudness_info()), state);

        let mut next = [0.125; 128];
        let mut reference_next = next;
        normalizer.process(&mut next, 2).unwrap();
        reference.process(&mut reference_next, 2).unwrap();
        assert_eq!(next, reference_next);
        assert_eq!(
            loudness_info_bits(&normalizer.get_loudness_info()),
            loudness_info_bits(&reference.get_loudness_info())
        );
    }

    // ========================================================================
    // D4: block-interior gain interpolation (2026-08-11 review)
    // ========================================================================

    /// Blocks the callback actually hands us, and the size the review measured
    /// the 1.04 dB staircase tread at.
    const D4_BLOCK: usize = 512;

    /// Largest single-sample gain step, in dB, the normalizer may take.
    const D4_MAX_STEP_DB: f64 = 0.5;

    /// Recover the per-sample applied gain from a DC probe.
    ///
    /// The internal limiter runs after the gain, so the probe level is chosen to
    /// keep its output far below the threshold: it then passes the signal through
    /// at unity and contributes only its delay, which the caller skips.
    fn normalizer_gain_trajectory(
        normalizer: &mut LoudnessNormalizer,
        level: f64,
        blocks: usize,
    ) -> Vec<f64> {
        let mut trajectory = Vec::with_capacity(blocks * D4_BLOCK);
        for _ in 0..blocks {
            let mut buffer = vec![level; D4_BLOCK * 2];
            normalizer.process_validated(&mut buffer).unwrap();
            trajectory.extend(buffer.iter().step_by(2).map(|out| out / level));
        }
        trajectory
    }

    /// D4: the smoother advances once per block, so applying its endpoint to the
    /// whole block turned a smooth 20 dB track change into ~1.04 dB steps at each
    /// block boundary. Interpolating across the block removes the staircase
    /// without moving the endpoint.
    #[test]
    fn track_change_gain_ramp_has_no_block_boundary_zipper() {
        let config = LoudnessConfig {
            enabled: true,
            mode: NormalizationMode::Track,
            target_lufs: -12.0,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();

        // A 20 dB track change, the scenario the requirement names.
        normalizer.atomic_state.set_target_gain(20.0);

        // 10 ms lookahead + true-peak span; the limiter emits zeros until its
        // delay line has filled, which is not gain and must not be measured.
        let skip = 493;
        let level = 0.01;
        let trajectory = normalizer_gain_trajectory(&mut normalizer, level, 8);
        let measured = &trajectory[skip..];

        let worst = measured
            .windows(2)
            .map(|pair| (linear_to_db(pair[1]) - linear_to_db(pair[0])).abs())
            .fold(0.0_f64, f64::max);

        assert!(
            worst <= D4_MAX_STEP_DB,
            "gain stepped {worst:.4} dB between adjacent samples (limit {D4_MAX_STEP_DB} dB); \
             a block-constant gain puts one ~1.04 dB tread at every {D4_BLOCK}-frame boundary"
        );

        // The trajectory must actually be moving -- a flat readout would satisfy
        // the step bound trivially.
        let travelled = linear_to_db(*measured.last().unwrap()) - linear_to_db(measured[0]);
        assert!(
            travelled > 1.0,
            "gain only travelled {travelled:.4} dB, so the step bound proves nothing"
        );
    }

    /// Interpolation must not change *where* the smoother gets to, only how it
    /// gets there: the endpoint after N blocks has to match the block-constant
    /// implementation bit for bit, because that is the trajectory the 200 ms
    /// smoothing time is specified against.
    #[test]
    fn block_interior_interpolation_leaves_the_smoother_endpoint_untouched() {
        let make = || {
            let config = LoudnessConfig {
                enabled: true,
                mode: NormalizationMode::Track,
                ..LoudnessConfig::default()
            };
            let normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();
            normalizer.atomic_state.set_target_gain(20.0);
            normalizer
        };

        // The public `process_gain` is the block-constant reference; it and the
        // ramp variant share one advance, so both must land identically.
        let reference = make();
        for _ in 0..8 {
            let _ = reference.atomic_state.process_gain(D4_BLOCK);
        }

        let mut ramped = make();
        let _ = normalizer_gain_trajectory(&mut ramped, 0.01, 8);

        assert_eq!(
            ramped.atomic_state.current_gain_db().to_bits(),
            reference.atomic_state.current_gain_db().to_bits(),
            "interpolating inside the block moved the endpoint the smoother reaches"
        );
    }

    /// A settled gain has `start == end`, which takes the flat path. That path
    /// must stay bit-exact with the block-constant multiply, so steady-state
    /// playback is unchanged by this feature.
    #[test]
    fn settled_gain_applies_the_flat_path_bit_exactly() {
        let config = LoudnessConfig {
            enabled: true,
            mode: NormalizationMode::Track,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();
        // The default preamp is -1 dB, so a fresh state has somewhere to travel.
        // Zero both to get a genuinely settled smoother.
        normalizer.atomic_state.set_preamp_gain(0.0);
        normalizer.atomic_state.set_target_gain(0.0);

        let (start, end) = normalizer.atomic_state.process_gain_ramp(D4_BLOCK);
        assert_eq!(
            start.to_bits(),
            end.to_bits(),
            "a settled smoother must report a flat block"
        );
        assert_eq!(end, 1.0, "0 dB target with 0 dB preamp must be unity gain");

        let input: Vec<f64> = (0..D4_BLOCK * 2)
            .map(|i| ((i as f64) * 0.01).sin() * 0.1)
            .collect();
        let mut buffer = input.clone();
        normalizer.process_validated(&mut buffer).unwrap();

        // Unity gain, and the probe sits far below the limiter threshold, so the
        // only thing between input and output is the limiter's delay: whatever
        // has emerged must be the input verbatim.
        let skip = 493;
        assert_eq!(
            &buffer[skip * 2..],
            &input[..input.len() - skip * 2],
            "the flat path is not bit-exact at unity gain"
        );
    }

    /// The interpolation runs on the audio thread, so it must not allocate.
    #[test]
    fn gain_interpolation_is_allocation_free() {
        let config = LoudnessConfig {
            enabled: true,
            mode: NormalizationMode::Track,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();
        normalizer.atomic_state.set_target_gain(20.0);
        let mut buffer = vec![0.01; D4_BLOCK * 2];
        normalizer.process_validated(&mut buffer).unwrap();

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..16 {
                normalizer.process_validated(&mut buffer).unwrap();
            }
        });
    }

    /// D5: the limiter's delay line kept running right up to the bypass, so
    /// re-enabling used to play out ~10 ms of pre-bypass audio ahead of the block
    /// actually being handed in.
    #[test]
    fn re_enabling_does_not_replay_pre_bypass_audio() {
        let config = LoudnessConfig {
            enabled: true,
            mode: NormalizationMode::Track,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();
        normalizer.atomic_state.set_preamp_gain(0.0);
        normalizer.atomic_state.set_target_gain(0.0);

        // A loud, unmistakable marker fills the limiter's delay line.
        let marker = 0.5;
        let mut loud = vec![marker; 512 * 2];
        normalizer.process_validated(&mut loud).unwrap();

        // Bypass, then hand it silence while disabled (which it passes through).
        normalizer.set_enabled(false);
        let mut bypassed = vec![0.0; 512 * 2];
        normalizer.process_validated(&mut bypassed).unwrap();
        assert!(
            bypassed.iter().all(|sample| *sample == 0.0),
            "a bypassed normalizer must be transparent"
        );

        // Re-arm and hand it silence. Anything non-zero coming out is audio from
        // before the gap.
        normalizer.set_enabled(true);
        let mut after = vec![0.0; 512 * 2];
        normalizer.process_validated(&mut after).unwrap();

        let leaked = after.iter().fold(0.0_f64, |acc, s| acc.max(s.abs()));
        assert_eq!(
            leaked, 0.0,
            "re-enabling replayed pre-bypass audio at {leaked} (marker was {marker}); \
             the limiter's delay line survived the gap"
        );
    }

    /// The stale-audio guard has to hold no matter which route flipped the flag.
    /// The enabled bit lives in the shared state, so a caller holding the `Arc`
    /// can bypass `set_enabled` entirely -- which is why the edge is detected on
    /// the processing path.
    #[test]
    fn re_arm_guard_covers_the_shared_atomic_state_route() {
        let config = LoudnessConfig {
            enabled: true,
            mode: NormalizationMode::Track,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();
        normalizer.atomic_state.set_preamp_gain(0.0);
        normalizer.atomic_state.set_target_gain(0.0);
        let shared = normalizer.atomic_state();

        let mut loud = vec![0.5; 512 * 2];
        normalizer.process_validated(&mut loud).unwrap();

        // Flip the flag through the shared handle, never touching `set_enabled`.
        shared.set_enabled(false);
        let mut bypassed = vec![0.0; 512 * 2];
        normalizer.process_validated(&mut bypassed).unwrap();
        shared.set_enabled(true);

        let mut after = vec![0.0; 512 * 2];
        normalizer.process_validated(&mut after).unwrap();

        assert!(
            after.iter().all(|sample| *sample == 0.0),
            "the shared-state bypass route let pre-bypass audio survive"
        );
    }

    /// Re-arming must not throw away the gain trajectory: the track has not
    /// changed, so the smoothed gain carries across the gap. Only the limiter's
    /// buffered audio is discarded.
    #[test]
    fn re_arming_preserves_the_smoothed_gain() {
        let config = LoudnessConfig {
            enabled: true,
            mode: NormalizationMode::Track,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();
        normalizer.atomic_state.set_target_gain(20.0);

        let mut buffer = vec![0.01; 512 * 2];
        for _ in 0..4 {
            normalizer.process_validated(&mut buffer).unwrap();
        }
        let before = normalizer.atomic_state.current_gain_db();
        assert!(before > 1.0, "gain never started moving");

        normalizer.set_enabled(false);
        normalizer.process_validated(&mut buffer).unwrap();
        normalizer.set_enabled(true);

        assert_eq!(
            normalizer.atomic_state.current_gain_db().to_bits(),
            before.to_bits(),
            "bypassing discarded the smoothed gain; re-arming would jump the level"
        );
    }

    /// The re-arm path runs on the audio thread.
    #[test]
    fn re_arm_reset_is_allocation_free() {
        let config = LoudnessConfig {
            enabled: true,
            mode: NormalizationMode::Track,
            ..LoudnessConfig::default()
        };
        let mut normalizer = LoudnessNormalizer::new(2, 48_000, config).unwrap();
        let mut buffer = vec![0.05; 512 * 2];
        normalizer.process_validated(&mut buffer).unwrap();
        let shared = normalizer.atomic_state();

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..8 {
                shared.set_enabled(false);
                normalizer.process_validated(&mut buffer).unwrap();
                shared.set_enabled(true);
                normalizer.process_validated(&mut buffer).unwrap();
            }
        });
    }
}
