//! Lock-free Parameter Structures
//!
//! Provides snapshot-based parameter passing from main thread to audio thread.
//! This eliminates the need for mutexes in the audio callback, ensuring
//! that DSP processing is never blocked or skipped due to lock contention.
//!
//! # Design Pattern
//!
//! Processor parameters are published as immutable snapshots through `ArcSwap`.
//! Setters patch snapshots with `ArcSwap::rcu`, so concurrent UI/control writes
//! retry instead of silently overwriting each other's fields. The audio thread
//! observes either the old or the new complete snapshot, never a mix of fields
//! from both.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use arc_swap::{ArcSwap, Guard};
use atomic_float::AtomicF64;

struct SharedParams<T> {
    current: ArcSwap<T>,
    generation: AtomicU64,
}

impl<T: Default> SharedParams<T> {
    fn new() -> Self {
        Self::from_snapshot(T::default())
    }
}

impl<T> SharedParams<T> {
    fn from_snapshot(snapshot: T) -> Self {
        Self {
            current: ArcSwap::new(Arc::new(snapshot)),
            generation: AtomicU64::new(0),
        }
    }

    #[inline]
    fn load(&self) -> Arc<T> {
        self.current.load_full()
    }

    #[inline]
    fn load_with_generation(&self) -> (Arc<T>, u64) {
        loop {
            let before = self.generation.load(Ordering::Acquire);
            let current = self.current.load_full();
            let after = self.generation.load(Ordering::Acquire);
            if before == after {
                return (current, after);
            }
        }
    }

    #[inline]
    fn load_if_changed(&self, cached: &Arc<T>) -> Option<Arc<T>> {
        let current = self.current.load();
        if std::ptr::eq(&**current, Arc::as_ref(cached)) {
            None
        } else {
            Some(Guard::into_inner(current))
        }
    }

    #[inline]
    fn load_if_changed_since(&self, cached_generation: u64) -> Option<(Arc<T>, u64)> {
        let generation = self.generation.load(Ordering::Acquire);
        if generation == cached_generation {
            None
        } else {
            Some((self.current.load_full(), generation))
        }
    }

    #[inline]
    fn publish(&self, snapshot: T) {
        self.current.store(Arc::new(snapshot));
        self.generation.fetch_add(1, Ordering::Release);
    }
}

impl<T: Clone> SharedParams<T> {
    #[inline]
    fn read(&self) -> T {
        (*self.current.load_full()).clone()
    }

    #[inline]
    fn update(&self, mut f: impl FnMut(&mut T)) {
        self.current.rcu(|current| {
            let mut snapshot = T::clone(current);
            f(&mut snapshot);
            snapshot
        });
        self.generation.fetch_add(1, Ordering::Release);
    }
}

