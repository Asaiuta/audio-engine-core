mod control;
mod handoff;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use control::ConvolverDropProbe;
pub use control::{ConvolverControl, ConvolverStatus};

use control::{ConsumerLease, PublishedConvolver};
use handoff::AudioOwned;

use super::{process_fixed_1_to_1, validate_channels, validate_sample_rate, FixedLifecycle};
use crate::processor::traits::{
    AudioBlockMut, ProcessBuffers, ProcessError, ProcessProgress, ProcessState, StreamingProcessor,
    TailSpec,
};

/// FFT convolver adapter with fixed, wait-free ownership hand-off.
pub struct ConvolverProcessor {
    owned: Option<AudioOwned<PublishedConvolver>>,
    incoming: Option<AudioOwned<PublishedConvolver>>,
    pending_retire: Option<AudioOwned<PublishedConvolver>>,
    control: ConvolverControl,
    lifecycle: FixedLifecycle,
    sample_rate_hz: u32,
    finish_remaining_frames: Option<usize>,
    finish_generation: Option<u64>,
    // Keep the lease last so local heavy ownership is dropped before another
    // consumer can acquire the control during non-RT teardown.
    _consumer_lease: ConsumerLease,
}

impl ConvolverProcessor {
    pub fn new(control: ConvolverControl) -> Result<Self, ProcessError> {
        let consumer_lease = control.acquire_consumer()?;
        Ok(Self {
            owned: None,
            incoming: None,
            pending_retire: None,
            control,
            lifecycle: FixedLifecycle::default(),
            sample_rate_hz: 44_100,
            finish_remaining_frames: None,
            finish_generation: None,
            _consumer_lease: consumer_lease,
        })
    }

    pub fn control(&self) -> ConvolverControl {
        self.control.clone()
    }

    fn try_flush_retired(&mut self) {
        if let Some(retired) = self.pending_retire.take() {
            if let Err(retired) = self.control.try_retire(retired) {
                self.pending_retire = Some(retired);
            }
        }
    }

    fn sync_convolver(&mut self) {
        self.try_flush_retired();

        if self.incoming.is_none() {
            self.incoming = self.control.take_published();
        }

        if !self.control.is_enabled() {
            if self.pending_retire.is_none() {
                if let Some(current) = self.owned.take() {
                    self.pending_retire = Some(current);
                    self.control.note_retired();
                    self.try_flush_retired();
                } else if let Some(incoming) = self.incoming.take() {
                    self.pending_retire = Some(incoming);
                    self.control.note_discarded();
                    self.control.note_retired();
                    self.try_flush_retired();
                }
            }

            let still_holding =
                self.owned.is_some() || self.incoming.is_some() || self.pending_retire.is_some();
            if still_holding {
                self.control.mark_backpressured();
            } else {
                self.control.clear_backpressure();
                self.control.acknowledge_drained();
            }
            return;
        }

        let Some(incoming) = self.incoming.take() else {
            if self.pending_retire.is_some() {
                self.control.mark_backpressured();
            } else {
                self.control.clear_backpressure();
            }
            return;
        };
        let generation = incoming.get().generation;
        match self.owned.take() {
            None => {
                self.owned = Some(incoming);
                self.control.note_adopted(generation);
                self.control.clear_backpressure();
            }
            Some(old) if self.pending_retire.is_none() => {
                self.owned = Some(incoming);
                self.control.note_adopted(generation);
                self.pending_retire = Some(old);
                self.control.note_retired();
                self.try_flush_retired();
                if self.pending_retire.is_some() {
                    self.control.mark_backpressured();
                } else {
                    self.control.clear_backpressure();
                }
            }
            Some(old) => {
                self.owned = Some(old);
                self.incoming = Some(incoming);
                self.control.mark_backpressured();
            }
        }
    }
}

