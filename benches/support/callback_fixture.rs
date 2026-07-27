use std::hint::black_box;
use std::sync::Arc;

use audio_engine_core::processor::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry,
    AtomicEqParams, AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams,
    AtomicVolumeParams, ConvolverControl, DspChain, FFTConvolver, NoiseShaperCurve,
    OutputChainBuilder, OutputChainParams, SaturationQualityValue, SaturationTypeValue, EQ_BANDS,
};
use serde::{Deserialize, Serialize};

pub const CALLBACK_CHANNELS: usize = 2;
pub const CALLBACK_SAMPLE_RATE_HZ: u32 = 48_000;
pub const CALLBACK_BUFFER_FRAMES: [usize; 4] = [64, 128, 256, 512];
pub const CALLBACK_WARMUP_BUFFERS: usize = 256;
const CALLBACK_CONVOLVER_TAPS_PER_CHANNEL: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallbackScenario {
    BypassDefault,
    ActiveDspNoConvolver,
    ActiveDspWithConvolver,
}

impl CallbackScenario {
    pub const ALL: [Self; 3] = [
        Self::BypassDefault,
        Self::ActiveDspNoConvolver,
        Self::ActiveDspWithConvolver,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::BypassDefault => "bypass_default",
            Self::ActiveDspNoConvolver => "active_dsp_no_convolver",
            Self::ActiveDspWithConvolver => "active_dsp_with_convolver",
        }
    }

    pub const fn config_key(self) -> &'static str {
        match self {
            Self::BypassDefault => "bypass_defaults",
            Self::ActiveDspNoConvolver => "active_oversampled4x_no_convolver",
            Self::ActiveDspWithConvolver => "active_oversampled4x_ir256",
        }
    }

    pub const fn config_description(self) -> &'static str {
        match self {
            Self::BypassDefault => {
                "optional stages disabled; volume unity; convolver slot empty"
            }
            Self::ActiveDspNoConvolver => {
                "EQ + Oversampled4x Tube saturation + Bauer low-pass crossfeed + dynamic loudness + true-peak limiter + 24-bit TPDF noise shaper; convolver slot empty"
            }
            Self::ActiveDspWithConvolver => {
                "active DSP configuration plus a stereo 256-tap synthetic convolver"
            }
        }
    }
}

pub fn callback_case_key(scenario: CallbackScenario, frames: usize) -> String {
    format!(
        "scenario={};frames={};config={}",
        scenario.name(),
        frames,
        scenario.config_key()
    )
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct CallbackWorkValidation {
    pub valid: bool,
    pub all_samples_finite: bool,
    pub output_changed: bool,
    pub expected_output_changed: bool,
    pub bypassed: bool,
    pub consumed_frames: usize,
    pub produced_frames: usize,
}

pub struct CallbackChainFixture {
    chain: DspChain,
}

struct CallbackParamRefs<'a> {
    eq: &'a AtomicEqParams,
    saturation: &'a AtomicSaturationParams,
    crossfeed: &'a AtomicCrossfeedParams,
    limiter: &'a AtomicPeakLimiterParams,
    volume: &'a AtomicVolumeParams,
    noise_shaper: &'a AtomicNoiseShaperParams,
    dynamic_loudness: &'a AtomicDynamicLoudnessParams,
}