macro_rules! impl_default_via_new {
    ($type:ty) => {
        impl Default for $type {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

macro_rules! impl_snapshot_accessors {
    ($snapshot:ty) => {
        #[inline]
        pub fn load(&self) -> Arc<$snapshot> {
            self.shared.load()
        }

        #[inline]
        pub fn load_with_generation(&self) -> (Arc<$snapshot>, u64) {
            self.shared.load_with_generation()
        }

        #[inline]
        pub fn load_if_changed(&self, cached: &Arc<$snapshot>) -> Option<Arc<$snapshot>> {
            self.shared.load_if_changed(cached)
        }

        #[inline]
        pub fn load_if_changed_since(
            &self,
            cached_generation: u64,
        ) -> Option<(Arc<$snapshot>, u64)> {
            self.shared.load_if_changed_since(cached_generation)
        }
    };
}

macro_rules! impl_set_enabled_accessor {
    () => {
        #[inline]
        pub fn set_enabled(&self, enabled: bool) {
            self.shared.update(|snapshot| {
                snapshot.enabled = enabled;
            });
        }
    };
}

macro_rules! impl_enabled_reader {
    () => {
        #[inline]
        pub fn is_enabled(&self) -> bool {
            self.read().enabled
        }
    };
}

// ============================================================================
// EQ Parameters
// ============================================================================

/// EQ band count constant
pub const EQ_BANDS: usize = 10;

/// EQ parameter snapshot for audio thread
#[derive(Debug, Clone, Copy)]
pub struct EqParamsSnapshot {
    /// Gain for each band in dB
    pub gains: [f64; EQ_BANDS],
    /// Whether EQ is enabled
    pub enabled: bool,
}

impl Default for EqParamsSnapshot {
    fn default() -> Self {
        Self {
            gains: [0.0; EQ_BANDS],
            enabled: false,
        }
    }
}

/// EQ parameters published as complete immutable snapshots.
pub struct AtomicEqParams {
    shared: SharedParams<EqParamsSnapshot>,
}

impl AtomicEqParams {
    /// Create new EQ params with default values
    pub fn new() -> Self {
        Self {
            shared: SharedParams::new(),
        }
    }

    /// Publish all EQ parameters as a complete snapshot.
    pub fn write(&self, gains: &[f64; EQ_BANDS], enabled: bool) {
        self.shared.publish(EqParamsSnapshot {
            gains: *gains,
            enabled,
        });
    }

    /// Read the current EQ parameter snapshot.
    pub fn read(&self) -> EqParamsSnapshot {
        self.shared.read()
    }

    impl_snapshot_accessors!(EqParamsSnapshot);

    /// Update a single band gain by patching and publishing a new snapshot.
    pub fn set_band_gain(&self, band: usize, gain_db: f64) {
        if band >= EQ_BANDS {
            return;
        }
        self.shared.update(|snap| {
            snap.gains[band] = gain_db.clamp(-15.0, 15.0);
        });
    }

    /// Set enabled state (main thread)
    pub fn set_enabled(&self, enabled: bool) {
        self.shared.update(|snap| {
            snap.enabled = enabled;
        });
    }

    // Quick read of enabled state only.
    impl_enabled_reader!();
}

impl_default_via_new!(AtomicEqParams);

// ============================================================================
// Saturation Parameters (Simple Atomic)
// ============================================================================

/// Saturation type enumeration for lock-free parameter passing.
///
/// M-4 fix: Provides bidirectional conversion with SaturationType
/// from the saturation module, eliminating unsafe string-based mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SaturationTypeValue {
    #[default]
    Tape = 0,
    Tube = 1,
    Transistor = 2,
}

impl From<u8> for SaturationTypeValue {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Tape,
            1 => Self::Tube,
            2 => Self::Transistor,
            _ => Self::default(),
        }
    }
}

impl From<crate::processor::SaturationType> for SaturationTypeValue {
    fn from(st: crate::processor::SaturationType) -> Self {
        match st {
            crate::processor::SaturationType::Tape => Self::Tape,
            crate::processor::SaturationType::Tube => Self::Tube,
            crate::processor::SaturationType::Transistor => Self::Transistor,
        }
    }
}

impl From<SaturationTypeValue> for crate::processor::SaturationType {
    fn from(v: SaturationTypeValue) -> Self {
        match v {
            SaturationTypeValue::Tape => Self::Tape,
            SaturationTypeValue::Tube => Self::Tube,
            SaturationTypeValue::Transistor => Self::Transistor,
        }
    }
}

impl From<super::dsp::NoiseShaperCurve> for u8 {
    fn from(curve: super::dsp::NoiseShaperCurve) -> Self {
        match curve {
            super::dsp::NoiseShaperCurve::Lipshitz5 => 0,
            super::dsp::NoiseShaperCurve::FWeighted9 => 1,
            super::dsp::NoiseShaperCurve::ModifiedE9 => 2,
            super::dsp::NoiseShaperCurve::ImprovedE9 => 3,
            super::dsp::NoiseShaperCurve::TpdfOnly => 4,
        }
    }
}

impl From<u8> for super::dsp::NoiseShaperCurve {
    fn from(value: u8) -> Self {
        match value {
            0 => super::dsp::NoiseShaperCurve::Lipshitz5,
            1 => super::dsp::NoiseShaperCurve::FWeighted9,
            2 => super::dsp::NoiseShaperCurve::ModifiedE9,
            3 => super::dsp::NoiseShaperCurve::ImprovedE9,
            4 => super::dsp::NoiseShaperCurve::TpdfOnly,
            _ => super::dsp::NoiseShaperCurve::Lipshitz5,
        }
    }
}

