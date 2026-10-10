//! Bounded supplemental probes, independent of the full comparison schedule.

use std::collections::BTreeSet;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use serde_json::{json, Value};

use super::quality::{analysis_window, render_signal, RenderedSignal};
use super::signal::{channel_rms, db_ratio, fit_components, tone_sample};
use super::{
    adapters, latency, ComparisonArgs, EngineFactory, PreparedInputs, RatePair, RATE_PAIRS,
};
use crate::support::{generated_unix_ms, pin_current_thread, write_json, BenchEnvironment};

const INPUT_FRAMES: usize = 16_384;
const CHUNK: usize = 512;

pub(super) fn run(argv: Vec<String>) -> Result<(), String> {
    let mut remaining = Vec::new();
    let mut engines = BTreeSet::from(["audio_engine_core".to_string()]);
    let mut rates = BTreeSet::from([RATE_PAIRS[0].id.to_string()]);
    for arg in argv {
        if arg == "--diagnostics" {
            continue;
        } else if let Some(ids) = arg.strip_prefix("--diagnostic-engines=") {
            engines = ids.split(',').map(str::to_string).collect();
            for id in &engines {
                super::validate_engine_id(id)?;
            }
        } else if let Some(ids) = arg.strip_prefix("--diagnostic-rates=") {
            rates = ids.split(',').map(str::to_string).collect();
            if rates
                .iter()
                .any(|id| !RATE_PAIRS.iter().any(|r| r.id == id))
            {
                return Err("unknown diagnostic rate id".into());
            }
        } else {
            remaining.push(arg);
        }
    }
    let args = ComparisonArgs::parse(remaining)?;
    if args.perf.help {
        println!("--diagnostics [--diagnostic-engines=id,id] [--diagnostic-rates=id,id] [--pinned --pin-core 2] [--out report.json] [--engine-library id=path]");
        return Ok(());
    }
    if args.perf.enforce || args.perf.baseline.is_some() || args.require_complete_matrix {
        return Err("supplemental diagnostics are report-only; full-matrix/baseline/enforce flags do not apply".into());
    }
    let discovery = adapters::discover(
        args.libsamplerate_path.as_deref(),
        &args.native_library_paths,
        &args.required_engines,
    );
    for id in engines.iter().chain(&args.required_engines) {
        if !discovery
            .factories
            .iter()
            .any(|f| &f.identity().engine_id == id)
        {
            return Err(format!("diagnostic engine unavailable: {id}"));
        }
    }
    let scheduling = if args.pinned {
        Some(pin_current_thread(args.pin_core)?)
    } else {
        None
    };
    audio_engine_core::runtime::audio_thread_init();
    let mut cases = Vec::new();
    let mut failures = 0;
    for factory in &discovery.factories {
        if !engines.contains(&factory.identity().engine_id) {
            continue;
        }
        for rate in RATE_PAIRS.into_iter().filter(|r| rates.contains(r.id)) {
            let result = if factory.supports_rate(rate.id) {
                measure(factory, rate)
            } else {
                Err("selected rate is not supported by this adapter".into())
            };
            let mut case = match result {
                Ok(case) => {
                    if case["valid"] != true {
                        failures += 1;
                    }
                    case
                }
                Err(error) => {
                    failures += 1;
                    json!({"valid": false, "error": error})
                }
            };
            case["engine"] = json!(factory.identity());
            case["rate"] = json!({"id": rate.id, "from_hz": rate.from_hz, "to_hz": rate.to_hz, "channels": rate.channels});
            eprintln!(
                "diagnostics {} {} valid={}",
                factory.identity().engine_id,
                rate.id,
                case["valid"]
            );
            cases.push(case);
        }
    }
    let report = json!({
        "schema": "resampler_supplemental_diagnostics_v1", "generated_unix_ms": generated_unix_ms(),
        "environment": BenchEnvironment::capture(), "scheduling": scheduling,
        "conditions": {
            "input_frames": INPUT_FRAMES, "quality_chunk_frames": CHUNK,
            "signal_channel_policy": "identical excitation on all channels; spectral and impulse metrics use channel 0; finite/complete checks cover all output",
            "tone_levels": [0.999, 0.5, 1e-3, 1e-6], "tone_phase_radians": 0.371,
            "group_delay": "negative principal phase difference / (2*pi*1 Hz); residual output-timeline delay, unambiguous only within +/-0.5 seconds; not API buffering or device latency",
            "ringing": "energy before/after impulse peak, excludes the peak; settling extent uses -100 dB relative peak; finite record only",
            "dense_sweeps": "49 single-phase transition samples from 18 kHz to shared Nyquist minus 1 Hz; 33 quadrature alias samples above destination Nyquist for downsampling; finite samples, not continuous bounds",
            "latency": "2048 separately timed calls per 64/128/512-frame chunk; includes timer overhead; excludes device scheduling",
            "scope": "report-only selected High/Linear recipes; complements existing response/image/DC/step/channel grid; not a complete quality acceptance"
        }, "cases": cases, "failed_cases": failures
    });
    if let Some(path) = args.perf.out.as_deref() {
        write_json(path, &report, "resampler diagnostics")?;
    } else {
        println!("{report}");
    }
    if failures != 0 {
        return Err(format!("{failures} diagnostic cases failed; see report"));
    }
    Ok(())
}

