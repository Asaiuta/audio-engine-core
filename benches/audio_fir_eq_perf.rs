use std::hint::black_box;
use std::time::Instant;

use audio_engine_core::processor::{FFTConvolver, FirEq, FirPhaseMode};

const SAMPLE_RATE: f64 = 48_000.0;
const CHANNELS: usize = 2;

// Tap counts span the practical range: 511 is a light linear-phase EQ, 2047 is a
// high-resolution filter with noticeably more bins per regeneration.
const TAP_COUNTS: [usize; 3] = [511, 1023, 2047];

#[derive(Clone, Copy)]
enum Phase {
    Linear,
    Minimum,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Minimum => "minimum",
        }
    }

    fn mode(self) -> FirPhaseMode {
        match self {
            Self::Linear => FirPhaseMode::Linear,
            Self::Minimum => FirPhaseMode::Minimum,
        }
    }

    fn all() -> &'static [Self] {
        &[Self::Linear, Self::Minimum]
    }
}

struct RegenReport {
    ns_per_regen: f64,
    ir_length: usize,
}

struct ProcessReport {
    ns_per_sample: f64,
    ns_per_buffer: f64,
    fft_size: usize,
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let quick = args.iter().any(|arg| arg == "--quick");
    let enforce = args.iter().any(|arg| arg == "--enforce");

    let (regen_iterations, process_iterations, trials) = if quick {
        (200, 400, 1)
    } else {
        (1_000, 2_000, 3)
    };
    let process_frames = 512;

    println!(
        "audio_fir_eq_perf mode={} sample_rate={} channels={} process_frames={} coverage=fir_ir_generation+convolver_apply",
        if quick { "quick" } else { "full" },
        SAMPLE_RATE as u32,
        CHANNELS,
        process_frames
    );
    println!(
        "audio_fir_eq_note ir_generation_includes=fft_design,phase_shaping excludes=cpal_device_write,decoder; apply_path=FirEq_ir->FFTConvolver"
    );

    // Part 1: IR regeneration cost (the FFT-based filter design triggered whenever
    // bands, tap count, or phase mode change). This is the non-realtime control cost.
    for &phase in Phase::all() {
        for &taps in &TAP_COUNTS {
            let report = benchmark_regen(phase, taps, regen_iterations, trials);
            println!(
                "fir_eq_regen phase={} taps={} ir_length={} ns_per_regen={:.3} regen_per_ms={:.1}",
                phase.name(),
                taps,
                report.ir_length,
                report.ns_per_regen,
                1_000_000.0 / report.ns_per_regen
            );

            if enforce && matches!(phase, Phase::Linear) && taps == 1023 {
                assert!(
                    report.ns_per_regen.is_finite() && report.ns_per_regen > 0.0,
                    "FIR EQ regeneration produced invalid timing"
                );
            }
        }
    }

    // Part 2: end-to-end apply cost — the generated IR fed through FFTConvolver, which
    // is how FirEq is actually used in the playback path.
    for &taps in &TAP_COUNTS {
        let report = benchmark_apply(taps, process_frames, process_iterations, trials);
        println!(
            "fir_eq_apply taps={} fft_size={} frames={} samples={} ns_per_sample={:.3} ns_per_buffer={:.3}",
            taps,
            report.fft_size,
            process_frames,
            process_frames * CHANNELS,
            report.ns_per_sample,
            report.ns_per_buffer
        );

        if enforce && taps == 1023 {
            assert!(
                report.ns_per_sample.is_finite() && report.ns_per_sample > 0.0,
                "FIR EQ apply produced invalid timing"
            );
        }
    }
}