impl CallbackChainFixture {
    pub fn build(scenario: CallbackScenario) -> Result<Self, String> {
        let eq_params = Arc::new(AtomicEqParams::new());
        let saturation_params = Arc::new(AtomicSaturationParams::new());
        let crossfeed_params = Arc::new(AtomicCrossfeedParams::new());
        let limiter_params = Arc::new(AtomicPeakLimiterParams::new());
        let volume_params = Arc::new(AtomicVolumeParams::new());
        let noise_shaper_params = Arc::new(AtomicNoiseShaperParams::new());
        let dynamic_loudness_params = Arc::new(AtomicDynamicLoudnessParams::new());
        let dynamic_loudness_telemetry = Arc::new(AtomicDynamicLoudnessTelemetry::new());

        configure_params(
            scenario,
            CallbackParamRefs {
                eq: &eq_params,
                saturation: &saturation_params,
                crossfeed: &crossfeed_params,
                limiter: &limiter_params,
                volume: &volume_params,
                noise_shaper: &noise_shaper_params,
                dynamic_loudness: &dynamic_loudness_params,
            },
        );

        let convolver_control = ConvolverControl::default();
        if scenario == CallbackScenario::ActiveDspWithConvolver {
            convolver_control.set_enabled(true);
            let convolver = FFTConvolver::new(
                &synthetic_callback_ir(CALLBACK_CONVOLVER_TAPS_PER_CHANNEL, CALLBACK_CHANNELS),
                CALLBACK_CHANNELS,
            )
            .map_err(|error| format!("benchmark IR configuration failed: {error}"))?;
            convolver_control
                .publish_at_rate(convolver, CALLBACK_SAMPLE_RATE_HZ)
                .map_err(|error| format!("benchmark convolver publication failed: {error}"))?;
        }

        let chain = OutputChainBuilder::new(OutputChainParams {
            channels: CALLBACK_CHANNELS,
            source_sample_rate: CALLBACK_SAMPLE_RATE_HZ,
            output_sample_rate: CALLBACK_SAMPLE_RATE_HZ,
            eq_params,
            saturation_params,
            crossfeed_params,
            convolver_control,
            volume_params,
            dynamic_loudness_params,
            dynamic_loudness_telemetry,
            limiter_params,
            noise_shaper_params,
        })
        .build_callback_chain()
        .map_err(|error| format!("benchmark output-chain configuration failed: {error}"))?;

        Ok(Self { chain })
    }

    pub fn chain_mut(&mut self) -> &mut DspChain {
        &mut self.chain
    }

    pub fn warm(&mut self, corpus: &[f64]) -> Result<(), String> {
        let mut scratch = corpus.to_vec();
        for _ in 0..CALLBACK_WARMUP_BUFFERS {
            scratch.copy_from_slice(corpus);
            let _progress = self
                .chain
                .process(black_box(&mut scratch), CALLBACK_CHANNELS)
                .map_err(|error| format!("benchmark callback warmup failed: {error}"))?;
        }
        Ok(())
    }
}

pub fn validate_callback_work(
    scenario: CallbackScenario,
    frames: usize,
    corpus: &[f64],
) -> Result<CallbackWorkValidation, String> {
    let mut fixture = CallbackChainFixture::build(scenario)?;
    fixture.warm(corpus)?;
    let mut scratch = corpus.to_vec();
    let progress = fixture
        .chain
        .process(&mut scratch, CALLBACK_CHANNELS)
        .map_err(|error| format!("benchmark callback validation failed: {error}"))?;
    let all_samples_finite = scratch.iter().all(|sample| sample.is_finite());
    let output_changed = scratch
        .iter()
        .zip(corpus)
        .any(|(output, input)| output.to_bits() != input.to_bits());
    let expected_output_changed = scenario != CallbackScenario::BypassDefault;
    let bypassed = progress.is_bypassed();
    let consumed_frames = progress.consumed_frames();
    let produced_frames = progress.produced_frames();

    Ok(CallbackWorkValidation {
        valid: all_samples_finite
            && output_changed == expected_output_changed
            && consumed_frames == frames
            && produced_frames == frames,
        all_samples_finite,
        output_changed,
        expected_output_changed,
        bypassed,
        consumed_frames,
        produced_frames,
    })
}