/// Saturation parameter snapshot
#[derive(Debug, Clone, Copy)]
pub struct SaturationParamsSnapshot {
    pub drive: f64,
    pub threshold: f64,
    pub mix: f64,
    pub sat_type: SaturationTypeValue,
    pub input_gain_db: f64,
    pub output_gain_db: f64,
    pub highpass_mode: bool,
    pub highpass_cutoff: f64,
    pub enabled: bool,
}

impl Default for SaturationParamsSnapshot {
    fn default() -> Self {
        Self {
            drive: 0.25,
            threshold: 0.88,
            mix: 0.2,
            sat_type: SaturationTypeValue::Tube,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            highpass_mode: false,
            highpass_cutoff: 4000.0,
            enabled: true,
        }
    }
}

/// Saturation parameters published as complete immutable snapshots.
pub struct AtomicSaturationParams {
    shared: SharedParams<SaturationParamsSnapshot>,
}

impl AtomicSaturationParams {
    pub fn new() -> Self {
        Self {
            shared: SharedParams::new(),
        }
    }

    /// Set drive amount (0.0 - 2.0)
    #[inline]
    pub fn set_drive(&self, drive: f64) {
        self.shared.update(|snapshot| {
            snapshot.drive = drive.clamp(0.0, 2.0);
        });
    }

    /// Set threshold (0.0 - 1.0)
    #[inline]
    pub fn set_threshold(&self, threshold: f64) {
        self.shared.update(|snapshot| {
            snapshot.threshold = threshold.clamp(0.0, 1.0);
        });
    }

    /// Set mix amount (0.0 - 1.0)
    #[inline]
    pub fn set_mix(&self, mix: f64) {
        self.shared.update(|snapshot| {
            snapshot.mix = mix.clamp(0.0, 1.0);
        });
    }

    /// Set saturation type
    #[inline]
    pub fn set_sat_type(&self, sat_type: SaturationTypeValue) {
        self.shared.update(|snapshot| {
            snapshot.sat_type = sat_type;
        });
    }

    /// Set input gain (dB)
    #[inline]
    pub fn set_input_gain(&self, gain_db: f64) {
        self.shared.update(|snapshot| {
            snapshot.input_gain_db = gain_db;
        });
    }

    /// Set output gain (dB)
    #[inline]
    pub fn set_output_gain(&self, gain_db: f64) {
        self.shared.update(|snapshot| {
            snapshot.output_gain_db = gain_db;
        });
    }

    /// Set highpass mode
    #[inline]
    pub fn set_highpass_mode(&self, enabled: bool) {
        self.shared.update(|snapshot| {
            snapshot.highpass_mode = enabled;
        });
    }

    /// Set highpass cutoff frequency
    #[inline]
    pub fn set_highpass_cutoff(&self, hz: f64) {
        self.shared.update(|snapshot| {
            snapshot.highpass_cutoff = hz.clamp(1000.0, 12000.0);
        });
    }

    impl_set_enabled_accessor!();

    /// Read all parameters into a snapshot
    #[inline]
    pub fn read(&self) -> SaturationParamsSnapshot {
        self.shared.read()
    }

    impl_snapshot_accessors!(SaturationParamsSnapshot);

    // Quick check if enabled.
    impl_enabled_reader!();
}

impl_default_via_new!(AtomicSaturationParams);

// ============================================================================
// Crossfeed Parameters
// ============================================================================

/// Crossfeed parameter snapshot
#[derive(Debug, Clone, Copy)]
pub struct CrossfeedParamsSnapshot {
    pub mix: f64,
    pub cutoff_hz: f64,
    pub enabled: bool,
}

impl Default for CrossfeedParamsSnapshot {
    fn default() -> Self {
        Self {
            mix: 0.35,
            cutoff_hz: 700.0,
            enabled: true,
        }
    }
}

