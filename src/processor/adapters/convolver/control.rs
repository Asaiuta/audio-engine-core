use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::processor::convolver::FFTConvolver;
use crate::processor::traits::ProcessError;

use super::handoff::{AtomicBoxSlot, AudioOwned};

/// Compatibility rate used by the legacy [`ConvolverControl::publish`] API.
/// New code should publish with an explicit rate domain.
pub const DEFAULT_CONVOLVER_SAMPLE_RATE_HZ: u32 = 44_100;

/// Allocation-free snapshot of dynamic convolver lifecycle telemetry.
///
/// Counter fields are monotonic and eventually consistent. Use
/// [`ConvolverControl::is_quiescent`] for authoritative teardown decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConvolverStatus {
    pub enabled: bool,
    pub latest_published_generation: u64,
    pub latest_adopted_generation: u64,
    /// Most recent publication generation fully drained from audio-local state.
    pub audio_drained_generation: u64,
    pub adopted_kernels: u64,
    pub superseded_kernels: u64,
    pub discarded_kernels: u64,
    pub retired_kernels: u64,
    pub reclaimed_kernels: u64,
    pub deferred_adoptions: u64,
    pub pending_kernels: u64,
    pub pending_reclamations: u64,
    pub backpressured: bool,
    pub audio_idle: bool,
    /// Active stream rate reported by the audio consumer.
    pub active_sample_rate_hz: u32,
    /// Required rate while enabled processing waits for a matching kernel.
    pub waiting_for_sample_rate_hz: Option<u32>,
}

pub(super) struct PublishedConvolver {
    pub(super) generation: u64,
    pub(super) sample_rate_hz: u32,
    pub(super) kernel: FFTConvolver,
    #[cfg(test)]
    pub(crate) _drop_probe: Option<ConvolverDropProbe>,
}

#[cfg(test)]
pub(crate) struct ConvolverDropProbe {
    pub(crate) audio_thread_id: std::thread::ThreadId,
    pub(crate) dropped_on_audio: Arc<AtomicBool>,
    pub(crate) drop_count: Arc<AtomicU64>,
}

#[cfg(test)]
impl Drop for ConvolverDropProbe {
    fn drop(&mut self) {
        if std::thread::current().id() == self.audio_thread_id {
            self.dropped_on_audio.store(true, Ordering::Release);
        }
        self.drop_count.fetch_add(1, Ordering::AcqRel);
    }
}

struct ConvolverControlInner {
    control_gate: Mutex<()>,
    published: AtomicBoxSlot<PublishedConvolver>,
    retired: AtomicBoxSlot<PublishedConvolver>,
    enabled: AtomicBool,
    consumer_active: AtomicBool,
    latest_published_generation: AtomicU64,
    latest_adopted_generation: AtomicU64,
    audio_drained_generation: AtomicU64,
    adopted_kernels: AtomicU64,
    superseded_kernels: AtomicU64,
    discarded_kernels: AtomicU64,
    retired_kernels: AtomicU64,
    reclaimed_kernels: AtomicU64,
    deferred_adoptions: AtomicU64,
    backpressured: AtomicBool,
    active_sample_rate_hz: AtomicU64,
    waiting_for_sample_rate_hz: AtomicU64,
}

/// Cloneable control-plane handle for one dynamic Convolver audio consumer.
#[derive(Clone)]
pub struct ConvolverControl {
    inner: Arc<ConvolverControlInner>,
}

pub(super) struct ConsumerLease {
    inner: Arc<ConvolverControlInner>,
}

impl Drop for ConsumerLease {
    fn drop(&mut self) {
        self.inner.consumer_active.store(false, Ordering::Release);
    }
}

impl ConvolverControl {
    pub fn new(enabled: bool) -> Self {
        Self {
            inner: Arc::new(ConvolverControlInner {
                control_gate: Mutex::new(()),
                published: AtomicBoxSlot::empty(),
                retired: AtomicBoxSlot::empty(),
                enabled: AtomicBool::new(enabled),
                consumer_active: AtomicBool::new(false),
                latest_published_generation: AtomicU64::new(0),
                latest_adopted_generation: AtomicU64::new(0),
                audio_drained_generation: AtomicU64::new(0),
                adopted_kernels: AtomicU64::new(0),
                superseded_kernels: AtomicU64::new(0),
                discarded_kernels: AtomicU64::new(0),
                retired_kernels: AtomicU64::new(0),
                reclaimed_kernels: AtomicU64::new(0),
                deferred_adoptions: AtomicU64::new(0),
                backpressured: AtomicBool::new(false),
                active_sample_rate_hz: AtomicU64::new(0),
                waiting_for_sample_rate_hz: AtomicU64::new(0),
            }),
        }
    }

