use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio_engine_core::processor::FFTConvolver;
use rustfft::{num_complex::Complex, FftPlanner};

const SAMPLE_RATE: f64 = 48_000.0;

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let quick = args.iter().any(|arg| arg == "--quick");
    let enforce = args.iter().any(|arg| arg == "--enforce");

    let iterations = if quick { 60 } else { 240 };
    let frames = if quick { 2_048 } else { 8_192 };
    let ir_frames = 256;
    let trials = parse_trials(&args).unwrap_or(if quick { 3 } else { 5 });

    println!(
        "audio_convolver_perf frames={frames} ir_frames={ir_frames} iterations={iterations} trials={trials}"
    );

    for channels in [2, 6] {
        let report = benchmark_convolver(channels, ir_frames, frames, iterations, trials);
        println!(
            "convolver channels={} process_into_median={:.3} ns/sample legacy_process_into_median={:.3} ns/sample into_speedup_median={:.2}% process_inplace_median={:.3} ns/sample legacy_process_inplace_median={:.3} ns/sample inplace_speedup_median={:.2}% allocating_process_median={:.3} ns/sample wrapper_overhead_median={:.2}%",
            channels,
            report.process_into_ns_per_sample,
            report.legacy_process_into_ns_per_sample,
            report.process_into_speedup_percent,
            report.process_inplace_ns_per_sample,
            report.legacy_process_inplace_ns_per_sample,
            report.process_inplace_speedup_percent,
            report.allocating_process_ns_per_sample,
            report.wrapper_overhead_percent,
        );

        if enforce {
            assert!(
                report.process_inplace_ns_per_sample <= report.process_into_ns_per_sample * 1.25,
                "process_inplace regressed beyond 25% for {} channels: process_inplace={:.3}, process_into={:.3}",
                channels,
                report.process_inplace_ns_per_sample,
                report.process_into_ns_per_sample,
            );
        }
    }
}

struct ConvolverReport {
    process_into_ns_per_sample: f64,
    process_inplace_ns_per_sample: f64,
    legacy_process_into_ns_per_sample: f64,
    legacy_process_inplace_ns_per_sample: f64,
    allocating_process_ns_per_sample: f64,
    process_into_speedup_percent: f64,
    process_inplace_speedup_percent: f64,
    wrapper_overhead_percent: f64,
}

fn benchmark_convolver(
    channels: usize,
    ir_frames: usize,
    frames: usize,
    iterations: usize,
    trials: usize,
) -> ConvolverReport {
    let trials = trials.max(1);
    let mut process_into = Vec::with_capacity(trials);
    let mut process_inplace = Vec::with_capacity(trials);
    let mut legacy_process_into = Vec::with_capacity(trials);
    let mut legacy_process_inplace = Vec::with_capacity(trials);
    let mut allocating_process = Vec::with_capacity(trials);

    for _ in 0..trials {
        let report = benchmark_convolver_once(channels, ir_frames, frames, iterations);
        process_into.push(report.process_into_ns_per_sample);
        process_inplace.push(report.process_inplace_ns_per_sample);
        legacy_process_into.push(report.legacy_process_into_ns_per_sample);
        legacy_process_inplace.push(report.legacy_process_inplace_ns_per_sample);
        allocating_process.push(report.allocating_process_ns_per_sample);
    }

    let process_into_ns_per_sample = median(&mut process_into);
    let process_inplace_ns_per_sample = median(&mut process_inplace);
    let legacy_process_into_ns_per_sample = median(&mut legacy_process_into);
    let legacy_process_inplace_ns_per_sample = median(&mut legacy_process_inplace);
    let allocating_process_ns_per_sample = median(&mut allocating_process);

    ConvolverReport {
        process_into_ns_per_sample,
        process_inplace_ns_per_sample,
        legacy_process_into_ns_per_sample,
        legacy_process_inplace_ns_per_sample,
        allocating_process_ns_per_sample,
        process_into_speedup_percent: speedup_percent(
            legacy_process_into_ns_per_sample,
            process_into_ns_per_sample,
        ),
        process_inplace_speedup_percent: speedup_percent(
            legacy_process_inplace_ns_per_sample,
            process_inplace_ns_per_sample,
        ),
        wrapper_overhead_percent: (allocating_process_ns_per_sample - process_into_ns_per_sample)
            / process_into_ns_per_sample
            * 100.0,
    }
}

