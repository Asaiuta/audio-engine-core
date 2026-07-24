//! Native SoXR (libsoxr) mono-channel resampler backend.
//!
//! One `MonoBackend` owns one `Soxr<Mono<f64>>` stream. SoXR natively provides
//! the semantics the shared resampler contract requires: arbitrary input chunk
//! sizes, duration-aligned output with no leading delay frames, `drain` as the
//! only end-of-stream operation, and `clear` restoring the initial state.

use crate::config::{PhaseResponse, ResampleQuality};
use soxr::{
    format::Mono,
    params::{QualityFlags, QualityRecipe, QualitySpec, Rolloff, RuntimeSpec},
    Soxr,
};

use super::BackendProgress;

pub(super) const BACKEND_NAME: &str = "soxr";

/// Convert ResampleQuality enum to SoX QualityRecipe
/// FIX for Defect 30: Actually use different quality levels
/// Note: QualityRecipe has Low variant, plus high() and very_high() constructor functions
fn quality_to_recipe(quality: ResampleQuality) -> QualityRecipe {
    match quality {
        ResampleQuality::Low => QualityRecipe::Low, // Fast, lower quality (enum variant)
        ResampleQuality::Standard => QualityRecipe::high(), // High quality (constructor)
        ResampleQuality::High => QualityRecipe::high(), // High quality (constructor)
        ResampleQuality::UltraHigh => QualityRecipe::very_high(), // VHQ, slowest (constructor)
    }
}

/// Create a QualitySpec with the given recipe and phase response
fn make_quality_spec(recipe: QualityRecipe, phase: PhaseResponse) -> QualitySpec {
    QualitySpec::configure(recipe, Rolloff::default(), QualityFlags::HighPrecisionClock)
        .with_phase_response(phase.to_soxr_value())
}

pub(super) struct MonoBackend {
    soxr: Soxr<Mono<f64>>,
}

impl MonoBackend {
    pub(super) fn new(
        from_rate: u32,
        to_rate: u32,
        phase: PhaseResponse,
        quality: ResampleQuality,
    ) -> Result<Self, String> {
        let quality_spec = make_quality_spec(quality_to_recipe(quality), phase);
        let runtime_spec = RuntimeSpec::new(1);
        Soxr::<Mono<f64>>::new_with_params(
            from_rate as f64,
            to_rate as f64,
            quality_spec,
            runtime_spec,
        )
        .map(|soxr| Self { soxr })
        .map_err(|error| format!("{error:?}"))
    }

    pub(super) fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<BackendProgress, &'static str> {
        let processed = self
            .soxr
            .process(input, output)
            .map_err(|_| "resampler backend process failed")?;
        Ok(BackendProgress {
            input_frames: processed.input_frames,
            output_frames: processed.output_frames,
        })
    }

    pub(super) fn drain(&mut self, output: &mut [f64]) -> Result<usize, &'static str> {
        self.soxr
            .drain(output)
            .map_err(|_| "resampler backend drain failed")
    }

    pub(super) fn clear(&mut self) -> Result<(), &'static str> {
        self.soxr
            .clear()
            .map_err(|_| "resampler backend clear failed")
    }

    pub(super) fn latency_frames(&self) -> usize {
        0
    }

    pub(super) fn finish_extension_frames(&self) -> usize {
        0
    }
}
