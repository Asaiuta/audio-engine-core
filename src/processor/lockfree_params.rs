//! Lock-free Parameter Structures
//!
//! Provides snapshot-based parameter passing from main thread to audio thread.
//! This eliminates the need for mutexes in the audio callback, ensuring
//! that DSP processing is never blocked or skipped due to lock contention.
//!
//! # Design Pattern
//!
//! Control-side reads retain convenient immutable `ArcSwap` snapshots.
//! Realtime consumers subscribe during setup to a dedicated hazard slot and
//! copy a complete `Copy` snapshot at block boundaries. Replaced storage is
//! reclaimed only by the control-side publisher, so the audio thread never
//! allocates, deallocates, or becomes the last owner of a published snapshot.

use std::ptr;
use std::sync::{
    atomic::{AtomicPtr, AtomicU64, Ordering},
    Arc, Mutex, MutexGuard,
};

use arc_swap::{ArcSwap, Guard};
use atomic_float::AtomicF64;

use crate::processor::loudness::LimiterMode;

use super::crossfeed::{
    DEFAULT_CUTOFF_HZ as CROSSFEED_DEFAULT_CUTOFF_HZ, DEFAULT_MIX as CROSSFEED_DEFAULT_MIX,
    MAX_CUTOFF_HZ as CROSSFEED_MAX_CUTOFF_HZ, MIN_CUTOFF_HZ as CROSSFEED_MIN_CUTOFF_HZ,
};

struct RealtimeSnapshotControl<T> {
    readers: Vec<Arc<AtomicPtr<T>>>,
    retired: Vec<Box<T>>,
}

struct RealtimeSnapshot<T> {
    current: AtomicPtr<T>,
    sequence: AtomicU64,
    control: Mutex<RealtimeSnapshotControl<T>>,
}

/// Pre-registered realtime reader for one immutable parameter snapshot type.
///
/// Obtain this only through the matching `Atomic*Params::subscribe_realtime`
/// method during setup, then retain it for allocation-free block-boundary
/// reads. The handle deliberately exposes no raw ownership operations.
pub struct RealtimeSnapshotReader<T> {
    hazard: Arc<AtomicPtr<T>>,
}