fn benchmark_convolver_once(
    channels: usize,
    ir_frames: usize,
    frames: usize,
    iterations: usize,
) -> ConvolverReport {
    let ir = synthetic_ir(ir_frames, channels);
    let input = synthetic_input(frames, channels);
    let mut output = vec![0.0; input.len()];
    let mut inplace_buffer = input.clone();
    let mut legacy_output = vec![0.0; input.len()];
    let mut legacy_inplace_buffer = input.clone();

    let mut into_conv = FFTConvolver::new(&ir, channels);
    let mut inplace_conv = FFTConvolver::new(&ir, channels);
    let mut allocating_conv = FFTConvolver::new(&ir, channels);
    let mut legacy_into_conv = LegacyConvolver::new(&ir, channels);
    let mut legacy_inplace_conv = LegacyConvolver::new(&ir, channels);

    let into_duration = measure(
        || {
            into_conv.process_into(black_box(&input), black_box(&mut output));
            black_box(output[0])
        },
        iterations,
    );

    let legacy_into_duration = measure(
        || {
            legacy_into_conv.process_into(black_box(&input), black_box(&mut legacy_output));
            black_box(legacy_output[0])
        },
        iterations,
    );

    let inplace_duration = measure(
        || {
            inplace_buffer.copy_from_slice(&input);
            inplace_conv.process_inplace(black_box(&mut inplace_buffer));
            black_box(inplace_buffer[0])
        },
        iterations,
    );

    let legacy_inplace_duration = measure(
        || {
            legacy_inplace_buffer.copy_from_slice(&input);
            legacy_inplace_conv.process_inplace(black_box(&mut legacy_inplace_buffer));
            black_box(legacy_inplace_buffer[0])
        },
        iterations,
    );

    let allocating_duration = measure(
        || {
            let output = allocating_conv.process(black_box(&input));
            black_box(output[0])
        },
        iterations,
    );

    let samples = frames * channels * iterations;
    let process_into_ns_per_sample = nanos_per_unit(into_duration, samples);
    let process_inplace_ns_per_sample = nanos_per_unit(inplace_duration, samples);
    let legacy_process_into_ns_per_sample = nanos_per_unit(legacy_into_duration, samples);
    let legacy_process_inplace_ns_per_sample = nanos_per_unit(legacy_inplace_duration, samples);
    let allocating_process_ns_per_sample = nanos_per_unit(allocating_duration, samples);

    ConvolverReport {
        process_into_ns_per_sample,
        process_inplace_ns_per_sample,
        legacy_process_into_ns_per_sample,
        legacy_process_inplace_ns_per_sample,
        allocating_process_ns_per_sample,
        process_into_speedup_percent: speedup_percent(
            legacy_process_into_ns_per_sample,
            process_into_ns_per_sample,
        ),
        process_inplace_speedup_percent: speedup_percent(
            legacy_process_inplace_ns_per_sample,
            process_inplace_ns_per_sample,
        ),
        wrapper_overhead_percent: (allocating_process_ns_per_sample - process_into_ns_per_sample)
            / process_into_ns_per_sample
            * 100.0,
    }
}

fn measure<T>(mut run: impl FnMut() -> T, iterations: usize) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(run());
    }
    start.elapsed()
}

fn nanos_per_unit(duration: Duration, units: usize) -> f64 {
    duration.as_nanos() as f64 / units as f64
}

fn speedup_percent(legacy: f64, current: f64) -> f64 {
    (legacy - current) / legacy * 100.0
}

fn parse_trials(args: &[String]) -> Option<usize> {
    args.iter()
        .find_map(|arg| arg.strip_prefix("--trials="))
        .and_then(|value| value.parse::<usize>().ok())
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|left, right| left.total_cmp(right));
    values[values.len() / 2]
}

fn synthetic_ir(frames: usize, channels: usize) -> Vec<f64> {
    let mut ir = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        let decay = (-(frame as f64) / 64.0).exp();
        for ch in 0..channels {
            let phase = (ch + 1) as f64 * 0.11;
            let tap = ((frame as f64 + 1.0) * phase).sin() * decay * 0.08;
            ir.push(if frame == 0 { 1.0 } else { tap });
        }
    }
    ir
}

fn synthetic_input(frames: usize, channels: usize) -> Vec<f64> {
    let mut seed = 0x0BAD_5EED_u64;
    let mut out = Vec::with_capacity(frames * channels);

    for frame in 0..frames {
        let t = frame as f64 / SAMPLE_RATE;
        let sine = (2.0 * std::f64::consts::PI * 997.0 * t).sin() * 0.25;
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let noise = (((seed >> 33) as f64 / u32::MAX as f64) * 2.0 - 1.0) * 0.03;
        for ch in 0..channels {
            out.push((sine + noise) * (1.0 - ch as f64 * 0.015));
        }
    }

    out
}

/// Bench-local copy of the pre-hotpath-refactor implementation.
///
/// Keep this type private to the harness so production code stays on the current
/// implementation while the perf task retains a stable legacy baseline.
struct LegacyConvolver {
    fft_size: usize,
    impulse_response_fft: Vec<Vec<Complex<f64>>>,
    overlap_buffers: Vec<Vec<f64>>,
    channels: usize,
    ir_len: usize,
    fft_forward: Arc<dyn rustfft::Fft<f64>>,
    fft_inverse: Arc<dyn rustfft::Fft<f64>>,
    scratch_complex: Vec<Complex<f64>>,
}

