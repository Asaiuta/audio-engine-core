//! Canonical output render-chain definition and builders.
//!
//! Setup code may allocate while it materializes processors. The realtime
//! invariant applies to the returned [`DspChain::process`] path.

use std::sync::Arc;

use crate::config::{PhaseResponse, ResampleQuality};

use super::adapters::{
    ConvolverControl, ConvolverProcessor, CrossfeedProcessor, DynamicLoudnessProcessor,
    EqProcessor, NoiseShaperProcessor, PeakLimiterProcessor, SaturationProcessor, VolumeProcessor,
};
use super::dsp_chain::DspChain;
use super::lockfree_params::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry,
    AtomicEqParams, AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams,
    AtomicVolumeParams,
};
use super::resampler::StreamingResampler;
use super::traits::{
    finish_checked, process_checked, AudioBlockMut, AudioBlockRef, FrameDuration, FrameRounding,
    ProcessBuffers, ProcessError, ProcessProgress, ProcessState, StreamingProcessor, TailSpec,
    TimingError,
};

const DEFAULT_OFFLINE_BLOCK_FRAMES: usize = 4_096;

fn process_fixed_stage<P: StreamingProcessor + ?Sized>(
    processor: &mut P,
    buffer: &mut [f64],
    channels: usize,
) -> Result<ProcessProgress, ProcessError> {
    let block = AudioBlockMut::new(buffer, channels)?;
    process_checked(processor, ProcessBuffers::in_place(block))
}

/// Timeline treatment applied after a complete offline finalize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderTimeline {
    /// Remove accumulated algorithmic latency while preserving semantic tails.
    #[default]
    Compensated,
    /// Retain the causal leading delay and all finalize output.
    RawCausal,
}

/// Termination policy for processors whose semantic tail is not finite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnknownTailPolicy {
    /// Per-frame RMS energy threshold in dBFS, measured before noise shaping.
    pub energy_threshold_dbfs: f64,
    /// Required continuous below-threshold duration.
    pub silence_hold_ms: u32,
    /// Hard upper bound on generated unknown tail duration.
    pub max_tail_ms: u32,
}

impl Default for UnknownTailPolicy {
    fn default() -> Self {
        Self {
            energy_threshold_dbfs: -120.0,
            silence_hold_ms: 250,
            max_tail_ms: 30_000,
        }
    }
}

/// Policy for complete offline rendering.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OfflineRenderPolicy {
    pub timeline: RenderTimeline,
    pub unknown_tail: UnknownTailPolicy,
}

impl OfflineRenderPolicy {
    pub fn raw_causal() -> Self {
        Self {
            timeline: RenderTimeline::RawCausal,
            ..Self::default()
        }
    }

    fn validate(self) -> Result<Self, ProcessError> {
        if !self.unknown_tail.energy_threshold_dbfs.is_finite()
            || self.unknown_tail.energy_threshold_dbfs > 0.0
        {
            return Err(ProcessError::InvalidRenderPolicy {
                message: "energy threshold must be finite and no greater than 0 dBFS",
            });
        }
        if self.unknown_tail.silence_hold_ms == 0 {
            return Err(ProcessError::InvalidRenderPolicy {
                message: "silence hold must be greater than zero",
            });
        }
        if self.unknown_tail.max_tail_ms < self.unknown_tail.silence_hold_ms {
            return Err(ProcessError::InvalidRenderPolicy {
                message: "maximum tail must be at least the silence hold duration",
            });
        }
        Ok(self)
    }
}

struct OfflineStageOutput {
    samples: Vec<f64>,
    output_sample_rate_hz: u32,
    latency: FrameDuration,
    tail: TailSpec,
    unknown_finish_capped: bool,
}

#[derive(Default)]
struct RenderTiming {
    latencies: Vec<FrameDuration>,
    finite_tails: Vec<FrameDuration>,
    has_unknown_tail: bool,
    unknown_finish_capped: bool,
}

impl RenderTiming {
    fn observe(&mut self, stage: &OfflineStageOutput) {
        self.latencies.push(stage.latency);
        match stage.tail {
            TailSpec::None => {}
            TailSpec::Finite(duration) => self.finite_tails.push(duration),
            TailSpec::Unknown | TailSpec::Infinite => self.has_unknown_tail = true,
        }
        self.unknown_finish_capped |= stage.unknown_finish_capped;
    }

    fn latency_frames(&self, sample_rate_hz: u32) -> Result<usize, ProcessError> {
        rounded_duration_sum(&self.latencies, sample_rate_hz, FrameRounding::Nearest)
            .map_err(Into::into)
    }

    fn finite_tail_frames(&self, sample_rate_hz: u32) -> Result<usize, ProcessError> {
        rounded_duration_sum(&self.finite_tails, sample_rate_hz, FrameRounding::Ceil)
            .map_err(Into::into)
    }
}

