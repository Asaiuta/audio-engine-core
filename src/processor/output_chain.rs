//! Canonical output render-chain definition and builders.
//!
//! Setup code may allocate while it materializes processors. The realtime
//! invariant applies to the returned [`DspChain::process`] path.

use std::sync::{atomic::AtomicBool, Arc};

use arc_swap::ArcSwapOption;

use crate::config::{PhaseResponse, ResampleQuality};

use super::adapters::{
    ConvolverProcessor, CrossfeedProcessor, DynamicLoudnessProcessor, EqProcessor,
    NoiseShaperProcessor, PeakLimiterProcessor, SaturationProcessor, VolumeProcessor,
};
use super::convolver::FFTConvolver;
use super::dsp_chain::DspChain;
use super::lockfree_params::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry,
    AtomicEqParams, AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams,
    AtomicVolumeParams,
};
use super::resampler::StreamingResampler;
use super::traits::AudioProcessor;

/// Canonical stage identifiers for the output chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputStageId {
    Volume,
    Equalizer,
    Saturation,
    Crossfeed,
    Convolver,
    DynamicLoudness,
    PeakLimiter,
    Resampler,
    NoiseShaper,
    Quantize,
    Meter,
}

/// Static metadata for one output-chain stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputStageDescriptor {
    pub id: OutputStageId,
    pub name: &'static str,
    pub callback_stage: bool,
    pub offline_stage: bool,
    pub carries_state: bool,
    pub introduces_latency: bool,
    pub latency_note: &'static str,
}

const OUTPUT_STAGE_DESCRIPTORS: [OutputStageDescriptor; 11] = [
    OutputStageDescriptor {
        id: OutputStageId::Volume,
        name: "Volume",
        callback_stage: true,
        offline_stage: true,
        carries_state: true,
        introduces_latency: false,
        latency_note: "gain smoothing state, no algorithmic delay",
    },
    OutputStageDescriptor {
        id: OutputStageId::Equalizer,
        name: "Equalizer",
        callback_stage: true,
        offline_stage: true,
        carries_state: true,
        introduces_latency: false,
        latency_note: "IIR state, no explicit output delay",
    },
    OutputStageDescriptor {
        id: OutputStageId::Saturation,
        name: "Saturation",
        callback_stage: true,
        offline_stage: true,
        carries_state: true,
        introduces_latency: false,
        latency_note: "optional high-pass state, no explicit output delay",
    },
    OutputStageDescriptor {
        id: OutputStageId::Crossfeed,
        name: "Crossfeed",
        callback_stage: true,
        offline_stage: true,
        carries_state: true,
        introduces_latency: false,
        latency_note: "biquad state, no explicit output delay",
    },
    OutputStageDescriptor {
        id: OutputStageId::Convolver,
        name: "Convolver",
        callback_stage: true,
        offline_stage: true,
        carries_state: true,
        introduces_latency: true,
        latency_note: "IR- and partition-size-dependent convolution latency",
    },
    OutputStageDescriptor {
        id: OutputStageId::DynamicLoudness,
        name: "DynamicLoudness",
        callback_stage: true,
        offline_stage: true,
        carries_state: true,
        introduces_latency: false,
        latency_note: "filter and smoother state, no explicit output delay",
    },
    OutputStageDescriptor {
        id: OutputStageId::PeakLimiter,
        name: "PeakLimiter",
        callback_stage: true,
        offline_stage: true,
        carries_state: true,
        introduces_latency: true,
        latency_note: "lookahead delay depends on sample rate and limiter mode",
    },
    OutputStageDescriptor {
        id: OutputStageId::Resampler,
        name: "Resampler",
        callback_stage: false,
        offline_stage: true,
        carries_state: true,
        introduces_latency: true,
        latency_note: "SoX filter latency; playback callback receives already-resampled buffers",
    },
    OutputStageDescriptor {
        id: OutputStageId::NoiseShaper,
        name: "NoiseShaper",
        callback_stage: true,
        offline_stage: true,
        carries_state: true,
        introduces_latency: false,
        latency_note: "error-feedback state, no explicit output delay",
    },
    OutputStageDescriptor {
        id: OutputStageId::Quantize,
        name: "Quantize",
        callback_stage: false,
        offline_stage: true,
        carries_state: false,
        introduces_latency: false,
        latency_note: "final sample-format reduction",
    },
    OutputStageDescriptor {
        id: OutputStageId::Meter,
        name: "Meter",
        callback_stage: false,
        offline_stage: true,
        carries_state: true,
        introduces_latency: false,
        latency_note: "analysis-only accumulation; not a signal-transform stage",
    },
];