/// Atomic crossfeed parameters
pub struct AtomicCrossfeedParams {
    shared: SharedParams<CrossfeedParamsSnapshot>,
}

impl AtomicCrossfeedParams {
    pub fn new() -> Self {
        Self {
            shared: SharedParams::new(),
        }
    }

    #[inline]
    pub fn set_mix(&self, mix: f64) {
        self.shared.update(|snapshot| {
            snapshot.mix = mix.clamp(0.0, 1.0);
        });
    }

    #[inline]
    pub fn set_cutoff(&self, hz: f64) {
        self.shared.update(|snapshot| {
            snapshot.cutoff_hz = hz.clamp(200.0, 2000.0);
        });
    }

    impl_set_enabled_accessor!();

    #[inline]
    pub fn read(&self) -> CrossfeedParamsSnapshot {
        self.shared.read()
    }

    impl_snapshot_accessors!(CrossfeedParamsSnapshot);

    impl_enabled_reader!();
}

impl_default_via_new!(AtomicCrossfeedParams);

// ============================================================================
// Peak Limiter Parameters
// ============================================================================

/// Peak limiter parameter snapshot
#[derive(Debug, Clone, Copy)]
pub struct PeakLimiterParamsSnapshot {
    pub threshold_db: f64,
    pub release_ms: f64,
    pub enabled: bool,
}

impl Default for PeakLimiterParamsSnapshot {
    fn default() -> Self {
        Self {
            threshold_db: -1.0,
            release_ms: 150.0,
            enabled: true,
        }
    }
}

/// Atomic peak limiter parameters
pub struct AtomicPeakLimiterParams {
    shared: SharedParams<PeakLimiterParamsSnapshot>,
}

impl AtomicPeakLimiterParams {
    pub fn new() -> Self {
        Self {
            shared: SharedParams::new(),
        }
    }

    #[inline]
    pub fn set_threshold(&self, db: f64) {
        self.shared.update(|snapshot| {
            snapshot.threshold_db = db.clamp(-20.0, 0.0);
        });
    }

    #[inline]
    pub fn set_release(&self, ms: f64) {
        self.shared.update(|snapshot| {
            snapshot.release_ms = ms.clamp(10.0, 1000.0);
        });
    }

    impl_set_enabled_accessor!();

    #[inline]
    pub fn read(&self) -> PeakLimiterParamsSnapshot {
        self.shared.read()
    }

    impl_snapshot_accessors!(PeakLimiterParamsSnapshot);

    impl_enabled_reader!();
}

impl_default_via_new!(AtomicPeakLimiterParams);

// ============================================================================
// Volume Parameters
// ============================================================================

/// Volume parameter snapshot
#[derive(Debug, Clone, Copy)]
pub struct VolumeParamsSnapshot {
    pub volume: f64, // 0.0 - 1.0
    pub muted: bool,
}

impl Default for VolumeParamsSnapshot {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
        }
    }
}

/// Atomic volume parameters
pub struct AtomicVolumeParams {
    shared: SharedParams<VolumeParamsSnapshot>,
}

impl AtomicVolumeParams {
    pub fn new() -> Self {
        Self {
            shared: SharedParams::new(),
        }
    }

    /// Set volume (0.0 = silence, 1.0 = full)
    #[inline]
    pub fn set_volume(&self, vol: f64) {
        self.shared.update(|snapshot| {
            snapshot.volume = vol.clamp(0.0, 1.0);
        });
    }

    /// Set mute state
    #[inline]
    pub fn set_muted(&self, muted: bool) {
        self.shared.update(|snapshot| {
            snapshot.muted = muted;
        });
    }

    /// Read current state
    #[inline]
    pub fn read(&self) -> VolumeParamsSnapshot {
        self.shared.read()
    }

    impl_snapshot_accessors!(VolumeParamsSnapshot);

    /// Get effective volume (0.0 if muted)
    #[inline]
    pub fn effective_volume(&self) -> f64 {
        let snapshot = self.read();
        if snapshot.muted {
            0.0
        } else {
            snapshot.volume
        }
    }
}

impl_default_via_new!(AtomicVolumeParams);

