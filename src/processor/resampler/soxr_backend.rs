//! Native SoXR (libsoxr) resampler backend.
//!
//! Stereo streams use one native interleaved `Soxr<Stereo<f64>>` instance so
//! caller-owned blocks reach libsoxr without channel staging. Other channel
//! layouts retain one `Soxr<Mono<f64>>` stream per channel. SoXR natively
//! provides the semantics the shared resampler contract requires: arbitrary
//! input chunk sizes, duration-aligned output with no leading delay frames,
//! `drain` as the only end-of-stream operation, and `clear` restoring the
//! initial state.

use crate::config::{PhaseResponse, ResampleQuality};
use soxr::{
    format::{Mono, Stereo},
    params::{QualityFlags, QualityRecipe, QualitySpec, Rolloff, RuntimeSpec},
    Soxr,
};

use super::{BackendInitError, BackendProcessError, BackendProgress};

pub(super) const BACKEND_NAME: &str = "soxr";

/// Resolve each public quality tier to a distinct SoXR recipe.
fn quality_to_recipe(quality: ResampleQuality) -> QualityRecipe {
    match quality {
        ResampleQuality::Low => QualityRecipe::Low,
        ResampleQuality::Standard => QualityRecipe::Medium,
        ResampleQuality::High => QualityRecipe::high(),
        ResampleQuality::UltraHigh => QualityRecipe::very_high(),
    }
}

/// Create a QualitySpec with the given recipe and phase response
fn make_quality_spec(recipe: QualityRecipe, phase: PhaseResponse) -> QualitySpec {
    QualitySpec::configure(recipe, Rolloff::default(), QualityFlags::HighPrecisionClock)
        .with_phase_response(phase.to_soxr_value())
}

enum NativeBackend {
    Mono(Soxr<Mono<f64>>),
    Stereo(Soxr<Stereo<f64>>),
}

pub(super) struct MonoBackend {
    soxr: NativeBackend,
}

impl MonoBackend {
    pub(super) fn new(
        from_rate: u32,
        to_rate: u32,
        phase: PhaseResponse,
        quality: ResampleQuality,
    ) -> Result<Self, BackendInitError> {
        let quality_spec = make_quality_spec(quality_to_recipe(quality), phase);
        let runtime_spec = RuntimeSpec::new(1);
        Soxr::<Mono<f64>>::new_with_params(
            from_rate as f64,
            to_rate as f64,
            quality_spec,
            runtime_spec,
        )
        .map(|soxr| Self {
            soxr: NativeBackend::Mono(soxr),
        })
        .map_err(|error| BackendInitError::Backend {
            message: format!("{error:?}"),
        })
    }

    pub(super) fn new_interleaved_stereo(
        from_rate: u32,
        to_rate: u32,
        phase: PhaseResponse,
        quality: ResampleQuality,
    ) -> Result<Self, BackendInitError> {
        let quality_spec = make_quality_spec(quality_to_recipe(quality), phase);
        let runtime_spec = RuntimeSpec::new(1);
        Soxr::<Stereo<f64>>::new_with_params(
            from_rate as f64,
            to_rate as f64,
            quality_spec,
            runtime_spec,
        )
        .map(|soxr| Self {
            soxr: NativeBackend::Stereo(soxr),
        })
        .map_err(|error| BackendInitError::Backend {
            message: format!("{error:?}"),
        })
    }

    pub(super) fn is_interleaved_stereo(&self) -> bool {
        matches!(self.soxr, NativeBackend::Stereo(_))
    }

    pub(super) fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<BackendProgress, BackendProcessError> {
        let NativeBackend::Mono(soxr) = &mut self.soxr else {
            return Err("mono resampler entry received interleaved backend".into());
        };
        let processed = soxr
            .process(input, output)
            .map_err(|_| BackendProcessError::new("resampler backend process failed"))?;
        Ok(BackendProgress {
            input_frames: processed.input_frames,
            output_frames: processed.output_frames,
        })
    }

    pub(super) fn process_interleaved_stereo(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<BackendProgress, BackendProcessError> {
        let NativeBackend::Stereo(soxr) = &mut self.soxr else {
            return Err("interleaved resampler entry received mono backend".into());
        };
        let input = stereo_frames(input)?;
        let output = stereo_frames_mut(output)?;
        let processed = soxr
            .process(input, output)
            .map_err(|_| BackendProcessError::new("resampler backend process failed"))?;
        Ok(BackendProgress {
            input_frames: processed.input_frames,
            output_frames: processed.output_frames,
        })
    }

    pub(super) fn drain(&mut self, output: &mut [f64]) -> Result<usize, BackendProcessError> {
        let NativeBackend::Mono(soxr) = &mut self.soxr else {
            return Err("mono resampler drain received interleaved backend".into());
        };
        soxr.drain(output)
            .map_err(|_| BackendProcessError::new("resampler backend drain failed"))
    }

    pub(super) fn drain_interleaved_stereo(
        &mut self,
        output: &mut [f64],
    ) -> Result<usize, BackendProcessError> {
        let NativeBackend::Stereo(soxr) = &mut self.soxr else {
            return Err("interleaved resampler drain received mono backend".into());
        };
        soxr.drain(stereo_frames_mut(output)?)
            .map_err(|_| BackendProcessError::new("resampler backend drain failed"))
    }

    pub(super) fn clear(&mut self) -> Result<(), BackendProcessError> {
        match &mut self.soxr {
            NativeBackend::Mono(soxr) => soxr.clear(),
            NativeBackend::Stereo(soxr) => soxr.clear(),
        }
        .map_err(|_| BackendProcessError::new("resampler backend clear failed"))
    }

    pub(super) fn latency_frames(&self) -> usize {
        0
    }

    pub(super) fn finish_extension_frames(&self) -> usize {
        0
    }
}

fn stereo_frames(samples: &[f64]) -> Result<&[[f64; 2]], BackendProcessError> {
    if !samples.len().is_multiple_of(2) {
        return Err("resampler backend received an incomplete stereo frame".into());
    }
    // SAFETY: `[f64; 2]` has the same alignment and contiguous layout as two
    // adjacent f64 values, and the sample count was validated as even.
    Ok(unsafe { std::slice::from_raw_parts(samples.as_ptr().cast(), samples.len() / 2) })
}

fn stereo_frames_mut(samples: &mut [f64]) -> Result<&mut [[f64; 2]], BackendProcessError> {
    if !samples.len().is_multiple_of(2) {
        return Err("resampler backend received an incomplete stereo frame".into());
    }
    // SAFETY: the mutable slice is uniquely borrowed; `[f64; 2]` has the same
    // alignment/layout as two adjacent f64 values, and its length is even.
    Ok(unsafe { std::slice::from_raw_parts_mut(samples.as_mut_ptr().cast(), samples.len() / 2) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_public_quality_tier_resolves_to_a_distinct_recipe() {
        let recipes = [
            quality_to_recipe(ResampleQuality::Low),
            quality_to_recipe(ResampleQuality::Standard),
            quality_to_recipe(ResampleQuality::High),
            quality_to_recipe(ResampleQuality::UltraHigh),
        ];

        assert_eq!(recipes[0], QualityRecipe::Low);
        assert_eq!(recipes[1], QualityRecipe::Medium);
        assert_eq!(recipes[2], QualityRecipe::Bits20);
        assert_eq!(recipes[3], QualityRecipe::Bits28);
        for (index, recipe) in recipes.iter().enumerate() {
            assert!(!recipes[..index].contains(recipe));
        }
    }
}