/// Full canonical output-chain order.
pub fn canonical_output_stage_descriptors() -> &'static [OutputStageDescriptor] {
    &OUTPUT_STAGE_DESCRIPTORS
}

/// Callback-safe stage names in canonical order.
pub fn callback_stage_names() -> Vec<&'static str> {
    OUTPUT_STAGE_DESCRIPTORS
        .iter()
        .filter(|stage| stage.callback_stage)
        .map(|stage| stage.name)
        .collect()
}

/// Offline render-stage names in canonical order.
pub fn offline_stage_names() -> Vec<&'static str> {
    OUTPUT_STAGE_DESCRIPTORS
        .iter()
        .filter(|stage| stage.offline_stage)
        .map(|stage| stage.name)
        .collect()
}

/// CSV snapshot of callback-safe stage names, for benchmark output.
pub fn callback_stage_order_csv() -> String {
    callback_stage_names().join(",")
}

/// CSV snapshot of offline render-stage names, for reports.
pub fn offline_stage_order_csv() -> String {
    offline_stage_names().join(",")
}

/// All parameter and control handles required to materialize an output chain.
#[derive(Clone)]
pub struct OutputChainParams {
    pub channels: usize,
    pub source_sample_rate: u32,
    pub output_sample_rate: u32,
    pub eq_params: Arc<AtomicEqParams>,
    pub saturation_params: Arc<AtomicSaturationParams>,
    pub crossfeed_params: Arc<AtomicCrossfeedParams>,
    pub convolver_swap: Arc<ArcSwapOption<FFTConvolver>>,
    pub convolver_enabled: Arc<AtomicBool>,
    pub volume_params: Arc<AtomicVolumeParams>,
    pub dynamic_loudness_params: Arc<AtomicDynamicLoudnessParams>,
    pub dynamic_loudness_telemetry: Arc<AtomicDynamicLoudnessTelemetry>,
    pub limiter_params: Arc<AtomicPeakLimiterParams>,
    pub noise_shaper_params: Arc<AtomicNoiseShaperParams>,
}

/// Builder for realtime and offline output chains.
#[derive(Clone)]
pub struct OutputChainBuilder {
    params: OutputChainParams,
}

impl OutputChainBuilder {
    pub fn new(params: OutputChainParams) -> Self {
        Self { params }
    }

    /// Build the callback-safe DSP chain from the canonical callback order.
    pub fn build_callback_chain(&self) -> DspChain {
        let sample_rate = self.params.source_sample_rate as f64;
        let mut chain = DspChain::with_capacity(callback_stage_names().len(), sample_rate);

        chain
            .add(VolumeProcessor::new(Arc::clone(&self.params.volume_params)))
            .add(EqProcessor::new(
                self.params.channels,
                sample_rate,
                Arc::clone(&self.params.eq_params),
            ))
            .add(SaturationProcessor::new(
                self.params.channels,
                Arc::clone(&self.params.saturation_params),
            ))
            .add(CrossfeedProcessor::new(
                sample_rate,
                Arc::clone(&self.params.crossfeed_params),
            ))
            .add(ConvolverProcessor::new(
                Arc::clone(&self.params.convolver_swap),
                Arc::clone(&self.params.convolver_enabled),
            ))
            .add(DynamicLoudnessProcessor::new(
                self.params.channels,
                self.params.source_sample_rate,
                Arc::clone(&self.params.dynamic_loudness_params),
                Arc::clone(&self.params.dynamic_loudness_telemetry),
            ))
            .add(PeakLimiterProcessor::new(
                self.params.channels,
                self.params.source_sample_rate,
                Arc::clone(&self.params.limiter_params),
            ))
            .add(NoiseShaperProcessor::new(
                self.params.channels,
                self.params.source_sample_rate,
                Arc::clone(&self.params.noise_shaper_params),
            ));

        chain.set_sample_rate(sample_rate);
        chain
    }