// ============================================================================
// Noise Shaper Parameters
// ============================================================================

/// Noise shaper parameter snapshot
#[derive(Debug, Clone, Copy)]
pub struct NoiseShaperParamsSnapshot {
    pub enabled: bool,
    pub bits: u32,
    pub curve: super::dsp::NoiseShaperCurve,
}

impl Default for NoiseShaperParamsSnapshot {
    fn default() -> Self {
        Self {
            enabled: true,
            bits: 24,
            curve: super::dsp::NoiseShaperCurve::Lipshitz5,
        }
    }
}

/// Atomic noise shaper parameters
pub struct AtomicNoiseShaperParams {
    shared: SharedParams<NoiseShaperParamsSnapshot>,
}

impl AtomicNoiseShaperParams {
    pub fn new() -> Self {
        Self {
            shared: SharedParams::new(),
        }
    }

    impl_set_enabled_accessor!();

    #[inline]
    pub fn set_bits(&self, bits: u32) {
        self.shared.update(|snapshot| {
            snapshot.bits = bits.clamp(8, 32);
        });
    }

    #[inline]
    pub fn set_curve(&self, curve: super::dsp::NoiseShaperCurve) {
        self.shared.update(|snapshot| {
            snapshot.curve = curve;
        });
    }

    #[inline]
    pub fn read(&self) -> NoiseShaperParamsSnapshot {
        self.shared.read()
    }

    impl_snapshot_accessors!(NoiseShaperParamsSnapshot);

    impl_enabled_reader!();

    #[inline]
    pub fn bits(&self) -> u32 {
        self.read().bits
    }

    #[inline]
    pub fn curve(&self) -> super::dsp::NoiseShaperCurve {
        self.read().curve
    }
}

impl_default_via_new!(AtomicNoiseShaperParams);

// ============================================================================
// Dynamic Loudness Parameters
// ============================================================================

/// Dynamic loudness parameter snapshot
#[derive(Debug, Clone, Copy)]
pub struct DynamicLoudnessParamsSnapshot {
    pub enabled: bool,
    pub volume: f64,
    pub strength: f64,
    pub ref_volume_db: Option<f64>,
}

impl Default for DynamicLoudnessParamsSnapshot {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 1.0,
            strength: 1.0,
            ref_volume_db: None,
        }
    }
}

/// Atomic dynamic loudness parameters
pub struct AtomicDynamicLoudnessParams {
    shared: SharedParams<DynamicLoudnessParamsSnapshot>,
}

impl AtomicDynamicLoudnessParams {
    pub fn new() -> Self {
        Self {
            shared: SharedParams::new(),
        }
    }

    impl_set_enabled_accessor!();

    #[inline]
    pub fn set_volume(&self, vol: f64) {
        self.shared.update(|snapshot| {
            snapshot.volume = vol.clamp(0.0, 1.0);
            snapshot.ref_volume_db = None;
        });
    }

    /// Set the reference volume in dB and publish the derived linear volume.
    #[inline]
    pub fn set_ref_volume_db(&self, db: f64) {
        let mut snapshot = self.shared.read();
        if snapshot.ref_volume_db == Some(db) {
            return;
        }
        snapshot.ref_volume_db = Some(db);
        // Convert dB to linear (0dB = 1.0, -20dB = 0.1, etc.)
        snapshot.volume = 10f64.powf(db / 20.0).clamp(0.0, 1.0);
        self.shared.publish(snapshot);
    }

    /// Set strength (0.0 - 1.0)
    #[inline]
    pub fn set_strength(&self, strength: f64) {
        self.shared.update(|snapshot| {
            snapshot.strength = strength.clamp(0.0, 1.0);
        });
    }

    #[inline]
    pub fn read(&self) -> DynamicLoudnessParamsSnapshot {
        self.shared.read()
    }

    impl_snapshot_accessors!(DynamicLoudnessParamsSnapshot);

    impl_enabled_reader!();

    /// Get strength (0.0 - 1.0)
    #[inline]
    pub fn strength(&self) -> f64 {
        self.read().strength
    }
}

impl_default_via_new!(AtomicDynamicLoudnessParams);

