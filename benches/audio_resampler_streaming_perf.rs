use std::hint::black_box;
use std::time::{Duration, Instant};

use audio_engine_core::processor::StreamingResampler;

const CHANNELS: usize = 2;
const BUFFER_FRAMES: [usize; 3] = [128, 256, 512];
const WARMUP_BUFFERS: usize = 64;

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    from_rate: u32,
    to_rate: u32,
}

const SCENARIOS: [Scenario; 3] = [
    Scenario {
        name: "equal_rate_48k",
        from_rate: 48_000,
        to_rate: 48_000,
    },
    Scenario {
        name: "music_44k1_to_48k",
        from_rate: 44_100,
        to_rate: 48_000,
    },
    Scenario {
        name: "upsample_48k_to_96k",
        from_rate: 48_000,
        to_rate: 96_000,
    },
];

#[derive(Clone, Copy)]
enum ApiPath {
    Borrowed,
    Into,
    Append,
}

impl ApiPath {
    fn name(self) -> &'static str {
        match self {
            Self::Borrowed => "process_chunk_borrowed",
            Self::Into => "process_chunk_into",
            Self::Append => "process_chunk_append",
        }
    }

    fn all() -> &'static [Self] {
        &[Self::Borrowed, Self::Into, Self::Append]
    }
}

struct Report {
    ns_per_input_sample: f64,
    ns_per_input_buffer: f64,
    output_frames: usize,
    elapsed: Duration,
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let quick = args.iter().any(|arg| arg == "--quick");
    let heavy = args.iter().any(|arg| arg == "--heavy");
    let enforce = args.iter().any(|arg| arg == "--enforce");

    let (iterations, trials) = if quick {
        (400, 1)
    } else if heavy {
        (4_000, 5)
    } else {
        (1_200, 3)
    };

    println!(
        "audio_resampler_streaming_perf mode={} channels={} coverage=streaming_resampler_only",
        if quick {
            "quick"
        } else if heavy {
            "heavy"
        } else {
            "full"
        },
        CHANNELS
    );
    println!(
        "audio_resampler_streaming_note excludes=decoder,callback_dsp_chain,cpal_device_write,gapless_state_machine"
    );

    for scenario in SCENARIOS {
        for frames in BUFFER_FRAMES {
            let input = synthetic_buffer(frames, CHANNELS, scenario.from_rate);
            for &api in ApiPath::all() {
                let report = benchmark_api(scenario, api, frames, &input, iterations, trials);
                println!(
                    "resampler_streaming scenario={} api={} frames={} input_samples={} from_rate={} to_rate={} output_frames={} iterations={} trials={} ns_per_input_sample={:.3} ns_per_input_buffer={:.3} elapsed_ms={:.3}",
                    scenario.name,
                    api.name(),
                    frames,
                    frames * CHANNELS,
                    scenario.from_rate,
                    scenario.to_rate,
                    report.output_frames,
                    iterations,
                    trials,
                    report.ns_per_input_sample,
                    report.ns_per_input_buffer,
                    report.elapsed.as_secs_f64() * 1_000.0
                );

                if enforce
                    && scenario.name == "music_44k1_to_48k"
                    && matches!(api, ApiPath::Borrowed)
                    && frames == 512
                {
                    assert!(
                        report.ns_per_input_sample.is_finite() && report.output_frames > 0,
                        "resampler benchmark produced invalid timing or no output"
                    );
                }
            }
        }
    }
}

fn benchmark_api(
    scenario: Scenario,
    api: ApiPath,
    frames: usize,
    input: &[f64],
    iterations: usize,
    trials: usize,
) -> Report {
    let mut best: Option<Report> = None;

    for _ in 0..trials {
        let mut resampler = StreamingResampler::new(CHANNELS, scenario.from_rate, scenario.to_rate)
            .expect("valid resampler rates");
        warm_resampler(&mut resampler, api, input);
        let report = measure_resampler(&mut resampler, api, frames, input, iterations);

        if best
            .as_ref()
            .is_none_or(|b| report.ns_per_input_sample < b.ns_per_input_sample)
        {
            best = Some(report);
        }
    }

    best.expect("at least one trial")
}

fn warm_resampler(resampler: &mut StreamingResampler, api: ApiPath, input: &[f64]) {
    let mut output = vec![0.0; streaming_output_capacity(resampler, input.len())];
    let mut append_output = Vec::with_capacity(output.len());

    for _ in 0..WARMUP_BUFFERS {
        run_api(resampler, api, input, &mut output, &mut append_output);
    }
}

fn measure_resampler(
    resampler: &mut StreamingResampler,
    api: ApiPath,
    frames: usize,
    input: &[f64],
    iterations: usize,
) -> Report {
    let mut output = vec![0.0; streaming_output_capacity(resampler, input.len())];
    let mut append_output = Vec::with_capacity(output.len());
    let mut output_frames = 0usize;
    let start = Instant::now();

    for _ in 0..iterations {
        output_frames = run_api(
            resampler,
            api,
            black_box(input),
            &mut output,
            &mut append_output,
        );
    }

    let elapsed = start.elapsed();
    let ns_per_input_buffer = elapsed.as_nanos() as f64 / iterations as f64;
    let ns_per_input_sample = ns_per_input_buffer / (frames * CHANNELS) as f64;

    Report {
        ns_per_input_sample,
        ns_per_input_buffer,
        output_frames,
        elapsed,
    }
}

fn streaming_output_capacity(resampler: &StreamingResampler, input_samples: usize) -> usize {
    // SoX streaming resamplers can emit delayed output in bursts after internal
    // buffering. Use a deliberately conservative caller scratch so this bench
    // measures the API path rather than capacity edge behavior.
    resampler
        .max_output_len_for_input(input_samples)
        .saturating_mul(8)
        .saturating_add(8192)
}

fn run_api(
    resampler: &mut StreamingResampler,
    api: ApiPath,
    input: &[f64],
    output: &mut [f64],
    append_output: &mut Vec<f64>,
) -> usize {
    match api {
        ApiPath::Borrowed => {
            let result = resampler.process_chunk_borrowed(input);
            black_box(result.samples);
            result.frames
        }
        ApiPath::Into => {
            let frames = resampler.process_chunk_into(input, output);
            black_box(&output[..frames * CHANNELS]);
            frames
        }
        ApiPath::Append => {
            append_output.clear();
            let frames = resampler.process_chunk_append(input, append_output);
            black_box(&append_output);
            frames
        }
    }
}

fn synthetic_buffer(frames: usize, channels: usize, sample_rate: u32) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    let sample_rate = sample_rate as f64;
    let mut left_phase = 0.0_f64;
    let mut right_phase = 0.0_f64;

    for frame in 0..frames {
        let t = frame as f64 / sample_rate;
        left_phase += std::f64::consts::TAU * (330.0 + 17.0 * (t * 2.5).sin()) / sample_rate;
        right_phase += std::f64::consts::TAU * (550.0 + 23.0 * (t * 1.7).cos()) / sample_rate;
        let envelope = 0.7 + 0.15 * (std::f64::consts::TAU * 1.1 * t).sin();
        let left = (left_phase.sin() * 0.6 + (left_phase * 2.0).sin() * 0.05) * envelope;
        let right = (right_phase.sin() * 0.55 - (right_phase * 3.0).cos() * 0.04) * envelope;

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