struct Probe<'a> {
    factory: &'a EngineFactory,
    rate: RatePair,
    capacity: usize,
    renders: usize,
    samples: usize,
}

impl Probe<'_> {
    fn render(&mut self, sample: impl Fn(usize, usize) -> f64) -> Result<RenderedSignal, String> {
        let result = render_signal(
            self.factory,
            self.rate,
            self.rate.channels,
            CHUNK,
            INPUT_FRAMES,
            self.capacity,
            sample,
        )?;
        self.renders += 1;
        self.samples += result.samples.len();
        if !result.finite || !result.terminal || result.actual_frames != result.expected_frames {
            return Err(format!(
                "render {} invalid: finite={}, terminal={}, frames={}/{}",
                self.renders,
                result.finite,
                result.terminal,
                result.actual_frames,
                result.expected_frames
            ));
        }
        Ok(result)
    }

    fn tone(&mut self, hz: u32, amplitude: f64, phase: f64) -> Result<RenderedSignal, String> {
        let rate = self.rate.from_hz;
        self.render(|n, _| tone_sample(n, rate, hz, amplitude, phase))
    }
}

fn measure(factory: &EngineFactory, rate: RatePair) -> Result<Value, String> {
    let capacity = factory
        .create(rate, rate.channels, CHUNK)?
        .max_output_frames()
        .max(1);
    let mut probe = Probe {
        factory,
        rate,
        capacity,
        renders: 0,
        samples: 0,
    };
    let impulse = probe.render(|n, _| if n == INPUT_FRAMES / 2 { 1.0 } else { 0.0 })?;
    let mono: Vec<_> = impulse
        .samples
        .chunks_exact(rate.channels)
        .map(|f| f[0])
        .collect();
    let expected_center =
        (INPUT_FRAMES / 2) as f64 * f64::from(rate.to_hz) / f64::from(rate.from_hz);
    let ringing = impulse_metrics(&mono, expected_center)?;
    let leading = ringing["peak_delay_output_frames"]
        .as_f64()
        .unwrap()
        .max(0.0)
        .ceil() as usize;
    let nominal = super::rounded_output_frames(INPUT_FRAMES, rate.from_hz, rate.to_hz);
    let (start, count) = analysis_window(&impulse, nominal, rate.to_hz, 1024, leading)
        .ok_or("insufficient diagnostic interior")?;
    let mut levels = Vec::new();
    for hz in [997, 18_000] {
        for amplitude in [0.999, 0.5, 1e-3, 1e-6] {
            let rendered = probe.tone(hz, amplitude, 0.371)?;
            let fit = fit_components(
                &rendered.samples,
                rate.channels,
                0,
                rate.to_hz,
                &[hz],
                start,
                count,
            )?;
            let component = &fit.components[0];
            levels.push(json!({"input_hz": hz, "amplitude": amplitude,
                "gain_db": db_ratio(component.amplitude, amplitude)?,
                "residual_rms": fit.residual_rms, "residual_dbfs": db_ratio(fit.residual_rms, 1.0)?,
                "thdn_db": db_ratio(fit.residual_rms, component.rms)?,
                "full_output_peak": rendered.samples.iter().fold(0.0f64, |m, v| m.max(v.abs()))}));
        }
    }
    let mut delays = Vec::new();
    for hz in [1000, 18_000] {
        let mut phases = Vec::new();
        for f in [hz, hz + 1] {
            let rendered = probe.tone(f, 0.5, 0.371)?;
            phases.push(
                fit_components(
                    &rendered.samples,
                    rate.channels,
                    0,
                    rate.to_hz,
                    &[f],
                    start,
                    count,
                )?
                .components[0]
                    .phase_radians
                    .ok_or("undefined group-delay phase")?,
            );
        }
        let seconds = group_delay_seconds(phases[0], phases[1], 1.0);
        delays.push(
            json!({"frequency_pair_hz": [hz, hz+1], "phases_radians": phases,
            "seconds": seconds, "output_frames": seconds * f64::from(rate.to_hz)}),
        );
    }
    let mut transition = Vec::new();
    let shared_nyquist = rate.from_hz.min(rate.to_hz) / 2;
    for hz in grid(18_000, shared_nyquist - 1, 49) {
        let rendered = probe.tone(hz, 0.5, 0.371)?;
        let fit = fit_components(
            &rendered.samples,
            rate.channels,
            0,
            rate.to_hz,
            &[hz],
            start,
            count,
        )?;
        transition
            .push(json!({"input_hz": hz, "gain_db": db_ratio(fit.components[0].amplitude, 0.5)?}));
    }
    let mut stopband = Vec::new();
    if rate.from_hz > rate.to_hz {
        for hz in grid(rate.to_hz / 2 + 1, rate.from_hz / 2 - 1, 33) {
            let mut power = 0.0;
            for phase in [0.0, FRAC_PI_2] {
                let rendered = probe.tone(hz, 0.5, phase)?;
                let rms = channel_rms(&rendered.samples, rate.channels, 0, start, count)?;
                power += rms * rms;
            }
            stopband.push(
                json!({"input_hz": hz, "quadrature_total_output_db": db_ratio(power.sqrt(), 0.5)?}),
            );
        }
    }
    let silence = probe.render(|_, _| 0.0)?;
    let silence_peak = silence.samples.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let mut tails = Vec::new();
    let mut tails_valid = true;
    for chunk in [64, 128, 512] {
        let inputs = PreparedInputs::with_chunk(rate, chunk);
        let capacity = factory
            .create(rate, rate.channels, chunk)?
            .max_output_frames()
            .max(1);
        match latency::measure(factory, rate, &inputs, 2048, capacity) {
            Ok(tail) => {
                tails_valid &= tail.valid;
                tails.push(json!(tail));
            }
            Err(error) => {
                tails_valid = false;
                tails.push(json!({"valid": false, "chunk_frames": chunk, "error": error}));
            }
        }
    }
    Ok(
        json!({"valid": tails_valid, "quality_valid": true, "renders_checked": probe.renders, "output_samples_checked": probe.samples,
        "output_capacity_frames": capacity, "analysis_start_frame": start, "analysis_frames": count,
        "complete_output_frames_per_render": impulse.actual_frames,
        "expected_output_frames_per_render": impulse.expected_frames,
        "multi_level": levels, "group_delay": delays, "centered_impulse": ringing,
        "transition": transition, "stopband": stopband, "silence_peak": silence_peak, "call_tails": tails}),
    )
}