fn configure_params(scenario: CallbackScenario, params: CallbackParamRefs<'_>) {
    let CallbackParamRefs {
        eq: eq_params,
        saturation: saturation_params,
        crossfeed: crossfeed_params,
        limiter: limiter_params,
        volume: volume_params,
        noise_shaper: noise_shaper_params,
        dynamic_loudness: dynamic_loudness_params,
    } = params;

    match scenario {
        CallbackScenario::BypassDefault => {
            eq_params.write(&[0.0; EQ_BANDS], false);
            saturation_params.set_enabled(false);
            saturation_params.set_armed(false);
            crossfeed_params.set_enabled(false);
            limiter_params.set_enabled(false);
            volume_params.set_volume(1.0);
            volume_params.set_muted(false);
            noise_shaper_params.set_enabled(false);
            dynamic_loudness_params.set_enabled(false);
        }
        CallbackScenario::ActiveDspNoConvolver => {
            eq_params.write(
                &[1.5, -0.75, 0.5, 0.0, -1.0, 0.8, 0.0, 1.0, -0.4, 0.2],
                true,
            );
            saturation_params.set_enabled(true);
            saturation_params.set_armed(true);
            saturation_params.set_drive(0.85);
            saturation_params.set_threshold(0.82);
            saturation_params.set_mix(0.35);
            saturation_params.set_sat_type(SaturationTypeValue::Tube);
            saturation_params.set_quality(SaturationQualityValue::Oversampled4x);
            saturation_params.set_highpass_mode(true);
            saturation_params.set_highpass_cutoff(4_000.0);
            crossfeed_params.set_enabled(true);
            crossfeed_params.set_mix(0.30);
            crossfeed_params.set_cutoff(700.0);
            limiter_params.set_enabled(true);
            limiter_params.set_threshold(-1.0);
            limiter_params.set_release(120.0);
            volume_params.set_volume(0.72);
            volume_params.set_muted(false);
            noise_shaper_params.set_enabled(true);
            noise_shaper_params.set_bits(24);
            noise_shaper_params.set_curve(NoiseShaperCurve::TpdfOnly);
            dynamic_loudness_params.set_enabled(true);
            dynamic_loudness_params.set_volume(0.72);
            dynamic_loudness_params.set_strength(0.65);
        }
        CallbackScenario::ActiveDspWithConvolver => configure_params(
            CallbackScenario::ActiveDspNoConvolver,
            CallbackParamRefs {
                eq: eq_params,
                saturation: saturation_params,
                crossfeed: crossfeed_params,
                limiter: limiter_params,
                volume: volume_params,
                noise_shaper: noise_shaper_params,
                dynamic_loudness: dynamic_loudness_params,
            },
        ),
    }
}

pub fn synthetic_callback_buffer(frames: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * CALLBACK_CHANNELS);
    let mut left_phase = 0.0_f64;
    let mut right_phase = 0.0_f64;
    let sample_rate = CALLBACK_SAMPLE_RATE_HZ as f64;

    for frame in 0..frames {
        let t = frame as f64 / sample_rate;
        left_phase += std::f64::consts::TAU * (220.0 + 11.0 * (t * 3.0).sin()) / sample_rate;
        right_phase += std::f64::consts::TAU * (330.0 + 7.0 * (t * 5.0).cos()) / sample_rate;
        let envelope = 0.65 + 0.20 * (std::f64::consts::TAU * 1.7 * t).sin();
        let transient = if frame % 127 == 0 { 0.28 } else { 0.0 };
        let left =
            (left_phase.sin() * 0.55 + (left_phase * 3.0).sin() * 0.08 + transient) * envelope;
        let right =
            (right_phase.sin() * 0.50 - (right_phase * 2.0).cos() * 0.07 - transient) * envelope;

        out.push(left.clamp(-0.95, 0.95));
        out.push(right.clamp(-0.95, 0.95));
    }

    out
}

fn synthetic_callback_ir(taps_per_channel: usize, channels: usize) -> Vec<f64> {
    let mut ir = Vec::with_capacity(taps_per_channel * channels);

    for tap in 0..taps_per_channel {
        let decay = (-(tap as f64) / 48.0).exp();
        for ch in 0..channels {
            let impulse = if tap == 0 { 0.72 } else { 0.0 };
            let early = if tap == 17 + ch * 3 { 0.12 } else { 0.0 };
            let tail = ((tap + ch * 11) as f64 * 0.37).sin() * 0.025 * decay;
            ir.push(impulse + early + tail);
        }
    }

    ir
}