/// Real-time dynamic loudness telemetry published by audio thread.
///
/// Exposes the current loudness compensation factor and 7-band gains
/// for UI/state query without touching real-time processor internals.
pub struct AtomicDynamicLoudnessTelemetry {
    factor: AtomicF64,
    band_gains: [AtomicF64; 7],
}

impl AtomicDynamicLoudnessTelemetry {
    pub fn new() -> Self {
        Self {
            factor: AtomicF64::new(0.0),
            band_gains: std::array::from_fn(|_| AtomicF64::new(0.0)),
        }
    }

    #[inline]
    pub fn update(&self, factor: f64, band_gains: [f64; 7]) {
        self.factor.store(factor, Ordering::Release);
        for (dst, gain) in self.band_gains.iter().zip(band_gains.iter().copied()) {
            dst.store(gain, Ordering::Release);
        }
    }

    #[inline]
    pub fn factor(&self) -> f64 {
        self.factor.load(Ordering::Acquire)
    }

    #[inline]
    pub fn band_gains(&self) -> [f64; 7] {
        let _ = self.factor.load(Ordering::Acquire);
        std::array::from_fn(|i| self.band_gains[i].load(Ordering::Relaxed))
    }
}

impl_default_via_new!(AtomicDynamicLoudnessTelemetry);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eq_params_write_read() {
        let params = AtomicEqParams::new();
        let gains = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];

        params.write(&gains, true);

        let snapshot = params.read();
        for (i, &g) in gains.iter().enumerate() {
            assert!((snapshot.gains[i] - g).abs() < 1e-10);
        }
        assert!(snapshot.enabled);
    }

    #[test]
    fn test_saturation_params() {
        let params = AtomicSaturationParams::new();

        params.set_drive(1.5);
        params.set_mix(0.7);
        params.set_enabled(true);

        let snapshot = params.read();
        assert!((snapshot.drive - 1.5).abs() < 1e-10);
        assert!((snapshot.mix - 0.7).abs() < 1e-10);
        assert!(snapshot.enabled);
    }

    #[test]
    fn test_simple_param_burst_final_state_visible() {
        let params = AtomicDynamicLoudnessParams::new();
        for i in 0..100 {
            params.set_volume(i as f64 / 100.0);
            params.set_strength(1.0 - i as f64 / 100.0);
        }

        let snapshot = params.read();
        assert!((snapshot.volume - 0.99).abs() < 1e-10);
        assert!((snapshot.strength - 0.01).abs() < 1e-10);
        assert!(snapshot.enabled);
    }

    #[test]
    fn test_eq_snapshot_publication_keeps_old_and_new_consistent() {
        let params = AtomicEqParams::new();
        let old = params.load();

        params.set_band_gain(3, 6.0);
        let new = params.load();

        assert!(!Arc::ptr_eq(&old, &new));
        assert_eq!(old.gains, [0.0; EQ_BANDS]);
        assert!((new.gains[3] - 6.0).abs() < 1e-10);
        for (index, gain) in new.gains.iter().enumerate() {
            if index != 3 {
                assert!((*gain - 0.0).abs() < 1e-10);
            }
        }
    }

    #[test]
    fn test_dynamic_loudness_ref_volume_db_skips_unchanged_publish() {
        let params = AtomicDynamicLoudnessParams::new();

        params.set_ref_volume_db(-6.0);
        let first = params.load();

        params.set_ref_volume_db(-6.0);
        let second = params.load();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn test_telemetry_band_gains_round_trip() {
        let telemetry = AtomicDynamicLoudnessTelemetry::new();
        let gains = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0];

        telemetry.update(0.5, gains);

        assert!((telemetry.factor() - 0.5).abs() < 1e-10);
        assert_eq!(telemetry.band_gains(), gains);
    }

    #[test]
    fn test_volume_params_muted() {
        let params = AtomicVolumeParams::new();

        params.set_volume(0.5);
        assert!((params.effective_volume() - 0.5).abs() < 1e-10);

        params.set_muted(true);
        assert!((params.effective_volume() - 0.0).abs() < 1e-10);
    }
}