fn rounded_duration_sum(
    durations: &[FrameDuration],
    sample_rate_hz: u32,
    rounding: FrameRounding,
) -> Result<usize, TimingError> {
    let mut total = 0.0;
    for duration in durations {
        total += duration.frames_at_rate_f64(sample_rate_hz)?;
    }
    let rounded = match rounding {
        FrameRounding::Floor => total.floor(),
        FrameRounding::Nearest => (total + 0.5).floor(),
        FrameRounding::Ceil => total.ceil(),
    };
    if !rounded.is_finite() || rounded < 0.0 || rounded > usize::MAX as f64 {
        return Err(TimingError::FrameCountOverflow);
    }
    Ok(rounded as usize)
}

fn checked_frame_sum(left: usize, right: usize) -> Result<usize, TimingError> {
    left.checked_add(right)
        .ok_or(TimingError::FrameCountOverflow)
}

fn frames_for_milliseconds(milliseconds: u32, sample_rate_hz: u32) -> Result<usize, TimingError> {
    if sample_rate_hz == 0 {
        return Err(TimingError::ZeroSampleRate);
    }
    let numerator = milliseconds as u128 * sample_rate_hz as u128;
    usize::try_from(numerator.div_ceil(1_000)).map_err(|_| TimingError::FrameCountOverflow)
}

struct TailEnergyDetector {
    energy_threshold: f64,
    hold_frames: usize,
    below_frames: usize,
    silence_start_frame: usize,
}

impl TailEnergyDetector {
    fn new(policy: UnknownTailPolicy, sample_rate_hz: u32) -> Result<Self, TimingError> {
        let amplitude_threshold = 10.0_f64.powf(policy.energy_threshold_dbfs / 20.0);
        Ok(Self {
            energy_threshold: amplitude_threshold * amplitude_threshold,
            hold_frames: frames_for_milliseconds(policy.silence_hold_ms, sample_rate_hz)?.max(1),
            below_frames: 0,
            silence_start_frame: 0,
        })
    }

    fn observe(&mut self, samples: &[f64], channels: usize, first_frame: usize) -> Option<usize> {
        for (offset, frame) in samples.chunks_exact(channels).enumerate() {
            let frame_energy =
                frame.iter().map(|sample| sample * sample).sum::<f64>() / channels as f64;
            if frame_energy <= self.energy_threshold {
                if self.below_frames == 0 {
                    self.silence_start_frame = first_frame + offset;
                }
                self.below_frames += 1;
                if self.below_frames >= self.hold_frames {
                    return Some(self.silence_start_frame);
                }
            } else {
                self.below_frames = 0;
            }
        }
        None
    }
}

fn finish_frame_limit(
    input_frames: usize,
    input_sample_rate_hz: u32,
    output_sample_rate_hz: u32,
    latency: FrameDuration,
    tail: TailSpec,
    policy: OfflineRenderPolicy,
    block_frames: usize,
) -> Result<usize, ProcessError> {
    let converted_input = FrameDuration::new(input_frames, input_sample_rate_hz)?
        .rounded_frames_at_rate(output_sample_rate_hz, FrameRounding::Ceil)?;
    let latency_frames =
        latency.rounded_frames_at_rate(output_sample_rate_hz, FrameRounding::Ceil)?;
    let finite_tail_frames = tail
        .finite_duration()
        .map(|duration| duration.rounded_frames_at_rate(output_sample_rate_hz, FrameRounding::Ceil))
        .transpose()?
        .unwrap_or(0);

    let declared = checked_frame_sum(latency_frames, finite_tail_frames)?;
    let limit = match tail {
        TailSpec::Unknown | TailSpec::Infinite => checked_frame_sum(
            frames_for_milliseconds(policy.unknown_tail.max_tail_ms, output_sample_rate_hz)?,
            latency_frames,
        )?,
        TailSpec::None | TailSpec::Finite(_) => {
            checked_frame_sum(checked_frame_sum(converted_input, declared)?, block_frames)?
        }
    };
    Ok(limit.max(1))
}