    /// Build the offline render chain from the canonical offline order.
    pub fn build_render_chain(&self) -> Result<OutputRenderChain, String> {
        OutputRenderChain::new(&self.params)
    }
}

/// Render result after the full offline output chain.
pub struct RenderedOutput {
    pub samples: Vec<f64>,
    pub final_limiter_gain_reduction_db: f64,
}

/// Offline output renderer. It shares the canonical stage order with the
/// realtime builder, and inserts the resampler before final noise shaping when
/// the render target rate differs from the source rate.
pub struct OutputRenderChain {
    channels: usize,
    source_sample_rate: u32,
    output_sample_rate: u32,
    volume: VolumeProcessor,
    eq: EqProcessor,
    saturation: SaturationProcessor,
    crossfeed: CrossfeedProcessor,
    convolver: ConvolverProcessor,
    dynamic_loudness: DynamicLoudnessProcessor,
    limiter: PeakLimiterProcessor,
    resampler: Option<StreamingResampler>,
    noise_shaper: NoiseShaperProcessor,
}

impl OutputRenderChain {
    fn new(params: &OutputChainParams) -> Result<Self, String> {
        let source_sample_rate = params.source_sample_rate as f64;
        let output_sample_rate = params.output_sample_rate as f64;
        let resampler = if params.source_sample_rate == params.output_sample_rate {
            None
        } else {
            Some(
                StreamingResampler::with_quality(
                    params.channels,
                    params.source_sample_rate,
                    params.output_sample_rate,
                    PhaseResponse::Linear,
                    ResampleQuality::UltraHigh,
                )
                .map_err(|err| {
                    format!(
                        "failed to create output-chain resampler {}->{}: {err}",
                        params.source_sample_rate, params.output_sample_rate
                    )
                })?,
            )
        };

        let mut chain = Self {
            channels: params.channels,
            source_sample_rate: params.source_sample_rate,
            output_sample_rate: params.output_sample_rate,
            volume: VolumeProcessor::new(Arc::clone(&params.volume_params)),
            eq: EqProcessor::new(
                params.channels,
                source_sample_rate,
                Arc::clone(&params.eq_params),
            ),
            saturation: SaturationProcessor::new(
                params.channels,
                Arc::clone(&params.saturation_params),
            ),
            crossfeed: CrossfeedProcessor::new(
                source_sample_rate,
                Arc::clone(&params.crossfeed_params),
            ),
            convolver: ConvolverProcessor::new(
                Arc::clone(&params.convolver_swap),
                Arc::clone(&params.convolver_enabled),
            ),
            dynamic_loudness: DynamicLoudnessProcessor::new(
                params.channels,
                params.source_sample_rate,
                Arc::clone(&params.dynamic_loudness_params),
                Arc::clone(&params.dynamic_loudness_telemetry),
            ),
            limiter: PeakLimiterProcessor::new(
                params.channels,
                params.source_sample_rate,
                Arc::clone(&params.limiter_params),
            ),
            resampler,
            noise_shaper: NoiseShaperProcessor::new(
                params.channels,
                params.output_sample_rate,
                Arc::clone(&params.noise_shaper_params),
            ),
        };

        chain.set_source_sample_rate(source_sample_rate);
        chain.noise_shaper.set_sample_rate(output_sample_rate);
        Ok(chain)
    }