    pub fn publish(&self, kernel: FFTConvolver) -> u64 {
        #[cfg(test)]
        {
            self.publish_inner(kernel, DEFAULT_CONVOLVER_SAMPLE_RATE_HZ, None)
        }
        #[cfg(not(test))]
        {
            self.publish_inner(kernel, DEFAULT_CONVOLVER_SAMPLE_RATE_HZ)
        }
    }

    /// Publish a kernel in an explicit non-zero sample-rate domain.
    pub fn publish_at_rate(
        &self,
        kernel: FFTConvolver,
        sample_rate_hz: u32,
    ) -> Result<u64, ProcessError> {
        if sample_rate_hz == 0 {
            return Err(ProcessError::InvalidSampleRate {
                processor: "ConvolverControl",
                sample_rate_hz,
            });
        }
        #[cfg(test)]
        {
            Ok(self.publish_inner(kernel, sample_rate_hz, None))
        }
        #[cfg(not(test))]
        {
            Ok(self.publish_inner(kernel, sample_rate_hz))
        }
    }

    fn publish_inner(
        &self,
        kernel: FFTConvolver,
        sample_rate_hz: u32,
        #[cfg(test)] drop_probe: Option<ConvolverDropProbe>,
    ) -> u64 {
        let _control_guard = self
            .inner
            .control_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = self.reclaim_retired_unlocked();

        let generation = self
            .inner
            .latest_published_generation
            .load(Ordering::Relaxed)
            .wrapping_add(1);
        let replaced = self
            .inner
            .published
            .replace_on_control(Box::new(PublishedConvolver {
                generation,
                sample_rate_hz,
                kernel,
                #[cfg(test)]
                _drop_probe: drop_probe,
            }));
        self.inner
            .latest_published_generation
            .store(generation, Ordering::Release);
        if replaced.is_some() {
            self.inner
                .superseded_kernels
                .fetch_add(1, Ordering::Relaxed);
        }
        drop(replaced);

        let _ = self.reclaim_retired_unlocked();
        generation
    }

    #[cfg(test)]
    pub(crate) fn publish_with_drop_probe(
        &self,
        kernel: FFTConvolver,
        drop_probe: ConvolverDropProbe,
    ) -> u64 {
        self.publish_inner(kernel, DEFAULT_CONVOLVER_SAMPLE_RATE_HZ, Some(drop_probe))
    }

    pub fn reclaim_retired(&self) -> bool {
        let _control_guard = self
            .inner
            .control_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.reclaim_retired_unlocked()
    }