fn drive_offline_stage(
    processor: &mut dyn StreamingProcessor,
    input: &[f64],
    channels: usize,
    input_sample_rate_hz: u32,
    policy: OfflineRenderPolicy,
    block_frames: usize,
) -> Result<OfflineStageOutput, ProcessError> {
    if block_frames == 0 {
        return Err(ProcessError::InvalidRenderPolicy {
            message: "offline block size must be greater than zero",
        });
    }

    let input_block = AudioBlockRef::new(input, channels)?;
    processor.set_sample_rate(input_sample_rate_hz)?;
    let output_sample_rate_hz = processor.output_sample_rate_hz(input_sample_rate_hz)?;
    let scratch_samples = block_frames
        .checked_mul(channels)
        .ok_or(TimingError::FrameCountOverflow)?;
    let mut scratch = vec![0.0; scratch_samples];
    let mut samples = Vec::with_capacity(input.len());
    let mut consumed_frames = 0;

    while consumed_frames < input_block.frames() {
        let input_start = consumed_frames * channels;
        let input_view = AudioBlockRef::new(&input[input_start..], channels)?;
        let output_view = AudioBlockMut::new(&mut scratch, channels)?;
        let buffers = ProcessBuffers::out_of_place(input_view, output_view)?;
        let progress = process_checked(processor, buffers)?;
        let produced_samples = progress.produced_frames() * channels;
        samples.extend_from_slice(&scratch[..produced_samples]);
        consumed_frames += progress.consumed_frames();
    }

    let latency = processor.latency();
    let tail = processor.tail();
    let unknown_tail = matches!(tail, TailSpec::Unknown | TailSpec::Infinite);
    let finish_limit = finish_frame_limit(
        input_block.frames(),
        input_sample_rate_hz,
        output_sample_rate_hz,
        latency,
        tail,
        policy,
        block_frames,
    )?;
    let protected_finish_frames = if unknown_tail {
        latency.rounded_frames_at_rate(output_sample_rate_hz, FrameRounding::Ceil)?
    } else {
        0
    };
    let mut tail_detector = if unknown_tail {
        Some(TailEnergyDetector::new(
            policy.unknown_tail,
            output_sample_rate_hz,
        )?)
    } else {
        None
    };
    let mut finish_frames = 0;
    let mut terminal = false;
    let mut energy_stopped = false;

    while finish_frames < finish_limit {
        let capacity_frames = block_frames.min(finish_limit - finish_frames);
        let output_view = AudioBlockMut::new(&mut scratch[..capacity_frames * channels], channels)?;
        let progress = finish_checked(processor, output_view)?;
        let produced_frames = progress.produced_frames();
        let produced_samples = produced_frames * channels;
        let appended_start_frame = samples.len() / channels;
        samples.extend_from_slice(&scratch[..produced_samples]);
        if let Some(detector) = tail_detector.as_mut() {
            let inspect_start = protected_finish_frames
                .saturating_sub(finish_frames)
                .min(produced_frames);
            if inspect_start < produced_frames {
                let inspect_samples = &scratch[inspect_start * channels..produced_samples];
                if let Some(silence_start_frame) = detector.observe(
                    inspect_samples,
                    channels,
                    appended_start_frame + inspect_start,
                ) {
                    samples.truncate(silence_start_frame * channels);
                    energy_stopped = true;
                }
            }
        }
        finish_frames += produced_frames;
        if progress.state() == ProcessState::Finished {
            terminal = true;
        }
        if terminal || energy_stopped {
            break;
        }
    }

    if !terminal && !unknown_tail {
        return Err(ProcessError::Backend {
            processor: processor.name(),
            operation: "finish",
            message: "finite finish exceeded its declared safety bound",
        });
    }

    Ok(OfflineStageOutput {
        samples,
        output_sample_rate_hz,
        latency,
        tail,
        unknown_finish_capped: !terminal && !energy_stopped && unknown_tail,
    })
}

fn trim_unknown_tail_before_dither(
    samples: &mut Vec<f64>,
    channels: usize,
    protected_frames: usize,
    sample_rate_hz: u32,
    policy: UnknownTailPolicy,
    finish_was_capped: bool,
) -> Result<bool, ProcessError> {
    let total_frames = samples.len() / channels;
    let scan_start = protected_frames.min(total_frames);
    let mut detector = TailEnergyDetector::new(policy, sample_rate_hz)?;
    if let Some(silence_start) =
        detector.observe(&samples[scan_start * channels..], channels, scan_start)
    {
        samples.truncate(silence_start * channels);
    }

    Ok(finish_was_capped)
}

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
        introduces_latency: true,
        latency_note: "optional high-pass and oversampling filter state; direct mode has no explicit output delay",
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
///
/// Cloning this value clones its control handles. Each simultaneously live
/// callback or render chain must nevertheless receive a distinct
/// [`ConvolverControl`], because that control has exactly one audio consumer.
#[derive(Clone)]
pub struct OutputChainParams {
    pub channels: usize,
    pub source_sample_rate: u32,
    pub output_sample_rate: u32,
    pub eq_params: Arc<AtomicEqParams>,
    pub saturation_params: Arc<AtomicSaturationParams>,
    pub crossfeed_params: Arc<AtomicCrossfeedParams>,
    pub convolver_control: ConvolverControl,
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

    /// Return the control-plane handle retained across processor type erasure.
    ///
    /// A control may have multiple control-thread clones, but callers must not
    /// keep more than one callback/render chain built from it alive at once.
    pub fn convolver_control(&self) -> ConvolverControl {
        self.params.convolver_control.clone()
    }