fn benchmark_regen(phase: Phase, taps: usize, iterations: usize, trials: usize) -> RegenReport {
    let mut best: Option<RegenReport> = None;

    for _ in 0..trials {
        let mut fir = FirEq::new(SAMPLE_RATE, taps);
        fir.set_phase_mode(phase.mode());

        // Warm: a couple of regenerations to settle FFT planner caches.
        fir.set_bands(&warm_curve());
        fir.set_bands(&STANDARD_TEST_CURVE);

        let start = Instant::now();
        for i in 0..iterations {
            // Alternate two curves so each iteration genuinely regenerates rather
            // than short-circuiting on identical input.
            let curve = if i % 2 == 0 {
                &STANDARD_TEST_CURVE
            } else {
                &ALT_TEST_CURVE
            };
            fir.set_bands(black_box(curve));
            black_box(fir.ir_length());
        }
        let elapsed = start.elapsed();

        let report = RegenReport {
            ns_per_regen: elapsed.as_nanos() as f64 / iterations as f64,
            ir_length: fir.ir_length(),
        };

        if best
            .as_ref()
            .is_none_or(|b| report.ns_per_regen < b.ns_per_regen)
        {
            best = Some(report);
        }
    }

    best.expect("at least one trial")
}

fn benchmark_apply(taps: usize, frames: usize, iterations: usize, trials: usize) -> ProcessReport {
    let mut best: Option<ProcessReport> = None;

    for _ in 0..trials {
        let mut fir = FirEq::new(SAMPLE_RATE, taps);
        fir.set_phase_mode(FirPhaseMode::Linear);
        fir.set_bands(&STANDARD_TEST_CURVE);

        let ir = fir.get_ir(CHANNELS);
        let mut convolver = FFTConvolver::new(&ir, CHANNELS);
        let fft_size = convolver.fft_size();

        let input = synthetic_input(frames, CHANNELS);
        let mut output = vec![0.0; input.len()];

        // Warm the overlap-add state.
        for _ in 0..64 {
            convolver.process_into(&input, &mut output);
        }

        let start = Instant::now();
        for _ in 0..iterations {
            convolver.process_into(black_box(&input), black_box(&mut output));
            black_box(output[0]);
        }
        let elapsed = start.elapsed();

        let ns_per_buffer = elapsed.as_nanos() as f64 / iterations as f64;
        let report = ProcessReport {
            ns_per_sample: ns_per_buffer / (frames * CHANNELS) as f64,
            ns_per_buffer,
            fft_size,
        };

        if best
            .as_ref()
            .is_none_or(|b| report.ns_per_sample < b.ns_per_sample)
        {
            best = Some(report);
        }
    }

    best.expect("at least one trial")
}

/// A non-flat curve with boosts and cuts across the 10 ISO bands.
const STANDARD_TEST_CURVE: [f64; 10] = [6.0, 4.0, 2.0, 0.0, -2.0, -3.0, -1.5, 1.0, 3.5, 5.0];

/// A different non-flat curve, alternated with the standard one so successive
/// regenerations do real work.
const ALT_TEST_CURVE: [f64; 10] = [-5.0, -3.0, 0.0, 2.5, 4.0, 3.0, 1.0, -1.0, -2.5, -4.0];

fn warm_curve() -> [f64; 10] {
    [1.0; 10]
}

fn synthetic_input(frames: usize, channels: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    let mut left_phase = 0.0_f64;
    let mut right_phase = 0.0_f64;

    for frame in 0..frames {
        let t = frame as f64 / SAMPLE_RATE;
        left_phase += std::f64::consts::TAU * (220.0 + 11.0 * (t * 3.0).sin()) / SAMPLE_RATE;
        right_phase += std::f64::consts::TAU * (330.0 + 7.0 * (t * 5.0).cos()) / SAMPLE_RATE;
        let envelope = 0.65 + 0.20 * (std::f64::consts::TAU * 1.7 * t).sin();
        let left = (left_phase.sin() * 0.55 + (left_phase * 3.0).sin() * 0.08) * envelope;
        let right = (right_phase.sin() * 0.50 - (right_phase * 2.0).cos() * 0.07) * envelope;

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