impl<T> Drop for RealtimeSnapshotReader<T> {
    fn drop(&mut self) {
        self.hazard.store(ptr::null_mut(), Ordering::SeqCst);
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl<T: Copy> RealtimeSnapshot<T> {
    fn new(snapshot: T) -> Self {
        Self {
            current: AtomicPtr::new(Box::into_raw(Box::new(snapshot))),
            sequence: AtomicU64::new(0),
            control: Mutex::new(RealtimeSnapshotControl {
                readers: Vec::new(),
                retired: Vec::new(),
            }),
        }
    }

    fn subscribe(&self) -> RealtimeSnapshotReader<T> {
        let hazard = Arc::new(AtomicPtr::new(ptr::null_mut()));
        lock_unpoisoned(&self.control)
            .readers
            .push(Arc::clone(&hazard));
        RealtimeSnapshotReader { hazard }
    }

    fn load_with_generation(&self, reader: &RealtimeSnapshotReader<T>) -> Option<(T, u64)> {
        let before = self.sequence.load(Ordering::SeqCst);
        if before & 1 != 0 {
            return None;
        }
        let pointer = self.current.load(Ordering::SeqCst);
        reader.hazard.store(pointer, Ordering::SeqCst);
        if self.current.load(Ordering::SeqCst) != pointer
            || self.sequence.load(Ordering::SeqCst) != before
        {
            reader.hazard.store(ptr::null_mut(), Ordering::SeqCst);
            return None;
        }

        // The writer retains every replaced Box while its pointer appears in
        // a reader hazard slot. The pointed-to snapshot is immutable and Copy.
        let snapshot = unsafe { *pointer };
        let after = self.sequence.load(Ordering::SeqCst);
        reader.hazard.store(ptr::null_mut(), Ordering::SeqCst);
        (before == after).then_some((snapshot, before / 2))
    }

    fn load_if_changed_since(
        &self,
        reader: &RealtimeSnapshotReader<T>,
        cached_generation: u64,
    ) -> Option<(T, u64)> {
        let sequence = self.sequence.load(Ordering::Acquire);
        if sequence & 1 != 0 || sequence / 2 == cached_generation {
            return None;
        }
        self.load_with_generation(reader)
            .filter(|(_, generation)| *generation != cached_generation)
    }

    fn publish(&self, snapshot: T) {
        let mut control = lock_unpoisoned(&self.control);
        let before = self.sequence.load(Ordering::SeqCst);
        debug_assert_eq!(before & 1, 0);
        let next = before.wrapping_add(2);
        self.sequence
            .store(before.wrapping_add(1), Ordering::SeqCst);
        let previous = self
            .current
            .swap(Box::into_raw(Box::new(snapshot)), Ordering::SeqCst);
        self.sequence.store(next, Ordering::SeqCst);

        // SAFETY: `previous` was created by Box::into_raw and atomically
        // removed from `current`; this control-side vector resumes ownership.
        control.retired.push(unsafe { Box::from_raw(previous) });
        let RealtimeSnapshotControl { readers, retired } = &mut *control;
        readers.retain(|reader| Arc::strong_count(reader) > 1);
        retired.retain(|retired| {
            let pointer = (&**retired) as *const T as *mut T;
            readers
                .iter()
                .any(|reader| reader.load(Ordering::SeqCst) == pointer)
        });
    }
}

impl<T> Drop for RealtimeSnapshot<T> {
    fn drop(&mut self) {
        let pointer = *self.current.get_mut();
        if !pointer.is_null() {
            // SAFETY: `self` has exclusive access and `pointer` is the one Box
            // still owned by the current slot.
            unsafe { drop(Box::from_raw(pointer)) };
        }
    }
}

struct SharedParams<T: Copy> {
    current: ArcSwap<T>,
    realtime: RealtimeSnapshot<T>,
    writer: Mutex<()>,
    generation: AtomicU64,
}

impl<T: Copy + Default> SharedParams<T> {
    fn new() -> Self {
        Self::from_snapshot(T::default())
    }
}

impl<T: Copy> SharedParams<T> {
    fn from_snapshot(snapshot: T) -> Self {
        Self {
            current: ArcSwap::new(Arc::new(snapshot)),
            realtime: RealtimeSnapshot::new(snapshot),
            writer: Mutex::new(()),
            generation: AtomicU64::new(0),
        }
    }

    #[inline]
    fn load(&self) -> Arc<T> {
        self.current.load_full()
    }

    /// Control-side coherent snapshot + generation read.
    ///
    /// This may briefly spin while a publisher is mid-publish (generation is
    /// odd), so it must only be called from control/UI threads. Realtime
    /// consumers must use `subscribe_realtime` +
    /// `load_realtime_if_changed_since`, whose failure mode is "keep the
    /// cached snapshot" instead of waiting.
    #[inline]
    fn load_with_generation(&self) -> (Arc<T>, u64) {
        loop {
            let before = self.generation.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let current = self.current.load_full();
            let after = self.generation.load(Ordering::Acquire);
            if before == after {
                return (current, after / 2);
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
        if generation & 1 == 0 && generation / 2 == cached_generation {
            return None;
        }
        let (current, generation) = self.load_with_generation();
        (generation != cached_generation).then_some((current, generation))
    }

    #[inline]
    fn publish(&self, snapshot: T) {
        let _writer = lock_unpoisoned(&self.writer);
        self.publish_locked(snapshot);
    }

    fn publish_locked(&self, snapshot: T) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.current.store(Arc::new(snapshot));
        self.realtime.publish(snapshot);
        self.generation.fetch_add(1, Ordering::Release);
    }

    fn subscribe_realtime(&self) -> (RealtimeSnapshotReader<T>, T, u64) {
        let reader = self.realtime.subscribe();
        loop {
            if let Some((snapshot, generation)) = self.realtime.load_with_generation(&reader) {
                return (reader, snapshot, generation);
            }
            std::thread::yield_now();
        }
    }

    #[inline]
    fn load_realtime_if_changed_since(
        &self,
        reader: &RealtimeSnapshotReader<T>,
        cached_generation: u64,
    ) -> Option<(T, u64)> {
        self.realtime
            .load_if_changed_since(reader, cached_generation)
    }
}

impl<T: Copy> SharedParams<T> {
    #[inline]
    fn read(&self) -> T {
        *self.current.load_full()
    }

    #[inline]
    fn update(&self, mut f: impl FnMut(&mut T)) {
        let _writer = lock_unpoisoned(&self.writer);
        let mut snapshot = **self.current.load();
        f(&mut snapshot);
        self.publish_locked(snapshot);
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

        /// Control-side coherent snapshot + generation read.
        ///
        /// May briefly spin while a publish is in flight; do not call from the
        /// audio callback. Realtime consumers use [`Self::subscribe_realtime`]
        /// and [`Self::load_realtime_if_changed_since`] instead, which never
        /// wait.
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

        /// Register one realtime consumer and return its initial snapshot.
        ///
        /// Registration allocates and takes a control-side lock, so call this
        /// before entering an audio callback.
        pub fn subscribe_realtime(&self) -> (RealtimeSnapshotReader<$snapshot>, $snapshot, u64) {
            self.shared.subscribe_realtime()
        }

        /// Copy a newly published complete snapshot without allocation or
        /// ownership destruction on the calling thread.
        #[inline]
        pub fn load_realtime_if_changed_since(
            &self,
            reader: &RealtimeSnapshotReader<$snapshot>,
            cached_generation: u64,
        ) -> Option<($snapshot, u64)> {
            self.shared
                .load_realtime_if_changed_since(reader, cached_generation)
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

/// Saturation processing quality for lock-free parameter passing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SaturationQualityValue {
    #[default]
    Direct = 0,
    Oversampled2x = 1,
    Oversampled4x = 2,
}

impl From<u8> for SaturationQualityValue {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Direct,
            1 => Self::Oversampled2x,
            2 => Self::Oversampled4x,
            _ => Self::default(),
        }
    }
}

impl From<crate::processor::SaturationQuality> for SaturationQualityValue {
    fn from(quality: crate::processor::SaturationQuality) -> Self {
        match quality {
            crate::processor::SaturationQuality::Direct => Self::Direct,
            crate::processor::SaturationQuality::Oversampled2x => Self::Oversampled2x,
            crate::processor::SaturationQuality::Oversampled4x => Self::Oversampled4x,
        }
    }
}

impl From<SaturationQualityValue> for crate::processor::SaturationQuality {
    fn from(v: SaturationQualityValue) -> Self {
        match v {
            SaturationQualityValue::Direct => Self::Direct,
            SaturationQualityValue::Oversampled2x => Self::Oversampled2x,
            SaturationQualityValue::Oversampled4x => Self::Oversampled4x,
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
    pub quality: SaturationQualityValue,
    pub input_gain_db: f64,
    pub output_gain_db: f64,
    pub highpass_mode: bool,
    pub highpass_cutoff: f64,
    pub enabled: bool,
    /// Setup/reset-time activation of the fixed-latency stage.
    pub armed: bool,
}

impl Default for SaturationParamsSnapshot {
    fn default() -> Self {
        Self {
            drive: 0.25,
            threshold: 0.88,
            mix: 0.2,
            sat_type: SaturationTypeValue::Tube,
            quality: SaturationQualityValue::Direct,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            highpass_mode: false,
            highpass_cutoff: 4000.0,
            enabled: true,
            armed: true,
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

    /// Publish all saturation settings as one coherent snapshot.
    #[inline]
    pub fn write(&self, snapshot: SaturationParamsSnapshot) {
        let mut snapshot = snapshot;
        snapshot.drive = snapshot.drive.clamp(0.0, 2.0);
        snapshot.threshold = snapshot.threshold.clamp(0.0, 1.0);
        snapshot.mix = snapshot.mix.clamp(0.0, 1.0);
        snapshot.highpass_cutoff = snapshot.highpass_cutoff.clamp(1000.0, 12000.0);
        self.shared.publish(snapshot);
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

    /// Set processing quality / antialiasing mode.
    #[inline]
    pub fn set_quality(&self, quality: SaturationQualityValue) {
        self.shared.update(|snapshot| {
            snapshot.quality = quality;
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

    /// Arm or hard-bypass the stage for the next reset/setup boundary.
    #[inline]
    pub fn set_armed(&self, armed: bool) {
        self.shared.update(|snapshot| {
            snapshot.armed = armed;
        });
    }

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
            mix: CROSSFEED_DEFAULT_MIX,
            cutoff_hz: CROSSFEED_DEFAULT_CUTOFF_HZ,
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

    /// Publish crossfeed settings as one coherent snapshot.
    #[inline]
    pub fn write(&self, enabled: bool, mix: f64, cutoff_hz: f64) {
        self.shared.publish(CrossfeedParamsSnapshot {
            enabled,
            mix: mix.clamp(0.0, 1.0),
            cutoff_hz: cutoff_hz.clamp(CROSSFEED_MIN_CUTOFF_HZ, CROSSFEED_MAX_CUTOFF_HZ),
        });
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
            snapshot.cutoff_hz = hz.clamp(CROSSFEED_MIN_CUTOFF_HZ, CROSSFEED_MAX_CUTOFF_HZ);
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
    pub mode: LimiterMode,
}

impl Default for PeakLimiterParamsSnapshot {
    fn default() -> Self {
        Self {
            threshold_db: -1.0,
            release_ms: 150.0,
            enabled: true,
            mode: LimiterMode::TruePeak,
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

    /// Select the detection [`LimiterMode`].
    ///
    /// The adapter applies mode changes in place with pre-sized limiter
    /// buffers, resetting limiter state when the active delay window changes.
    /// Set this from the control thread, not inside the audio callback.
    #[inline]
    pub fn set_mode(&self, mode: LimiterMode) {
        self.shared.update(|snapshot| {
            snapshot.mode = mode;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// Publish noise-shaping settings as one coherent snapshot.
    #[inline]
    pub fn write(&self, enabled: bool, bits: u32, curve: super::dsp::NoiseShaperCurve) {
        self.shared.publish(NoiseShaperParamsSnapshot {
            enabled,
            bits: bits.clamp(8, 32),
            curve,
        });
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

    /// Publish current listening volume and compensation strength as one
    /// coherent snapshot. `volume` is linear, where 1.0 is 0 dBFS.
    #[inline]
    pub fn write(&self, enabled: bool, volume: f64, strength: f64) {
        self.shared.publish(DynamicLoudnessParamsSnapshot {
            enabled,
            volume: volume.clamp(0.0, 1.0),
            strength: strength.clamp(0.0, 1.0),
            ref_volume_db: None,
        });
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
    use std::sync::atomic::AtomicBool;

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
    fn composite_publishers_do_not_expose_mixed_tuples() {
        let crossfeed = AtomicCrossfeedParams::new();
        crossfeed.write(true, 0.25, 900.0);
        let crossfeed_snapshot = crossfeed.read();
        assert!(crossfeed_snapshot.enabled);
        assert_eq!(crossfeed_snapshot.mix, 0.25);
        assert_eq!(crossfeed_snapshot.cutoff_hz, 900.0);

        let loudness = AtomicDynamicLoudnessParams::new();
        loudness.write(true, 0.25, 0.75);
        let loudness_snapshot = loudness.read();
        assert!(loudness_snapshot.enabled);
        assert_eq!(loudness_snapshot.volume, 0.25);
        assert_eq!(loudness_snapshot.strength, 0.75);

        let noise = AtomicNoiseShaperParams::new();
        noise.write(true, 16, crate::processor::dsp::NoiseShaperCurve::TpdfOnly);
        let noise_snapshot = noise.read();
        assert!(noise_snapshot.enabled);
        assert_eq!(noise_snapshot.bits, 16);
        assert_eq!(
            noise_snapshot.curve,
            crate::processor::dsp::NoiseShaperCurve::TpdfOnly
        );
    }

    #[test]
    fn test_saturation_params() {
        let params = AtomicSaturationParams::new();

        params.set_drive(1.5);
        params.set_mix(0.7);
        params.set_quality(SaturationQualityValue::Oversampled4x);
        params.set_enabled(true);

        let snapshot = params.read();
        assert!((snapshot.drive - 1.5).abs() < 1e-10);
        assert!((snapshot.mix - 0.7).abs() < 1e-10);
        assert_eq!(snapshot.quality, SaturationQualityValue::Oversampled4x);
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

    #[test]
    fn realtime_reader_is_allocation_free_during_concurrent_publication() {
        const UPDATES: u64 = 10_000;
        const MAX_READ_ATTEMPTS: usize = 20_000_000;

        let params = Arc::new(AtomicEqParams::new());
        let (reader, initial, initial_generation) = params.subscribe_realtime();
        let ready = Arc::new(AtomicBool::new(false));
        let start = Arc::new(AtomicBool::new(false));
        let publishing_done = Arc::new(AtomicBool::new(false));

        let audio_params = Arc::clone(&params);
        let audio_ready = Arc::clone(&ready);
        let audio_start = Arc::clone(&start);
        let audio_publishing_done = Arc::clone(&publishing_done);
        let audio = std::thread::spawn(move || {
            let mut snapshot = initial;
            let mut generation = initial_generation;
            let mut attempts = 0;

            assert_no_alloc::assert_no_alloc(|| {
                audio_ready.store(true, Ordering::Release);
                while !audio_start.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }

                while attempts < MAX_READ_ATTEMPTS {
                    attempts += 1;
                    if let Some((next, next_generation)) =
                        audio_params.load_realtime_if_changed_since(&reader, generation)
                    {
                        let marker = next.gains[0];
                        assert!(next.gains.iter().all(|gain| *gain == marker));
                        assert_eq!(next.enabled, (marker as u64) & 1 == 0);
                        snapshot = next;
                        generation = next_generation;
                    }

                    if audio_publishing_done.load(Ordering::Acquire) && generation == UPDATES {
                        break;
                    }
                    std::hint::spin_loop();
                }
            });

            (snapshot, generation, attempts)
        });

        while !ready.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        start.store(true, Ordering::Release);
        for update in 1..=UPDATES {
            params.write(&[update as f64; EQ_BANDS], update & 1 == 0);
        }
        publishing_done.store(true, Ordering::Release);

        let (snapshot, generation, attempts) = audio.join().unwrap();
        assert_eq!(
            generation, UPDATES,
            "reader stopped after {attempts} attempts"
        );
        assert_eq!(snapshot.gains, [UPDATES as f64; EQ_BANDS]);
        assert!(snapshot.enabled);
    }
}