    /// Build the callback-safe DSP chain from the canonical callback order.
    pub fn build_callback_chain(&self) -> Result<DspChain, ProcessError> {
        let sample_rate_hz = self.params.source_sample_rate;
        let sample_rate = sample_rate_hz as f64;
        let mut chain = DspChain::with_capacity(callback_stage_names().len(), sample_rate_hz);

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
                self.params.convolver_control.clone(),
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

        chain.set_sample_rate(sample_rate_hz)?;
        Ok(chain)
    }

    /// Build the offline render chain from the canonical offline order.
    pub fn build_render_chain(&self) -> Result<OutputRenderChain, String> {
        OutputRenderChain::new(&self.params, OfflineRenderPolicy::default())
    }

    /// Build an offline chain with an explicit default render policy.
    pub fn build_render_chain_with_policy(
        &self,
        policy: OfflineRenderPolicy,
    ) -> Result<OutputRenderChain, String> {
        policy.validate().map_err(|error| error.to_string())?;
        OutputRenderChain::new(&self.params, policy)
    }
}

/// Render result after the full offline output chain.
pub struct RenderedOutput {
    pub samples: Vec<f64>,
    pub final_limiter_gain_reduction_db: f64,
    /// Final frame count after optional latency compensation.
    pub rendered_frames: usize,
    /// Algorithmic latency removed in compensated mode, or retained in raw mode.
    pub algorithmic_latency_frames: usize,
    /// Accumulated finite semantic tail preserved at the final output rate.
    pub semantic_tail_frames: usize,
    /// True only when an unknown/infinite tail hit its configured safety limit.
    pub tail_truncated: bool,
}

struct ConvolverReclaimer(ConvolverControl);

impl ConvolverReclaimer {
    fn reclaim(&self) {
        let _ = self.0.reclaim_retired();
    }
}

impl Drop for ConvolverReclaimer {
    fn drop(&mut self) {
        self.reclaim();
    }
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
    default_policy: OfflineRenderPolicy,
}

impl OutputRenderChain {
    fn new(
        params: &OutputChainParams,
        default_policy: OfflineRenderPolicy,
    ) -> Result<Self, String> {
        let source_sample_rate = params.source_sample_rate as f64;
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
            convolver: ConvolverProcessor::new(params.convolver_control.clone()),
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
            default_policy,
        };