fn grid(low: u32, high: u32, count: usize) -> BTreeSet<u32> {
    (0..count)
        .map(|i| low + ((high - low) as usize * i / (count - 1)) as u32)
        .collect()
}

fn group_delay_seconds(phase_low: f64, phase_high: f64, delta_hz: f64) -> f64 {
    -((phase_high - phase_low + PI).rem_euclid(TAU) - PI) / (TAU * delta_hz)
}

fn impulse_metrics(samples: &[f64], expected_center: f64) -> Result<Value, String> {
    if samples.iter().any(|x| !x.is_finite()) {
        return Err("non-finite impulse".into());
    }
    let peak = (0..samples.len())
        .max_by(|&a, &b| samples[a].abs().total_cmp(&samples[b].abs()))
        .ok_or("empty impulse")?;
    let energy: f64 = samples.iter().map(|x| x * x).sum();
    if energy <= 0.0 {
        return Err("zero-energy impulse".into());
    }
    let pre: f64 = samples[..peak].iter().map(|x| x * x).sum();
    let post: f64 = samples[peak + 1..].iter().map(|x| x * x).sum();
    let centroid = samples
        .iter()
        .enumerate()
        .map(|(i, x)| i as f64 * x * x)
        .sum::<f64>()
        / energy;
    let threshold = samples[peak].abs() * 1e-5;
    let first = samples.iter().position(|x| x.abs() >= threshold).unwrap();
    let last = samples.iter().rposition(|x| x.abs() >= threshold).unwrap();
    Ok(
        json!({"peak_frame": peak, "peak_amplitude": samples[peak], "total_energy": energy,
        "expected_center_output_frame": expected_center, "peak_delay_output_frames": peak as f64 - expected_center,
        "energy_centroid_output_frame": centroid, "centroid_delay_output_frames": centroid - expected_center,
        "pre_peak_energy_fraction": pre/energy, "post_peak_energy_fraction": post/energy,
        "settling_threshold_db_relative_peak": -100, "first_above_threshold_frame": first, "last_above_threshold_frame": last,
        "pre_extent_frames": peak-first, "post_extent_frames": last-peak,
        "threshold_extent_touches_record_edge": first == 0 || last+1 == samples.len()}),
    )
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::*;

    #[test]
    fn analytic_delay_crosses_phase_wrap() {
        let delay = 0.0037;
        let low = -PI + 0.001;
        let high = (low - TAU * delay + PI).rem_euclid(TAU) - PI;
        assert!((group_delay_seconds(low, high, 1.0) - delay).abs() < 1e-15);
        let signal: Vec<_> = (0..16_384)
            .map(|n| tone_sample(n, 48000, 1001, 0.5, -TAU * 1001.0 * delay))
            .collect();
        let fit = fit_components(&signal, 1, 0, 48000, &[1001], 2048, 8192).unwrap();
        let phase = fit.components[0].phase_radians.unwrap();
        assert!((group_delay_seconds(-TAU * 1000.0 * delay, phase, 1.0) - delay).abs() < 1e-14);
    }

    #[test]
    fn known_fir_energy_and_settling() {
        let mut fir = vec![0.0; 32];
        fir[9] = 0.25;
        fir[10] = 1.0;
        fir[11] = 0.5;
        let m = impulse_metrics(&fir, 8.0).unwrap();
        assert_eq!(m["peak_delay_output_frames"], 2.0);
        assert_eq!(m["pre_extent_frames"], 1);
        assert_eq!(m["post_extent_frames"], 1);
        assert!((m["pre_peak_energy_fraction"].as_f64().unwrap() - 1.0 / 21.0).abs() < 1e-15);
        assert!((m["post_peak_energy_fraction"].as_f64().unwrap() - 4.0 / 21.0).abs() < 1e-15);
        assert!((m["energy_centroid_output_frame"].as_f64().unwrap() - 213.0 / 21.0).abs() < 1e-14);
    }
}