impl LegacyConvolver {
    fn new(ir_data: &[f64], channels: usize) -> Self {
        let ir_len_total = ir_data.len();
        let ir_len_per_ch = ir_len_total / channels;

        let mut fft_size = 1;
        while fft_size < (ir_len_per_ch * 2) {
            fft_size <<= 1;
        }

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        let fft_forward = planner.plan_fft_forward(fft_size);
        let fft_inverse = planner.plan_fft_inverse(fft_size);

        let mut ir_ffts = Vec::with_capacity(channels);
        let mut overlap_bufs = Vec::with_capacity(channels);

        for ch in 0..channels {
            let mut buffer = vec![Complex::new(0.0, 0.0); fft_size];
            for i in 0..ir_len_per_ch {
                buffer[i] = Complex::new(ir_data[i * channels + ch], 0.0);
            }
            fft.process(&mut buffer);
            ir_ffts.push(buffer);
            overlap_bufs.push(vec![0.0; ir_len_per_ch - 1]);
        }

        Self {
            fft_size,
            impulse_response_fft: ir_ffts,
            overlap_buffers: overlap_bufs,
            channels,
            ir_len: ir_len_per_ch,
            fft_forward,
            fft_inverse,
            scratch_complex: vec![Complex::new(0.0, 0.0); fft_size],
        }
    }

    #[allow(clippy::needless_range_loop)]
    fn process_into(&mut self, input: &[f64], output: &mut [f64]) {
        debug_assert_eq!(input.len(), output.len());

        let channels = self.channels;
        let total_frames = input.len() / channels;
        let fft_size = self.fft_size;
        let ir_len = self.ir_len;
        let step_size = fft_size - ir_len + 1;
        let inv_n = 1.0 / fft_size as f64;

        output.fill(0.0);

        for ch in 0..channels {
            let mut processed_frames = 0;

            while processed_frames < total_frames {
                let chunk_len = std::cmp::min(step_size, total_frames - processed_frames);
                let scratch = &mut self.scratch_complex;
                scratch.fill(Complex::new(0.0, 0.0));

                for i in 0..ir_len - 1 {
                    scratch[i] = Complex::new(self.overlap_buffers[ch][i], 0.0);
                }

                for i in 0..chunk_len {
                    scratch[i + ir_len - 1] =
                        Complex::new(input[(processed_frames + i) * channels + ch], 0.0);
                }

                self.fft_forward.process(scratch);

                let ir_fft = &self.impulse_response_fft[ch];
                for i in 0..fft_size {
                    scratch[i] *= ir_fft[i];
                }

                self.fft_inverse.process(scratch);

                for i in 0..chunk_len {
                    output[(processed_frames + i) * channels + ch] =
                        scratch[i + ir_len - 1].re * inv_n;
                }

                let overlap = &mut self.overlap_buffers[ch];
                if chunk_len >= ir_len - 1 {
                    for i in 0..ir_len - 1 {
                        overlap[i] = input
                            [(processed_frames + chunk_len - (ir_len - 1) + i) * channels + ch];
                    }
                } else {
                    let shift = chunk_len;
                    let keep = ir_len - 1 - shift;
                    for i in 0..keep {
                        overlap[i] = overlap[i + shift];
                    }
                    for i in 0..shift {
                        overlap[keep + i] = input[(processed_frames + i) * channels + ch];
                    }
                }

                processed_frames += chunk_len;
            }
        }
    }

    #[allow(clippy::needless_range_loop)]
    fn process_inplace(&mut self, buf: &mut [f64]) {
        let channels = self.channels;
        let total_frames = buf.len() / channels;
        let fft_size = self.fft_size;
        let ir_len = self.ir_len;
        let step_size = fft_size - ir_len + 1;
        let inv_n = 1.0 / fft_size as f64;

        for ch in 0..channels {
            let mut processed_frames = 0;

            while processed_frames < total_frames {
                let chunk_len = std::cmp::min(step_size, total_frames - processed_frames);
                let scratch = &mut self.scratch_complex;
                scratch.fill(Complex::new(0.0, 0.0));

                for i in 0..ir_len - 1 {
                    scratch[i] = Complex::new(self.overlap_buffers[ch][i], 0.0);
                }

                for i in 0..chunk_len {
                    scratch[i + ir_len - 1] =
                        Complex::new(buf[(processed_frames + i) * channels + ch], 0.0);
                }

                self.fft_forward.process(scratch);

                let ir_fft = &self.impulse_response_fft[ch];
                for i in 0..fft_size {
                    scratch[i] *= ir_fft[i];
                }

                self.fft_inverse.process(scratch);

                let overlap = &mut self.overlap_buffers[ch];
                if chunk_len >= ir_len - 1 {
                    for i in 0..ir_len - 1 {
                        overlap[i] =
                            buf[(processed_frames + chunk_len - (ir_len - 1) + i) * channels + ch];
                    }
                } else {
                    let shift = chunk_len;
                    let keep = ir_len - 1 - shift;
                    for i in 0..keep {
                        overlap[i] = overlap[i + shift];
                    }
                    for i in 0..shift {
                        overlap[keep + i] = buf[(processed_frames + i) * channels + ch];
                    }
                }

                for i in 0..chunk_len {
                    buf[(processed_frames + i) * channels + ch] =
                        scratch[i + ir_len - 1].re * inv_n;
                }

                processed_frames += chunk_len;
            }
        }
    }
}