        chain
            .set_source_sample_rate(params.source_sample_rate)
            .map_err(|err| format!("failed to configure source-rate DSP stages: {err}"))?;
        chain
            .noise_shaper
            .set_sample_rate(params.output_sample_rate)
            .map_err(|err| format!("failed to configure output noise shaper: {err}"))?;
        Ok(chain)
    }

    /// Process through the offline output chain up to final sample-format
    /// quantization. This is useful for parity tests against the callback chain
    /// when `source_sample_rate == output_sample_rate`.
    pub fn process_pre_quantize(&mut self, buffer: &mut Vec<f64>) -> Result<(), ProcessError> {
        let reclaimer = ConvolverReclaimer(self.convolver.control());
        reclaimer.reclaim();
        let _ = process_fixed_stage(&mut self.volume, buffer, self.channels)?;
        let _ = process_fixed_stage(&mut self.eq, buffer, self.channels)?;
        let _ = process_fixed_stage(&mut self.saturation, buffer, self.channels)?;
        let _ = process_fixed_stage(&mut self.crossfeed, buffer, self.channels)?;
        let _ = process_fixed_stage(&mut self.convolver, buffer, self.channels)?;
        let _ = process_fixed_stage(&mut self.dynamic_loudness, buffer, self.channels)?;
        let _ = process_fixed_stage(&mut self.limiter, buffer, self.channels)?;

        if let Some(resampler) = self.resampler.as_mut() {
            let rendered = drive_offline_stage(
                resampler,
                buffer,
                self.channels,
                self.source_sample_rate,
                OfflineRenderPolicy::default(),
                DEFAULT_OFFLINE_BLOCK_FRAMES,
            )?;
            *buffer = rendered.samples;
        }

        let _ = process_fixed_stage(&mut self.noise_shaper, buffer, self.channels)?;
        reclaimer.reclaim();
        Ok(())
    }

    /// Render samples through DSP, optional resampling, final noise shaping, and
    /// f32 output quantization.
    pub fn render(&mut self, samples: &[f64]) -> Result<RenderedOutput, ProcessError> {
        self.render_with_policy(samples, self.default_policy)
    }

    /// Render with an explicit timeline and unknown-tail policy.
    pub fn render_with_policy(
        &mut self,
        samples: &[f64],
        policy: OfflineRenderPolicy,
    ) -> Result<RenderedOutput, ProcessError> {
        self.render_with_policy_and_block_frames(samples, policy, DEFAULT_OFFLINE_BLOCK_FRAMES)
    }

    fn render_with_policy_and_block_frames(
        &mut self,
        samples: &[f64],
        policy: OfflineRenderPolicy,
        block_frames: usize,
    ) -> Result<RenderedOutput, ProcessError> {
        let reclaimer = ConvolverReclaimer(self.convolver.control());
        reclaimer.reclaim();
        let policy = policy.validate()?;
        let source_block = AudioBlockRef::new(samples, self.channels)?;
        self.reset_for_render()?;

        let mut output = samples.to_vec();
        let mut sample_rate_hz = self.source_sample_rate;
        let mut timing = RenderTiming::default();

        macro_rules! render_stage {
            ($processor:expr) => {{
                let stage = drive_offline_stage(
                    $processor,
                    &output,
                    self.channels,
                    sample_rate_hz,
                    policy,
                    block_frames,
                )?;
                timing.observe(&stage);
                sample_rate_hz = stage.output_sample_rate_hz;
                output = stage.samples;
            }};
        }

        render_stage!(&mut self.volume);
        render_stage!(&mut self.eq);
        render_stage!(&mut self.saturation);
        render_stage!(&mut self.crossfeed);
        render_stage!(&mut self.convolver);
        render_stage!(&mut self.dynamic_loudness);
        render_stage!(&mut self.limiter);
        if let Some(resampler) = self.resampler.as_mut() {
            render_stage!(resampler);
        }

        if sample_rate_hz != self.output_sample_rate {
            return Err(ProcessError::SampleRateMismatch {
                processor: "OutputRenderChain",
                expected_sample_rate_hz: self.output_sample_rate,
                actual_sample_rate_hz: sample_rate_hz,
            });
        }

        let latency_frames = timing.latency_frames(sample_rate_hz)?;
        let semantic_tail_frames = timing.finite_tail_frames(sample_rate_hz)?;
        let source_duration_frames =
            FrameDuration::new(source_block.frames(), self.source_sample_rate)?
                .rounded_frames_at_rate(sample_rate_hz, FrameRounding::Nearest)?;
        let protected_frames = checked_frame_sum(
            checked_frame_sum(source_duration_frames, latency_frames)?,
            semantic_tail_frames,
        )?;
        let tail_truncated = if timing.has_unknown_tail {
            trim_unknown_tail_before_dither(
                &mut output,
                self.channels,
                protected_frames,
                sample_rate_hz,
                policy.unknown_tail,
                timing.unknown_finish_capped,
            )?
        } else {
            false
        };

        render_stage!(&mut self.noise_shaper);
        let latency_frames = timing.latency_frames(sample_rate_hz)?;
        let semantic_tail_frames = timing.finite_tail_frames(sample_rate_hz)?;

        for sample in &mut output {
            *sample = *sample as f32 as f64;
        }

        if policy.timeline == RenderTimeline::Compensated {
            let trim_frames = latency_frames.min(output.len() / self.channels);
            let trim_samples = trim_frames * self.channels;
            output.copy_within(trim_samples.., 0);
            output.truncate(output.len() - trim_samples);
        }

        let rendered_frames = output.len() / self.channels;
        reclaimer.reclaim();

        Ok(RenderedOutput {
            samples: output,
            final_limiter_gain_reduction_db: self.limiter.gain_reduction_db(),
            rendered_frames,
            algorithmic_latency_frames: latency_frames,
            semantic_tail_frames,
            tail_truncated,
        })
    }

    pub fn limiter_gain_reduction_db(&self) -> f64 {
        self.limiter.gain_reduction_db()
    }

    fn reset_for_render(&mut self) -> Result<(), ProcessError> {
        let mut first_error = None;
        macro_rules! reset_stage {
            ($processor:expr) => {
                if let Err(error) = $processor.reset() {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            };
        }

        reset_stage!(&mut self.volume);
        reset_stage!(&mut self.eq);
        reset_stage!(&mut self.saturation);
        reset_stage!(&mut self.crossfeed);
        reset_stage!(&mut self.convolver);
        reset_stage!(&mut self.dynamic_loudness);
        reset_stage!(&mut self.limiter);
        if let Some(resampler) = self.resampler.as_mut() {
            reset_stage!(resampler);
        }
        reset_stage!(&mut self.noise_shaper);

        first_error.map_or(Ok(()), Err)
    }

    fn set_source_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
        self.volume.set_sample_rate(sample_rate_hz)?;
        self.eq.set_sample_rate(sample_rate_hz)?;
        self.saturation.set_sample_rate(sample_rate_hz)?;
        self.crossfeed.set_sample_rate(sample_rate_hz)?;
        self.convolver.set_sample_rate(sample_rate_hz)?;
        self.dynamic_loudness.set_sample_rate(sample_rate_hz)?;
        self.limiter.set_sample_rate(sample_rate_hz)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processor::{
        FFTConvolver, NoiseShaperCurve, SaturationQualityValue, SaturationTypeValue, EQ_BANDS,
    };

    const CHANNELS: usize = 2;
    const SAMPLE_RATE: u32 = 48_000;

    #[test]
    fn callback_builder_order_matches_canonical_stage_list() {
        let builder = test_builder();
        let chain = builder.build_callback_chain().unwrap();

        assert_callback_stage_order_matches(&chain.processor_names());
    }

    #[test]
    fn callback_runtime_order_matches_offline_shared_stage_intersection() {
        let builder = test_builder();
        let chain = builder.build_callback_chain().unwrap();
        let shared_offline_stage_names = canonical_output_stage_descriptors()
            .iter()
            .filter(|stage| stage.offline_stage && stage.callback_stage)
            .map(|stage| stage.name)
            .collect::<Vec<_>>();

        assert_eq!(chain.processor_names(), shared_offline_stage_names);
    }

    #[test]
    fn callback_builder_retains_convolver_control_after_type_erasure() {
        let params = transparent_render_params(SAMPLE_RATE, SAMPLE_RATE);
        let builder = OutputChainBuilder::new(params);
        let control = builder.convolver_control();
        let mut chain = builder.build_callback_chain().unwrap();

        control.set_enabled(true);
        let generation = control.publish(FFTConvolver::new(&[0.5, 0.5], CHANNELS));
        let mut samples = [1.0, -1.0, 0.5, -0.5];
        let progress = chain.process(&mut samples, CHANNELS).unwrap();

        assert_eq!(samples, [0.5, -0.5, 0.25, -0.25]);
        assert_eq!(progress.produced_frames(), 2);
        assert_eq!(control.status().latest_adopted_generation, generation);
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
        let mut callback_chain = builder.build_callback_chain().unwrap();
        let mut render_chain = builder.build_render_chain().unwrap();

        let input = fixture_signal(512);
        let mut callback = input.clone();
        let mut rendered = input;

        let _ = callback_chain.process(&mut callback, CHANNELS).unwrap();
        render_chain.process_pre_quantize(&mut rendered).unwrap();

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
        let mut chain = builder.build_callback_chain().unwrap();
        let mut buffer = fixture_signal(256);

        let _ = chain.process(&mut buffer, CHANNELS).unwrap();

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..32 {
                let _ = chain.process(&mut buffer, CHANNELS).unwrap();
            }
        });
    }

    #[test]
    fn callback_chain_is_equivalent_across_irregular_frame_chunks() {
        let builder = active_test_builder();
        let mut whole_chain = builder.build_callback_chain().unwrap();
        let mut chunked_chain = builder.build_callback_chain().unwrap();
        let input = fixture_signal(4_096);
        let mut whole = input.clone();
        let mut chunked = input;

        let _ = whole_chain.process(&mut whole, CHANNELS).unwrap();

        let chunk_pattern = [1, 17, 3, 127, 64, 5, 251, 32];
        let total_frames = chunked.len() / CHANNELS;
        let mut start_frame = 0;
        let mut pattern_index = 0;
        while start_frame < total_frames {
            let end_frame = (start_frame + chunk_pattern[pattern_index % chunk_pattern.len()])
                .min(total_frames);
            let _ = chunked_chain
                .process(
                    &mut chunked[start_frame * CHANNELS..end_frame * CHANNELS],
                    CHANNELS,
                )
                .unwrap();
            start_frame = end_frame;
            pattern_index += 1;
        }

        for (index, (left, right)) in whole.iter().zip(&chunked).enumerate() {
            assert_eq!(
                left.to_bits(),
                right.to_bits(),
                "sample {index} changed with callback chunking: whole={left:?} chunked={right:?}"
            );
        }
    }

    #[test]
    fn callback_chain_reset_isolates_prior_stream_state() {
        let builder = active_test_builder();
        let mut reused = builder.build_callback_chain().unwrap();
        let mut reference = builder.build_callback_chain().unwrap();
        reused.reset().unwrap();
        reference.reset().unwrap();

        let mut warmup = fixture_signal(2_048);
        let _ = reused.process(&mut warmup, CHANNELS).unwrap();
        reused.reset().unwrap();

        let input = fixture_signal(512);
        let mut actual = input.clone();
        let mut expected = input;
        let _ = reused.process(&mut actual, CHANNELS).unwrap();
        let _ = reference.process(&mut expected, CHANNELS).unwrap();

        for (index, (left, right)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(
                left.to_bits(),
                right.to_bits(),
                "sample {index} leaked pre-reset state: reused={left:?} reference={right:?}"
            );
        }
    }

    #[test]
    fn default_render_compensates_limiter_latency_and_preserves_last_impulse() {
        let params = transparent_render_params(SAMPLE_RATE, SAMPLE_RATE);
        params.limiter_params.set_enabled(true);
        let builder = OutputChainBuilder::new(params);
        let mut chain = builder.build_render_chain().unwrap();
        let mut input = vec![0.0; 128 * CHANNELS];
        let input_len = input.len();
        input[input_len - 2] = 0.5;
        input[input_len - 1] = -0.5;

        let raw = chain
            .render_with_policy(&input, OfflineRenderPolicy::raw_causal())
            .unwrap();
        let compensated = chain.render(&input).unwrap();

        assert!(raw.algorithmic_latency_frames > 0);
        assert_eq!(
            raw.rendered_frames,
            input.len() / CHANNELS + raw.algorithmic_latency_frames
        );
        assert_eq!(compensated.rendered_frames, input.len() / CHANNELS);
        assert_eq!(
            &raw.samples[raw.algorithmic_latency_frames * CHANNELS..],
            compensated.samples.as_slice()
        );
        assert!(compensated.samples[compensated.samples.len() - 2].abs() > 0.49);
        assert!(compensated.samples[compensated.samples.len() - 1].abs() > 0.49);
        assert!(!raw.tail_truncated);
        assert!(!compensated.tail_truncated);
    }

    #[test]
    fn convolver_tail_flows_through_limiter_and_resampler_independent_of_block_size() {
        let params = transparent_render_params(48_000, 96_000);
        params.limiter_params.set_enabled(true);
        let builder = OutputChainBuilder::new(params);
        let control = builder.convolver_control();
        control.set_enabled(true);
        control.publish(FFTConvolver::new(
            &[1.0, 1.0, 0.5, 0.5, 0.25, 0.25],
            CHANNELS,
        ));
        let mut chain = builder.build_render_chain().unwrap();
        let mut input = vec![0.0; 64 * CHANNELS];
        let input_len = input.len();
        input[input_len - 2] = 0.4;
        input[input_len - 1] = -0.4;
        let policy = OfflineRenderPolicy::default();

        let small_blocks = chain
            .render_with_policy_and_block_frames(&input, policy, 17)
            .unwrap();
        let large_blocks = chain
            .render_with_policy_and_block_frames(&input, policy, 257)
            .unwrap();

        assert_eq!(small_blocks.semantic_tail_frames, 4);
        assert_eq!(small_blocks.rendered_frames, 64 * 2 + 4);
        assert_eq!(small_blocks.rendered_frames, large_blocks.rendered_frames);
        assert_eq!(
            small_blocks.algorithmic_latency_frames,
            large_blocks.algorithmic_latency_frames
        );
        assert_eq!(small_blocks.tail_truncated, large_blocks.tail_truncated);
        assert_eq!(small_blocks.samples, large_blocks.samples);
        assert!(small_blocks.samples[small_blocks.samples.len() - 16..]
            .iter()
            .any(|sample| sample.abs() > 1.0e-5));
    }

    struct UnknownTailProcessor {
        channels: usize,
        sample_rate_hz: u32,
        decay: f64,
        tail_frame: usize,
        finishing: bool,
    }

    impl UnknownTailProcessor {
        fn new(channels: usize, sample_rate_hz: u32, decay: f64) -> Self {
            Self {
                channels,
                sample_rate_hz,
                decay,
                tail_frame: 0,
                finishing: false,
            }
        }
    }

    impl StreamingProcessor for UnknownTailProcessor {
        fn name(&self) -> &'static str {
            "UnknownTailTest"
        }

        fn process(
            &mut self,
            buffers: ProcessBuffers<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            if self.finishing {
                return Err(ProcessError::AlreadyFinished {
                    processor: self.name(),
                });
            }
            match buffers.into_parts() {
                super::super::traits::ProcessBufferParts::InPlace(block) => Ok(
                    ProcessProgress::new(block.frames(), block.frames(), ProcessState::NeedInput),
                ),
                super::super::traits::ProcessBufferParts::OutOfPlace { input, mut output } => {
                    let frames = input.frames().min(output.frames());
                    let samples = frames * self.channels;
                    output.samples_mut()[..samples].copy_from_slice(&input.samples()[..samples]);
                    let state = if frames < input.frames() {
                        ProcessState::NeedOutput
                    } else {
                        ProcessState::NeedInput
                    };
                    Ok(ProcessProgress::new(frames, frames, state))
                }
            }
        }

        fn finish(
            &mut self,
            mut output: AudioBlockMut<'_>,
        ) -> Result<ProcessProgress, ProcessError> {
            self.finishing = true;
            for frame in output.samples_mut().chunks_exact_mut(self.channels) {
                let value = self.decay.powf(self.tail_frame as f64);
                frame.fill(value);
                self.tail_frame += 1;
            }
            Ok(ProcessProgress::new(
                0,
                output.frames(),
                ProcessState::NeedOutput,
            ))
        }

        fn reset(&mut self) -> Result<(), ProcessError> {
            self.tail_frame = 0;
            self.finishing = false;
            Ok(())
        }

        fn tail(&self) -> TailSpec {
            TailSpec::Unknown
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn set_enabled(&mut self, _enabled: bool) {}

        fn set_sample_rate(&mut self, sample_rate_hz: u32) -> Result<(), ProcessError> {
            self.sample_rate_hz = sample_rate_hz;
            Ok(())
        }
    }

    #[test]
    fn unknown_tail_energy_stop_is_block_size_independent() {
        let policy = OfflineRenderPolicy {
            unknown_tail: UnknownTailPolicy {
                energy_threshold_dbfs: -40.0,
                silence_hold_ms: 5,
                max_tail_ms: 100,
            },
            ..OfflineRenderPolicy::default()
        };
        let input = [1.0];

        let mut small = UnknownTailProcessor::new(1, 1_000, 0.8);
        let mut small_output =
            drive_offline_stage(&mut small, &input, 1, 1_000, policy, 7).unwrap();
        let small_generated_tail_frames = small.tail_frame;
        let small_truncated = trim_unknown_tail_before_dither(
            &mut small_output.samples,
            1,
            1,
            1_000,
            policy.unknown_tail,
            small_output.unknown_finish_capped,
        )
        .unwrap();

        let mut large = UnknownTailProcessor::new(1, 1_000, 0.8);
        let mut large_output =
            drive_offline_stage(&mut large, &input, 1, 1_000, policy, 31).unwrap();
        let large_generated_tail_frames = large.tail_frame;
        let large_truncated = trim_unknown_tail_before_dither(
            &mut large_output.samples,
            1,
            1,
            1_000,
            policy.unknown_tail,
            large_output.unknown_finish_capped,
        )
        .unwrap();

        assert!(!small_truncated);
        assert!(!large_truncated);
        assert_eq!(small_output.samples, large_output.samples);
        assert!(small_generated_tail_frames < 100);
        assert!(large_generated_tail_frames < 100);
        assert!(small_generated_tail_frames <= 28);
        assert!(large_generated_tail_frames <= 31);
    }

    #[test]
    fn unknown_tail_reports_truncation_at_safety_limit() {
        let policy = OfflineRenderPolicy {
            unknown_tail: UnknownTailPolicy {
                energy_threshold_dbfs: -80.0,
                silence_hold_ms: 5,
                max_tail_ms: 20,
            },
            ..OfflineRenderPolicy::default()
        };
        let input = [1.0];
        let mut processor = UnknownTailProcessor::new(1, 1_000, 1.0);
        let mut rendered =
            drive_offline_stage(&mut processor, &input, 1, 1_000, policy, 7).unwrap();
        assert_eq!(processor.tail_frame, 20);
        let truncated = trim_unknown_tail_before_dither(
            &mut rendered.samples,
            1,
            1,
            1_000,
            policy.unknown_tail,
            rendered.unknown_finish_capped,
        )
        .unwrap();

        assert!(truncated);
        assert_eq!(rendered.samples.len(), 1 + 20);
    }

    #[test]
    fn capped_unknown_tail_remains_reported_after_silence_trim() {
        let policy = UnknownTailPolicy {
            energy_threshold_dbfs: -80.0,
            silence_hold_ms: 5,
            max_tail_ms: 20,
        };
        let mut samples = vec![1.0];
        samples.extend_from_slice(&[0.0; 5]);

        let truncated =
            trim_unknown_tail_before_dither(&mut samples, 1, 1, 1_000, policy, true).unwrap();

        assert!(truncated);
        assert_eq!(samples, vec![1.0]);
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
        params
            .saturation_params
            .set_quality(SaturationQualityValue::Oversampled4x);
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
            convolver_control: ConvolverControl::default(),
            volume_params: Arc::new(AtomicVolumeParams::new()),
            dynamic_loudness_params: Arc::new(AtomicDynamicLoudnessParams::new()),
            dynamic_loudness_telemetry: Arc::new(AtomicDynamicLoudnessTelemetry::new()),
            limiter_params: Arc::new(AtomicPeakLimiterParams::new()),
            noise_shaper_params: Arc::new(AtomicNoiseShaperParams::new()),
        }
    }

    fn transparent_render_params(
        source_sample_rate: u32,
        output_sample_rate: u32,
    ) -> OutputChainParams {
        let mut params = test_params();
        params.source_sample_rate = source_sample_rate;
        params.output_sample_rate = output_sample_rate;
        params.eq_params.write(&[0.0; EQ_BANDS], false);
        params.saturation_params.set_enabled(false);
        params.crossfeed_params.set_enabled(false);
        params.dynamic_loudness_params.set_enabled(false);
        params.limiter_params.set_enabled(false);
        params.noise_shaper_params.set_enabled(false);
        params
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
