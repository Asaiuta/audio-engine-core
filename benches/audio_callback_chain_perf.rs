use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;

use audio_engine_core::processor::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry,
    AtomicEqParams, AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams,
    AtomicVolumeParams, ConvolverProcessor, CrossfeedProcessor, DspChain, DynamicLoudnessProcessor,
    EqProcessor, FFTConvolver, NoiseShaperCurve, PeakLimiterProcessor, SaturationProcessor,
    SaturationTypeValue, VolumeProcessor, EQ_BANDS,
};

const CHANNELS: usize = 2;
const SAMPLE_RATE: f64 = 48_000.0;
const BUFFER_FRAMES: [usize; 4] = [64, 128, 256, 512];
const WARMUP_BUFFERS: usize = 256;
const NODE_ORDER: &str =
    "Equalizer,Saturation,Crossfeed,Convolver,Volume,DynamicLoudness,PeakLimiter";

#[derive(Clone, Copy)]
enum Scenario {
    BypassDefault,
    ActiveDspNoConvolver,
    ActiveDspWithConvolver,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Self::BypassDefault => "bypass_default",
            Self::ActiveDspNoConvolver => "active_dsp_no_convolver",
            Self::ActiveDspWithConvolver => "active_dsp_with_convolver",
        }
    }

    fn all() -> &'static [Self] {
        &[
            Self::BypassDefault,
            Self::ActiveDspNoConvolver,
            Self::ActiveDspWithConvolver,
        ]
    }
}

struct ChainBundle {
    chain: DspChain,
}

struct Report {
    ns_per_sample: f64,
    ns_per_buffer: f64,
    elapsed: Duration,
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let quick = args.iter().any(|arg| arg == "--quick");
    let heavy = args.iter().any(|arg| arg == "--heavy");
    let enforce = args.iter().any(|arg| arg == "--enforce");

    let (iterations, trials) = if quick {
        (500, 1)
    } else if heavy {
        (10_000, 5)
    } else {
        (2_000, 3)
    };

    println!(
        "audio_callback_chain_perf mode={} sample_rate={} channels={} nodes={} copy_input=true coverage=dsp_chain_only",
        if quick {
            "quick"
        } else if heavy {
            "heavy"
        } else {
            "full"
        },
        SAMPLE_RATE as u32,
        CHANNELS,
        NODE_ORDER
    );
    println!(
        "audio_callback_chain_note excludes=cpal_device_write,decoder,resampler,spectrum,loudness_normalization_pre_gain,gapless_state_machine"
    );

    for &scenario in Scenario::all() {
        for &frames in &BUFFER_FRAMES {
            let report = benchmark_scenario(scenario, frames, iterations, trials);
            println!(
                "callback_chain scenario={} frames={} samples={} iterations={} trials={} ns_per_sample={:.3} ns_per_buffer={:.3} elapsed_ms={:.3}",
                scenario.name(),
                frames,
                frames * CHANNELS,
                iterations,
                trials,
                report.ns_per_sample,
                report.ns_per_buffer,
                report.elapsed.as_secs_f64() * 1_000.0
            );

            if enforce && matches!(scenario, Scenario::ActiveDspNoConvolver) && frames == 512 {
                assert!(
                    report.ns_per_sample.is_finite() && report.ns_per_sample > 0.0,
                    "callback chain benchmark produced invalid timing"
                );
            }
        }
    }
}

fn benchmark_scenario(
    scenario: Scenario,
    frames: usize,
    iterations: usize,
    trials: usize,
) -> Report {
    let mut best: Option<Report> = None;

    for _ in 0..trials {
        let mut bundle = build_chain_bundle(scenario);
        let corpus = synthetic_buffer(frames, CHANNELS);
        warm_chain(&mut bundle, scenario, &corpus);
        let report = measure_chain(&mut bundle.chain, &corpus, frames, iterations);

        if best
            .as_ref()
            .is_none_or(|current| report.ns_per_sample < current.ns_per_sample)
        {
            best = Some(report);
        }
    }

    best.expect("at least one trial")
}

// Rebuilds the same lock-free DSP chain the Lyne realtime callback runs, using
// only the crate's public `DspChain` + processor adapters. The node order and
// parameter configuration mirror the integration's `build_dsp_chain`, so the
// timings here track the realtime callback's DSP cost (it excludes the decoder,
// resampler, spectrum packing, and the OS device write).
fn build_chain_bundle(scenario: Scenario) -> ChainBundle {
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
        &eq_params,
        &saturation_params,
        &crossfeed_params,
        &limiter_params,
        &volume_params,
        &noise_shaper_params,
        &dynamic_loudness_params,
    );

    let convolver_swap = Arc::new(ArcSwapOption::empty());
    let convolver_enabled = Arc::new(AtomicBool::new(false));

    if matches!(scenario, Scenario::ActiveDspWithConvolver) {
        convolver_enabled.store(true, Ordering::Release);
        convolver_swap.store(Some(Arc::new(FFTConvolver::new(
            &synthetic_ir(256, CHANNELS),
            CHANNELS,
        ))));
    }

    let mut chain = DspChain::new(SAMPLE_RATE);
    chain.add(EqProcessor::new(CHANNELS, SAMPLE_RATE, eq_params));
    chain.add(SaturationProcessor::new(CHANNELS, saturation_params));
    chain.add(CrossfeedProcessor::new(SAMPLE_RATE, crossfeed_params));
    chain.add(ConvolverProcessor::new(convolver_swap, convolver_enabled));
    chain.add(VolumeProcessor::new(volume_params));
    chain.add(DynamicLoudnessProcessor::new(
        CHANNELS,
        SAMPLE_RATE as u32,
        dynamic_loudness_params,
        dynamic_loudness_telemetry,
    ));
    chain.add(PeakLimiterProcessor::new(
        CHANNELS,
        SAMPLE_RATE as u32,
        limiter_params,
    ));

    ChainBundle { chain }
}