impl StreamingProcessor for ConvolverProcessor {
    fn name(&self) -> &'static str {
        "Convolver"
    }

    fn process(&mut self, buffers: ProcessBuffers<'_>) -> Result<ProcessProgress, ProcessError> {
        self.lifecycle.ensure_processing("Convolver")?;
        self.sync_convolver();

        if !self.control.is_enabled() {
            return process_fixed_1_to_1("Convolver", false, None, buffers, |_, _| Ok(()));
        }
        let Some(owned) = self.owned.as_mut() else {
            return process_fixed_1_to_1("Convolver", false, None, buffers, |_, _| Ok(()));
        };
        let convolver = owned.get_mut();
        let channels = convolver.kernel.channels();
        process_fixed_1_to_1(
            "Convolver",
            true,
            Some(channels),
            buffers,
            |buffer, _channels| {
                convolver.kernel.process_inplace(buffer);
                Ok(())
            },
        )
    }

    fn finish(&mut self, output: AudioBlockMut<'_>) -> Result<ProcessProgress, ProcessError> {
        let enabled = self.control.is_enabled();
        if self.lifecycle.is_finished() {
            if !enabled {
                self.sync_convolver();
            }
            return Ok(ProcessProgress::finished(0));
        }

        let already_finishing = self.finish_remaining_frames.is_some();
        if !already_finishing && !enabled {
            self.sync_convolver();
        }

        let channels = if already_finishing || enabled {
            self.owned
                .as_ref()
                .map(|convolver| convolver.get().kernel.channels())
        } else {
            None
        };
        validate_channels("Convolver", channels, output.channels())?;

        if !already_finishing {
            self.lifecycle.begin_finish();
            let (generation, remaining) = if enabled {
                self.owned
                    .as_ref()
                    .map(|convolver| {
                        let convolver = convolver.get();
                        (
                            Some(convolver.generation),
                            convolver.kernel.ir_length().saturating_sub(1),
                        )
                    })
                    .unwrap_or((None, 0))
            } else {
                (None, 0)
            };
            self.finish_generation = generation;
            self.finish_remaining_frames = Some(remaining);
        }

        let Some(remaining) = self.finish_remaining_frames.as_mut() else {
            return Err(ProcessError::Backend {
                processor: "Convolver",
                operation: "finish",
                message: "finish state was not initialized",
            });
        };
        if *remaining == 0 {
            return Ok(self.lifecycle.finish());
        }

        let mut output = output;
        let frames = output.frames().min(*remaining);
        let samples = frames * output.channels();
        output.samples_mut()[..samples].fill(0.0);
        if frames > 0 {
            let Some(owned) = self.owned.as_mut() else {
                return Err(ProcessError::Backend {
                    processor: "Convolver",
                    operation: "finish",
                    message: "finish-locked kernel is missing",
                });
            };
            let convolver = owned.get_mut();
            if Some(convolver.generation) != self.finish_generation {
                return Err(ProcessError::Backend {
                    processor: "Convolver",
                    operation: "finish",
                    message: "finish-locked generation changed",
                });
            }
            convolver
                .kernel
                .process_inplace(&mut output.samples_mut()[..samples]);
        }
        *remaining -= frames;

        if *remaining == 0 {
            let _ = self.lifecycle.finish();
            Ok(ProcessProgress::finished(frames))
        } else {
            Ok(ProcessProgress::new(0, frames, ProcessState::NeedOutput))
        }
    }

    fn reset(&mut self) -> Result<(), ProcessError> {
        if let Some(owned) = self.owned.as_mut() {
            owned.get_mut().kernel.reset();
        }
        self.lifecycle.reset();
        self.finish_remaining_frames = None;
        self.finish_generation = None;
        Ok(())
    }

    fn tail(&self) -> TailSpec {
        if let Some(remaining) = self.finish_remaining_frames {
            return TailSpec::finite(remaining, self.sample_rate_hz).unwrap_or(TailSpec::Unknown);
        }
        if !self.control.is_enabled() {
            return TailSpec::None;
        }
        let frames = self
            .owned
            .as_ref()
            .map(|convolver| convolver.get().kernel.ir_length().saturating_sub(1))
            .unwrap_or(0);
        TailSpec::finite(frames, self.sample_rate_hz).unwrap_or(TailSpec::Unknown)
    }

    fn is_enabled(&self) -> bool {
        self.control.is_enabled()
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.control.set_enabled(enabled);
    }

    fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        validate_sample_rate("Convolver", sample_rate_hz)?;
        self.sample_rate_hz = sample_rate_hz;
        self.lifecycle.reset();
        self.finish_remaining_frames = None;
        self.finish_generation = None;
        Ok(())
    }
}