    fn reclaim_retired_unlocked(&self) -> bool {
        let Some(retired) = self.inner.retired.reclaim_on_control() else {
            return false;
        };
        self.inner.reclaimed_kernels.fetch_add(1, Ordering::Relaxed);
        drop(retired);
        true
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.inner.enabled.store(enabled, Ordering::Release);
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Acquire)
    }

    /// Authoritative teardown check for a stopped publisher set.
    pub fn is_quiescent(&self) -> bool {
        self.is_quiescent_with_slot_check_hook(|| {})
    }

    fn is_quiescent_with_slot_check_hook(&self, after_initial_slot_check: impl FnOnce()) -> bool {
        let _control_guard = self
            .inner
            .control_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_enabled() || !self.inner.published.is_empty() || !self.inner.retired.is_empty() {
            return false;
        }

        after_initial_slot_check();

        let generations_match = self.inner.audio_drained_generation.load(Ordering::Acquire)
            == self
                .inner
                .latest_published_generation
                .load(Ordering::Acquire);
        generations_match && self.inner.published.is_empty() && self.inner.retired.is_empty()
    }

    pub fn status(&self) -> ConvolverStatus {
        let latest_published_generation = self
            .inner
            .latest_published_generation
            .load(Ordering::Acquire);
        let audio_drained_generation = self.inner.audio_drained_generation.load(Ordering::Acquire);
        let adopted_kernels = self.inner.adopted_kernels.load(Ordering::Acquire);
        let superseded_kernels = self.inner.superseded_kernels.load(Ordering::Acquire);
        let discarded_kernels = self.inner.discarded_kernels.load(Ordering::Acquire);
        let retired_kernels = self.inner.retired_kernels.load(Ordering::Acquire);
        let reclaimed_kernels = self.inner.reclaimed_kernels.load(Ordering::Acquire);
        let completed_publications = adopted_kernels
            .saturating_add(superseded_kernels)
            .saturating_add(discarded_kernels);

        ConvolverStatus {
            enabled: self.is_enabled(),
            latest_published_generation,
            latest_adopted_generation: self.inner.latest_adopted_generation.load(Ordering::Acquire),
            audio_drained_generation,
            adopted_kernels,
            superseded_kernels,
            discarded_kernels,
            retired_kernels,
            reclaimed_kernels,
            deferred_adoptions: self.inner.deferred_adoptions.load(Ordering::Acquire),
            pending_kernels: latest_published_generation.saturating_sub(completed_publications),
            pending_reclamations: retired_kernels.saturating_sub(reclaimed_kernels),
            backpressured: self.inner.backpressured.load(Ordering::Acquire),
            audio_idle: audio_drained_generation == latest_published_generation,
            active_sample_rate_hz: self.inner.active_sample_rate_hz.load(Ordering::Acquire) as u32,
            waiting_for_sample_rate_hz: match self
                .inner
                .waiting_for_sample_rate_hz
                .load(Ordering::Acquire) as u32
            {
                0 => None,
                rate => Some(rate),
            },
        }
    }

    pub(super) fn acquire_consumer(&self) -> Result<ConsumerLease, ProcessError> {
        self.inner
            .consumer_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ProcessError::ConsumerAlreadyActive {
                processor: "Convolver",
            })?;
        Ok(ConsumerLease {
            inner: Arc::clone(&self.inner),
        })
    }

    pub(super) fn take_published(&self) -> Option<AudioOwned<PublishedConvolver>> {
        self.inner.published.take_for_audio()
    }

    pub(super) fn try_retire(
        &self,
        value: AudioOwned<PublishedConvolver>,
    ) -> Result<(), AudioOwned<PublishedConvolver>> {
        self.inner.retired.try_store_from_audio(value)
    }

    pub(super) fn note_adopted(&self, generation: u64) {
        self.inner
            .latest_adopted_generation
            .store(generation, Ordering::Release);
        self.inner.adopted_kernels.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn note_discarded(&self) {
        self.inner.discarded_kernels.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn note_retired(&self) {
        self.inner.retired_kernels.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn mark_backpressured(&self) {
        if !self.inner.backpressured.swap(true, Ordering::AcqRel) {
            self.inner
                .deferred_adoptions
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn clear_backpressure(&self) {
        self.inner.backpressured.store(false, Ordering::Release);
    }

    pub(super) fn set_active_sample_rate(&self, sample_rate_hz: u32) {
        self.inner
            .active_sample_rate_hz
            .store(sample_rate_hz as u64, Ordering::Release);
    }

    pub(super) fn set_waiting_sample_rate(&self, sample_rate_hz: Option<u32>) {
        self.inner
            .waiting_for_sample_rate_hz
            .store(sample_rate_hz.unwrap_or(0) as u64, Ordering::Release);
    }

    pub(super) fn acknowledge_drained(&self) {
        self.acknowledge_drained_with(|| {});
    }

    fn acknowledge_drained_with(&self, after_generation_load: impl FnOnce()) {
        if !self.inner.published.is_empty() {
            return;
        }
        let generation = self
            .inner
            .latest_published_generation
            .load(Ordering::Acquire);
        after_generation_load();
        if self.inner.published.is_empty() {
            self.inner
                .audio_drained_generation
                .store(generation, Ordering::Release);
        }
    }

    #[cfg(test)]
    pub(crate) fn acknowledge_drained_with_test_hook(&self, hook: impl FnOnce()) {
        self.acknowledge_drained_with(hook);
    }

    #[cfg(test)]
    pub(crate) fn is_quiescent_with_test_hook(&self, hook: impl FnOnce()) -> bool {
        self.is_quiescent_with_slot_check_hook(hook)
    }

    #[cfg(test)]
    pub(crate) fn set_generation_state_for_test(&self, published: u64, drained: u64) {
        self.inner
            .latest_published_generation
            .store(published, Ordering::Release);
        self.inner
            .audio_drained_generation
            .store(drained, Ordering::Release);
    }
}

impl Default for ConvolverControl {
    fn default() -> Self {
        Self::new(false)
    }
}