    /// Process through the offline output chain up to final sample-format
    /// quantization. This is useful for parity tests against the callback chain
    /// when `source_sample_rate == output_sample_rate`.
    pub fn process_pre_quantize(&mut self, buffer: &mut Vec<f64>) {
        self.volume.process(buffer, self.channels);
        self.eq.process(buffer, self.channels);
        self.saturation.process(buffer, self.channels);
        self.crossfeed.process(buffer, self.channels);
        self.convolver.process(buffer, self.channels);
        self.dynamic_loudness.process(buffer, self.channels);
        self.limiter.process(buffer, self.channels);

        if let Some(resampler) = self.resampler.as_mut() {
            let input_frames = buffer.len() / self.channels;
            let estimated_samples = ((input_frames as f64 * self.output_sample_rate as f64
                / self.source_sample_rate as f64)
                .ceil() as usize)
                .saturating_add(256)
                * self.channels;
            let mut resampled = Vec::with_capacity(estimated_samples);

            for chunk in buffer.chunks(self.channels * 4096) {
                resampler.process_chunk_append(chunk, &mut resampled);
            }
            resampler.flush_into(&mut resampled);
            *buffer = resampled;
        }

        self.noise_shaper.process(buffer, self.channels);
    }

    /// Render samples through DSP, optional resampling, final noise shaping, and
    /// f32 output quantization.
    pub fn render(&mut self, samples: &[f64]) -> RenderedOutput {
        let mut output = samples.to_vec();
        self.process_pre_quantize(&mut output);

        for sample in &mut output {
            *sample = *sample as f32 as f64;
        }

        RenderedOutput {
            samples: output,
            final_limiter_gain_reduction_db: self.limiter.gain_reduction_db(),
        }
    }

    pub fn limiter_gain_reduction_db(&self) -> f64 {
        self.limiter.gain_reduction_db()
    }