#[allow(clippy::too_many_arguments)]
fn configure_params(
    scenario: Scenario,
    eq_params: &AtomicEqParams,
    saturation_params: &AtomicSaturationParams,
    crossfeed_params: &AtomicCrossfeedParams,
    limiter_params: &AtomicPeakLimiterParams,
    volume_params: &AtomicVolumeParams,
    noise_shaper_params: &AtomicNoiseShaperParams,
    dynamic_loudness_params: &AtomicDynamicLoudnessParams,
) {
    match scenario {
        Scenario::BypassDefault => {
            eq_params.write(&[0.0; EQ_BANDS], false);
            saturation_params.set_enabled(false);
            crossfeed_params.set_enabled(false);
            limiter_params.set_enabled(false);
            volume_params.set_volume(1.0);
            volume_params.set_muted(false);
            noise_shaper_params.set_enabled(false);
            dynamic_loudness_params.set_enabled(false);
        }
        Scenario::ActiveDspNoConvolver => {
            eq_params.write(
                &[1.5, -0.75, 0.5, 0.0, -1.0, 0.8, 0.0, 1.0, -0.4, 0.2],
                true,
            );
            saturation_params.set_enabled(true);
            saturation_params.set_drive(0.85);
            saturation_params.set_threshold(0.82);
            saturation_params.set_mix(0.35);
            saturation_params.set_sat_type(SaturationTypeValue::Tube);
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
        Scenario::ActiveDspWithConvolver => {
            configure_params(
                Scenario::ActiveDspNoConvolver,
                eq_params,
                saturation_params,
                crossfeed_params,
                limiter_params,
                volume_params,
                noise_shaper_params,
                dynamic_loudness_params,
            );
        }
    }
}

fn warm_chain(bundle: &mut ChainBundle, _scenario: Scenario, corpus: &[f64]) {
    let mut scratch = corpus.to_vec();

    for _ in 0..WARMUP_BUFFERS {
        scratch.copy_from_slice(corpus);
        bundle.chain.process(black_box(&mut scratch), CHANNELS);
    }
}

fn measure_chain(chain: &mut DspChain, corpus: &[f64], frames: usize, iterations: usize) -> Report {
    let mut scratch = vec![0.0; corpus.len()];
    let start = Instant::now();

    for _ in 0..iterations {
        scratch.copy_from_slice(black_box(corpus));
        chain.process(black_box(&mut scratch), CHANNELS);
        black_box(&scratch);
    }

    let elapsed = start.elapsed();
    let ns_per_buffer = elapsed.as_nanos() as f64 / iterations as f64;
    let ns_per_sample = ns_per_buffer / (frames * CHANNELS) as f64;

    Report {
        ns_per_sample,
        ns_per_buffer,
        elapsed,
    }
}

fn synthetic_buffer(frames: usize, channels: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    let mut left_phase = 0.0_f64;
    let mut right_phase = 0.0_f64;

    for frame in 0..frames {
        let t = frame as f64 / SAMPLE_RATE;
        left_phase += std::f64::consts::TAU * (220.0 + 11.0 * (t * 3.0).sin()) / SAMPLE_RATE;
        right_phase += std::f64::consts::TAU * (330.0 + 7.0 * (t * 5.0).cos()) / SAMPLE_RATE;
        let envelope = 0.65 + 0.20 * (std::f64::consts::TAU * 1.7 * t).sin();
        let transient = if frame % 127 == 0 { 0.28 } else { 0.0 };
        let left =
            (left_phase.sin() * 0.55 + (left_phase * 3.0).sin() * 0.08 + transient) * envelope;
        let right =
            (right_phase.sin() * 0.50 - (right_phase * 2.0).cos() * 0.07 - transient) * envelope;

        out.push(left.clamp(-0.95, 0.95));
        if channels > 1 {
            out.push(right.clamp(-0.95, 0.95));
        }
        for ch in 2..channels {
            out.push((left * (1.0 - ch as f64 * 0.05)).clamp(-0.95, 0.95));
        }
    }

    out
}

fn synthetic_ir(taps_per_channel: usize, channels: usize) -> Vec<f64> {
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
