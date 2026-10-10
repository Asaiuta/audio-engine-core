//! Explicit report-only spectral, channel and transient probe coverage.

use std::collections::BTreeSet;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use serde::{Deserialize, Serialize};

use super::adapters::EngineFactory;
use super::quality::{analysis_window, render_signal, RenderedSignal};
use super::signal::{
    channel_rms, db_ratio, fit_components, fold_frequency, tone_sample, DB_DISPLAY_FLOOR,
};
use super::{rounded_output_frames, MetricClassification, QualitySummary, RatePair, SampleFormat};

const AMPLITUDE: f64 = 0.5;
const PHASES: [f64; 2] = [0.0, FRAC_PI_2];
const PASSBAND_LIMIT_HZ: u32 = 18_000;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct QualityGrid {
    pub(crate) input_amplitude: f64,
    pub(crate) input_phases_radians: Vec<f64>,
    pub(crate) passband_limit_hz: u32,
    pub(crate) db_display_floor: f64,
    pub(crate) analysis_method: String,
    pub(crate) analysis_window_policy: String,
    pub(crate) analyzer_reference_scope: String,
    pub(crate) renders_checked: usize,
    pub(crate) output_samples_checked: usize,
    pub(crate) response: Vec<ResponsePoint>,
    pub(crate) aliases: Vec<AliasPoint>,
    pub(crate) images: Vec<ImagePoint>,
    pub(crate) channel_isolation: Vec<ChannelPoint>,
    pub(crate) dc_gain_db_by_channel: Vec<Option<f64>>,
    pub(crate) positive_step_full_peak_by_channel: Vec<f64>,
    pub(crate) negative_step_full_peak_by_channel: Vec<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ResponsePoint {
    pub(crate) input_hz: u32,
    pub(crate) band: String,
    pub(crate) channel: usize,
    pub(crate) input_phase_radians: f64,
    pub(crate) analysis_start_frame: usize,
    pub(crate) analysis_frames: usize,
    pub(crate) gain_db: Option<f64>,
    pub(crate) phase_error_radians: Option<f64>,
    pub(crate) thdn_db: Option<f64>,
    pub(crate) residual_rms: Option<f64>,
    pub(crate) analyzer_reference_thdn_db: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct AliasPoint {
    pub(crate) input_hz: u32,
    pub(crate) folded_hz: u32,
    pub(crate) channel: usize,
    pub(crate) analysis_start_frame: usize,
    pub(crate) analysis_frames: usize,
    pub(crate) quadrature_total_output_db: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ImagePoint {
    pub(crate) input_hz: u32,
    pub(crate) image_hz: u32,
    pub(crate) channel: usize,
    pub(crate) analysis_start_frame: usize,
    pub(crate) analysis_frames: usize,
    pub(crate) quadrature_image_db: Option<f64>,
    pub(crate) analyzer_reference_image_db: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ChannelPoint {
    pub(crate) input_channel: usize,
    pub(crate) output_channel: usize,
    pub(crate) input_hz: u32,
    pub(crate) quadrature_output_db: Option<f64>,
}

struct Probe<'a> {
    factory: &'a EngineFactory,
    rate: RatePair,
    channels: usize,
    chunk: usize,
    input_frames: usize,
    capacity: usize,
    nominal: usize,
    delay: usize,
}

impl Probe<'_> {
    fn render(&self, sample: impl Fn(usize, usize) -> f64) -> Result<RenderedSignal, String> {
        render_signal(
            self.factory,
            self.rate,
            self.channels,
            self.chunk,
            self.input_frames,
            self.capacity,
            sample,
        )
    }

    fn tone(
        &self,
        hz: u32,
        phase: f64,
        active_channel: Option<usize>,
    ) -> Result<RenderedSignal, String> {
        self.render(|n, ch| {
            if active_channel.is_none_or(|active| active == ch) {
                tone_sample(n, self.rate.from_hz, hz, AMPLITUDE, phase)
            } else {
                0.0
            }
        })
    }

    fn window(&self, rendered: &RenderedSignal) -> Result<(usize, usize), String> {
        analysis_window(rendered, self.nominal, self.rate.to_hz, 32, self.delay)
            .ok_or_else(|| "insufficient interior output for quality analysis".to_string())
    }

    fn reference(&self, frames: usize, hz: u32, phase: f64) -> Vec<f64> {
        (0..frames)
            .map(|n| {
                // A non-basis-aligned phase exercises coefficient fitting as well
                // as generation. This self-control is not an external error bound.
                let value = tone_sample(n, self.rate.to_hz, hz, AMPLITUDE, phase + 0.371);
                match self.factory.identity().sample_format {
                    SampleFormat::InterleavedF64 => value,
                    SampleFormat::InterleavedF32 => f64::from(value as f32),
                }
            })
            .collect()
    }
}

#[derive(Default)]
struct Audit {
    errors: Vec<String>,
    renders: usize,
    samples: usize,
    non_finite: bool,
}

impl Audit {
    fn observe(&mut self, label: &str, rendered: &RenderedSignal, impulse: &RenderedSignal) {
        self.renders += 1;
        self.samples += rendered.samples.len();
        self.non_finite |= !rendered.finite;
        if !rendered.finite {
            self.errors
                .push(format!("{label} output contains non-finite samples"));
        }
        if rendered.actual_frames != rendered.expected_frames {
            self.errors.push(format!(
                "{label} complete output has {} frames, expected {}",
                rendered.actual_frames, rendered.expected_frames
            ));
        }
        if rendered.expected_frames != impulse.expected_frames
            || rendered.reported_api_buffering_latency_frames
                != impulse.reported_api_buffering_latency_frames
            || rendered.observed_input_frames_before_first_output
                != impulse.observed_input_frames_before_first_output
            || rendered.observed_input_frames_through_first_output
                != impulse.observed_input_frames_through_first_output
        {
            self.errors.push(format!(
                "{label} duration or startup accounting differs from impulse"
            ));
        }
        if !rendered.terminal {
            self.errors.push(format!("{label} drain did not terminate"));
        }
    }

    fn metric<T>(&mut self, label: &str, result: Result<T, String>) -> Option<T> {
        result
            .map_err(|error| self.errors.push(format!("{label}: {error}")))
            .ok()
    }
}

fn response_frequencies(rate: RatePair) -> BTreeSet<u32> {
    let nyquist = rate.from_hz.min(rate.to_hz) / 2;
    let mut grid = [20, 100, 997, 5_000, 10_000, 15_000, 18_000]
        .into_iter()
        .collect::<BTreeSet<_>>();
    for percent in [85, 90, 95, 98, 99] {
        grid.insert(nyquist * percent / 100);
    }
    grid.insert(nyquist - 1);
    grid.retain(|&hz| hz > 0 && hz < nyquist);
    grid
}

fn alias_frequencies(rate: RatePair) -> BTreeSet<u32> {
    if rate.to_hz >= rate.from_hz {
        return BTreeSet::new();
    }
    let start = rate.to_hz / 2;
    let end = rate.from_hz / 2;
    let mut grid = BTreeSet::from([start, start + 1, end]);
    for percent in [1, 10, 25, 50, 75, 95, 99] {
        grid.insert(start + (end - start) * percent / 100);
    }
    // Retain the historical 23 kHz probe and exact/near-DC folds where legal.
    for hz in [23_000, rate.to_hz - 1, rate.to_hz, rate.to_hz + 1] {
        if hz >= start && hz <= end {
            grid.insert(hz);
        }
    }
    grid
}

fn image_frequencies(rate: RatePair, input_hz: u32) -> BTreeSet<u32> {
    let mut grid = BTreeSet::new();
    for multiple in 1..=rate.to_hz / rate.from_hz + 1 {
        let center = multiple * rate.from_hz;
        for hz in [center.saturating_sub(input_hz), center + input_hz] {
            if hz > rate.from_hz / 2 && hz <= rate.to_hz / 2 {
                grid.insert(hz);
            }
        }
    }
    grid
}

pub(crate) fn measure_quality(
    factory: &EngineFactory,
    rate: RatePair,
    channels: usize,
    chunk: usize,
    input_frames: usize,
    capacity: usize,
) -> Result<QualitySummary, String> {
    if channels == 0 || chunk == 0 || input_frames == 0 || capacity == 0 {
        return Err("quality workload requires non-zero geometry".to_string());
    }
    let mut probe = Probe {
        factory,
        rate,
        channels,
        chunk,
        input_frames,
        capacity,
        nominal: rounded_output_frames(input_frames, rate.from_hz, rate.to_hz),
        delay: 0,
    };
    let impulse = probe.render(|n, _| f64::from(n == 0))?;
    let mut audit = Audit::default();
    audit.observe("impulse", &impulse, &impulse);
    let (peak_frame, peak) = impulse
        .samples
        .chunks_exact(channels)
        .enumerate()
        .map(|(i, row)| (i, row[0].abs()))
        .filter(|(_, peak)| peak.is_finite())
        .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .unwrap_or((0, 0.0));
    if peak < 1.0e-9 {
        audit.errors.push(format!(
            "impulse peak magnitude {peak} is below analyzable threshold"
        ));
    }
    probe.delay = peak_frame;
    let mut grid = QualityGrid {
        input_amplitude: AMPLITUDE, input_phases_radians: PHASES.to_vec(),
        passband_limit_hz: PASSBAND_LIMIT_HZ, db_display_floor: DB_DISPLAY_FLOOR,
        analysis_method: "bounded_integer_hz_phase;compensated_normal_equations;tone_plus_dc;images_joint_fundamental_image_dc;aliases_quadrature_total_rms".to_string(),
        analysis_window_policy: "guard=min(max(output_rate/10,4096),usable_frames/4);absolute_output_sample_origin;no_gain_or_delay_alignment".to_string(),
        analyzer_reference_scope: "analytic output-rate tone with phase offset and native-format quantization;self_control_not_external_accuracy_bound;display_floor_is_not_resolution".to_string(),
        ..QualityGrid::default()
    };
    for hz in response_frequencies(rate) {
        for phase in PHASES {
            let rendered = probe.tone(hz, phase, None)?;
            audit.observe("response", &rendered, &impulse);
            let (start, count) = probe.window(&rendered)?;
            let reference = probe.reference(rendered.actual_frames, hz, phase);
            let reference_fit = fit_components(&reference, 1, 0, rate.to_hz, &[hz], start, count)?;
            let reference_thdn =
                db_ratio(reference_fit.residual_rms, reference_fit.components[0].rms).ok();
            for channel in 0..channels {
                let fit = audit.metric(
                    &format!("{hz} Hz analysis failed (channel {channel})"),
                    fit_components(
                        &rendered.samples,
                        channels,
                        channel,
                        rate.to_hz,
                        &[hz],
                        start,
                        count,
                    ),
                );
                if hz <= PASSBAND_LIMIT_HZ
                    && fit
                        .as_ref()
                        .is_some_and(|fit| fit.components[0].amplitude < AMPLITUDE * 1.0e-4)
                {
                    audit.errors.push(format!("{hz} Hz analysis failed: passband amplitude is below analyzable threshold on channel {channel}"));
                }
                let gain = fit
                    .as_ref()
                    .and_then(|fit| db_ratio(fit.components[0].amplitude, AMPLITUDE).ok());
                grid.response.push(ResponsePoint {
                    input_hz: hz,
                    band: if hz <= PASSBAND_LIMIT_HZ {
                        "passband"
                    } else {
                        "transition"
                    }
                    .to_string(),
                    channel,
                    input_phase_radians: phase,
                    analysis_start_frame: start,
                    analysis_frames: count,
                    gain_db: gain,
                    phase_error_radians: fit
                        .as_ref()
                        .and_then(|fit| fit.components[0].phase_radians)
                        .map(|measured| (measured - phase + PI).rem_euclid(TAU) - PI),
                    thdn_db: fit
                        .as_ref()
                        .and_then(|fit| db_ratio(fit.residual_rms, fit.components[0].rms).ok()),
                    residual_rms: fit.as_ref().map(|fit| fit.residual_rms),
                    analyzer_reference_thdn_db: reference_thdn,
                });
            }
        }
    }
    for hz in alias_frequencies(rate) {
        let pair = [
            probe.tone(hz, PHASES[0], None)?,
            probe.tone(hz, PHASES[1], None)?,
        ];
        for render in &pair {
            audit.observe("alias", render, &impulse);
        }
        let (start, count) = probe.window(&pair[0])?;
        for channel in 0..channels {
            let energy = quadrature_rms(&pair, channels, channel, start, count)
                .and_then(|rms| db_ratio(rms, AMPLITUDE));
            grid.aliases.push(AliasPoint {
                input_hz: hz,
                folded_hz: fold_frequency(hz, rate.to_hz),
                channel,
                analysis_start_frame: start,
                analysis_frames: count,
                quadrature_total_output_db: audit.metric("alias quadrature energy", energy),
            });
        }
    }
    if rate.to_hz > rate.from_hz {
        let mut inputs = BTreeSet::from([997, 18_000, 21_000, rate.from_hz / 2 - 1]);
        let boundary_input = rate.from_hz.saturating_sub(rate.to_hz / 2);
        if boundary_input > 0 {
            inputs.insert(boundary_input);
        }
        for hz in inputs
            .into_iter()
            .filter(|&hz| hz > 0 && hz < rate.from_hz / 2)
        {
            let images = image_frequencies(rate, hz);
            if images.is_empty() {
                continue;
            }
            let pair = [
                probe.tone(hz, PHASES[0], None)?,
                probe.tone(hz, PHASES[1], None)?,
            ];
            for render in &pair {
                audit.observe("image", render, &impulse);
            }
            let (start, count) = probe.window(&pair[0])?;
            let references = PHASES.map(|phase| probe.reference(pair[0].actual_frames, hz, phase));
            for image_hz in images {
                let reference_energy = references
                    .iter()
                    .map(|reference| {
                        fit_components(reference, 1, 0, rate.to_hz, &[hz, image_hz], start, count)
                            .map(|fit| fit.components[1].rms.powi(2))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let reference_db =
                    db_ratio(reference_energy.iter().sum::<f64>().sqrt(), AMPLITUDE).ok();
                for channel in 0..channels {
                    let energy = pair
                        .iter()
                        .map(|render| {
                            fit_components(
                                &render.samples,
                                channels,
                                channel,
                                rate.to_hz,
                                &[hz, image_hz],
                                start,
                                count,
                            )
                            .map(|fit| fit.components[1].rms.powi(2))
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .and_then(|values| db_ratio(values.iter().sum::<f64>().sqrt(), AMPLITUDE));
                    grid.images.push(ImagePoint {
                        input_hz: hz,
                        image_hz,
                        channel,
                        analysis_start_frame: start,
                        analysis_frames: count,
                        quadrature_image_db: audit.metric("image joint fit", energy),
                        analyzer_reference_image_db: reference_db,
                    });
                }
            }
        }
    }
    for input_channel in 0..channels {
        let pair = [
            probe.tone(997, PHASES[0], Some(input_channel))?,
            probe.tone(997, PHASES[1], Some(input_channel))?,
        ];
        for render in &pair {
            audit.observe("channel isolation", render, &impulse);
        }
        let (start, count) = probe.window(&pair[0])?;
        for output_channel in 0..channels {
            let level = audit.metric(
                "channel isolation",
                quadrature_rms(&pair, channels, output_channel, start, count)
                    .and_then(|rms| db_ratio(rms, AMPLITUDE)),
            );
            if input_channel == output_channel && level.is_some_and(|db| db < -80.0) {
                audit.errors.push(format!(
                    "active channel {input_channel} is silent or misrouted"
                ));
            }
            grid.channel_isolation.push(ChannelPoint {
                input_channel,
                output_channel,
                input_hz: 997,
                quadrature_output_db: level,
            });
        }
    }
    let dc = probe.render(|_, _| AMPLITUDE)?;
    audit.observe("DC", &dc, &impulse);
    let (start, count) = probe.window(&dc)?;
    for channel in 0..channels {
        let mean = (start..start + count)
            .map(|frame| dc.samples[frame * channels + channel])
            .sum::<f64>()
            / count as f64;
        grid.dc_gain_db_by_channel
            .push(audit.metric("DC gain", db_ratio(mean.abs(), AMPLITUDE)));
    }
    for sign in [1.0, -1.0] {
        let step = probe.render(|frame, _| {
            if frame >= input_frames / 2 {
                sign * AMPLITUDE
            } else {
                0.0
            }
        })?;
        audit.observe("step", &step, &impulse);
        let peaks = (0..channels)
            .map(|channel| {
                step.samples
                    .chunks_exact(channels)
                    .map(|frame| frame[channel].abs())
                    .fold(0.0_f64, f64::max)
            })
            .collect();
        if sign > 0.0 {
            grid.positive_step_full_peak_by_channel = peaks;
        } else {
            grid.negative_step_full_peak_by_channel = peaks;
        }
    }
    grid.renders_checked = audit.renders;
    grid.output_samples_checked = audit.samples;
    let low = grid.response.iter().find(|point| {
        point.input_hz == 997 && point.channel == 0 && point.input_phase_radians == 0.0
    });
    let high = grid.response.iter().find(|point| {
        point.input_hz == 18_000 && point.channel == 0 && point.input_phase_radians == 0.0
    });
    let alias = grid
        .aliases
        .iter()
        .find(|point| point.input_hz == 23_000 && point.channel == 0);
    Ok(QualitySummary {
        classification: MetricClassification::Report,
        valid: audit.errors.is_empty(),
        input_frames,
        expected_complete_output_frames: impulse.expected_frames,
        actual_complete_output_frames: impulse.actual_frames,
        all_output_samples_finite: !audit.non_finite,
        reported_api_buffering_latency_frames: impulse.reported_api_buffering_latency_frames,
        observed_input_frames_before_first_output: impulse
            .observed_input_frames_before_first_output,
        observed_input_frames_through_first_output: impulse
            .observed_input_frames_through_first_output,
        first_output_during_drain: impulse.first_output_during_drain,
        measured_impulse_peak_frame: peak_frame,
        measured_impulse_peak_magnitude: peak,
        gain_997_hz_db: low.and_then(|point| point.gain_db.filter(|_| point.thdn_db.is_some())),
        gain_18_khz_db: high.and_then(|point| point.gain_db),
        passband_max_abs_deviation_db: grid
            .response
            .iter()
            .filter(|point| point.band == "passband")
            .filter_map(|point| point.gain_db.map(f64::abs))
            .max_by(f64::total_cmp),
        thdn_997_hz_db: low.and_then(|point| point.thdn_db),
        stopband_input_hz: alias.map(|point| f64::from(point.input_hz)),
        folded_alias_hz: alias.map(|point| f64::from(point.folded_hz)),
        alias_attenuation_db: alias.and_then(|point| point.quadrature_total_output_db),
        validity_errors: audit.errors,
        grid,
    })
}

fn quadrature_rms(
    pair: &[RenderedSignal; 2],
    channels: usize,
    channel: usize,
    start: usize,
    count: usize,
) -> Result<f64, String> {
    let first = channel_rms(&pair[0].samples, channels, channel, start, count)?;
    let second = channel_rms(&pair[1].samples, channels, channel, start, count)?;
    // Divide by input peak amplitude at the call site: this equals the RMS
    // ratio for a quadrature pair, including exact output DC and Nyquist.
    Ok(first.hypot(second))
}

#[cfg(test)]
#[allow(unused_imports)] // Custom-harness benches also compile with cfg(test).
mod tests {
    use super::*;

    #[test]
    fn every_direction_has_its_applicable_spectral_grid_and_boundary_probes() {
        for rate in super::super::RATE_PAIRS {
            assert!(response_frequencies(rate).len() >= 10);
            if rate.from_hz > rate.to_hz {
                let aliases = alias_frequencies(rate);
                assert!(aliases.contains(&(rate.to_hz / 2)));
                assert!(aliases.contains(&(rate.from_hz / 2)));
                assert!(aliases.contains(&23_000));
                if rate.from_hz / 2 >= rate.to_hz {
                    assert!(aliases.contains(&rate.to_hz));
                }
            } else {
                assert!(!image_frequencies(rate, rate.from_hz / 2 - 1).is_empty());
            }
        }
    }
}