    fn set_source_sample_rate(&mut self, sample_rate: f64) {
        self.volume.set_sample_rate(sample_rate);
        self.eq.set_sample_rate(sample_rate);
        self.saturation.set_sample_rate(sample_rate);
        self.crossfeed.set_sample_rate(sample_rate);
        self.dynamic_loudness.set_sample_rate(sample_rate);
        self.limiter.set_sample_rate(sample_rate);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::processor::{NoiseShaperCurve, SaturationTypeValue, EQ_BANDS};

    const CHANNELS: usize = 2;
    const SAMPLE_RATE: u32 = 48_000;

    #[test]
    fn callback_builder_order_matches_canonical_stage_list() {
        let builder = test_builder();
        let chain = builder.build_callback_chain();

        assert_callback_stage_order_matches(&chain.processor_names());
    }

    #[test]
    fn callback_runtime_order_matches_offline_shared_stage_intersection() {
        let builder = test_builder();
        let chain = builder.build_callback_chain();
        let shared_offline_stage_names = canonical_output_stage_descriptors()
            .iter()
            .filter(|stage| stage.offline_stage && stage.callback_stage)
            .map(|stage| stage.name)
            .collect::<Vec<_>>();

        assert_eq!(chain.processor_names(), shared_offline_stage_names);
    }

    #[test]
    fn callback_stage_order_assertion_rejects_reordered_chain() {
        let mut reordered = callback_stage_names();
        reordered.swap(0, 1);

        let result = std::panic::catch_unwind(|| {
            assert_callback_stage_order_matches(&reordered);
        });

        assert!(
            result.is_err(),
            "deliberately reordered callback chain must fail parity assertion"
        );
    }

    #[test]
    fn offline_stage_order_preserves_render_only_nodes() {
        assert_eq!(
            offline_stage_names(),
            vec![
                "Volume",
                "Equalizer",
                "Saturation",
                "Crossfeed",
                "Convolver",
                "DynamicLoudness",
                "PeakLimiter",
                "Resampler",
                "NoiseShaper",
                "Quantize",
                "Meter",
            ]
        );
    }

    #[test]
    fn stage_metadata_marks_stateful_and_latency_stages() {
        let limiter = descriptor(OutputStageId::PeakLimiter);
        assert!(limiter.carries_state);
        assert!(limiter.introduces_latency);

        let quantize = descriptor(OutputStageId::Quantize);
        assert!(!quantize.carries_state);
        assert!(!quantize.introduces_latency);
    }

    #[test]
    fn render_chain_matches_callback_chain_pre_quantize_when_no_resampler() {
        let builder = active_test_builder();
        let mut callback_chain = builder.build_callback_chain();
        let mut render_chain = builder.build_render_chain().unwrap();

        let input = fixture_signal(512);
        let mut callback = input.clone();
        let mut rendered = input;

        callback_chain.process(&mut callback, CHANNELS);
        render_chain.process_pre_quantize(&mut rendered);

        assert_eq!(callback.len(), rendered.len());
        for (idx, (left, right)) in callback.iter().zip(&rendered).enumerate() {
            assert_eq!(
                left.to_bits(),
                right.to_bits(),
                "sample {idx} diverged: callback={left:?} render={right:?}"
            );
        }
    }

    #[test]
    fn callback_chain_processing_is_allocation_free_after_setup() {
        let builder = active_test_builder();
        let mut chain = builder.build_callback_chain();
        let mut buffer = fixture_signal(256);

        chain.process(&mut buffer, CHANNELS);

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..32 {
                chain.process(&mut buffer, CHANNELS);
            }
        });
    }

    fn descriptor(id: OutputStageId) -> OutputStageDescriptor {
        canonical_output_stage_descriptors()
            .iter()
            .copied()
            .find(|stage| stage.id == id)
            .expect("descriptor exists")
    }

    fn assert_callback_stage_order_matches(observed: &[&'static str]) {
        assert_eq!(
            observed,
            callback_stage_names().as_slice(),
            "callback stage order diverged from canonical output chain"
        );
    }

    fn test_builder() -> OutputChainBuilder {
        OutputChainBuilder::new(test_params())
    }

    fn active_test_builder() -> OutputChainBuilder {
        let params = test_params();
        params.eq_params.write(&[0.0; EQ_BANDS], false);
        params.saturation_params.set_enabled(true);
        params.saturation_params.set_drive(0.4);
        params.saturation_params.set_mix(0.25);
        params
            .saturation_params
            .set_sat_type(SaturationTypeValue::Tube);
        params.saturation_params.set_highpass_mode(true);
        params.crossfeed_params.set_enabled(true);
        params.crossfeed_params.set_mix(0.25);
        params.volume_params.set_volume(0.8);
        params.dynamic_loudness_params.set_enabled(false);
        params.limiter_params.set_enabled(true);
        params.limiter_params.set_threshold(-1.0);
        params.noise_shaper_params.set_enabled(true);
        params.noise_shaper_params.set_bits(24);
        params
            .noise_shaper_params
            .set_curve(NoiseShaperCurve::TpdfOnly);
        OutputChainBuilder::new(params)
    }

    fn test_params() -> OutputChainParams {
        OutputChainParams {
            channels: CHANNELS,
            source_sample_rate: SAMPLE_RATE,
            output_sample_rate: SAMPLE_RATE,
            eq_params: Arc::new(AtomicEqParams::new()),
            saturation_params: Arc::new(AtomicSaturationParams::new()),
            crossfeed_params: Arc::new(AtomicCrossfeedParams::new()),
            convolver_swap: Arc::new(ArcSwapOption::empty()),
            convolver_enabled: Arc::new(AtomicBool::new(false)),
            volume_params: Arc::new(AtomicVolumeParams::new()),
            dynamic_loudness_params: Arc::new(AtomicDynamicLoudnessParams::new()),
            dynamic_loudness_telemetry: Arc::new(AtomicDynamicLoudnessTelemetry::new()),
            limiter_params: Arc::new(AtomicPeakLimiterParams::new()),
            noise_shaper_params: Arc::new(AtomicNoiseShaperParams::new()),
        }
    }

    fn fixture_signal(frames: usize) -> Vec<f64> {
        let mut out = Vec::with_capacity(frames * CHANNELS);
        for frame in 0..frames {
            let t = frame as f64 / SAMPLE_RATE as f64;
            let left = (std::f64::consts::TAU * 997.0 * t).sin() * 0.25;
            let right = (std::f64::consts::TAU * 1201.0 * t).sin() * 0.20;
            out.push(left);
            out.push(right);
        }
        out
    }
}
