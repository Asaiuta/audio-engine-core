use std::f64::consts::PI;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use audio_engine_core::config::{PhaseResponse, ResampleQuality};
use audio_engine_core::processor::{
    finish_checked, offline_render_stage_order_csv, post_render_analysis_order_csv,
    process_checked, AtomicCrossfeedParams, AtomicDynamicLoudnessParams,
    AtomicDynamicLoudnessTelemetry, AtomicEqParams, AtomicNoiseShaperParams,
    AtomicPeakLimiterParams, AtomicSaturationParams, AtomicVolumeParams, AudioBlockMut,
    AudioBlockRef, ConvolverControl, Crossfeed, CrossfeedParamsSnapshot, CrossfeedProcessor,
    DynamicLoudness, EqParamsSnapshot, EqProcessor, Equalizer, LimiterMode, LoudnessMeter,
    NoiseShaper, NoiseShaperCurve, NoiseShaperParamsSnapshot, NoiseShaperProcessor,
    OfflineRenderPolicy, OutputChainBuilder, OutputChainParams, PeakLimiter,
    PeakLimiterParamsSnapshot, PeakLimiterProcessor, ProcessBuffers, ProcessState, RenderTimeline,
    RenderedOutput, Saturation, SaturationParamsSnapshot, SaturationProcessor, SaturationQuality,
    SaturationQualityValue, SaturationType, SaturationTypeValue, StreamingProcessor,
    StreamingResampler, VolumeParamsSnapshot, VolumeProcessor, EQ_BANDS,
};
use ebur128::Channel;
use rustfft::{num_complex::Complex, FftPlanner};
use serde::Serialize;

pub mod support;

use support::{
    environment_json, generated_unix_ms, write_json, BenchEnvironment, REPORT_SCHEMA_VERSION,
};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const RESAMPLE_FROM: u32 = 44_100;
const STOPBAND_FROM: u32 = 96_000;
const STOPBAND_TO: u32 = 48_000;
const RESAMPLE_TO: u32 = 48_000;
const AMPLITUDE_DBFS: f64 = -6.0;
const LIMITER_THRESHOLD_DBFS: f64 = -1.0;
const LIMITER_LOOKAHEAD_MS: f64 = 10.0;
const LIMITER_RELEASE_MS: f64 = 100.0;
const NOISE_SHAPER_BITS: u32 = 16;
const NOISE_STIMULUS_FREQUENCY_HZ: f64 = 997.0;
const NOISE_STIMULUS_SINE_DBFS: f64 = -90.0;
const NOISE_STIMULUS_DC_OFFSET_DBFS: f64 = -84.0;
const NOISE_SPECTRUM_FFT_LEN: usize = 65_536;
const LOUDNESS_SINE_DURATION_SECS: f64 = 10.0;
const LOUDNESS_STEPPED_DURATION_SECS: f64 = 12.0;
const DEFAULT_EBU_CORPUS_DIR: &str = "libebur128/test";
const FULL_OUTPUT_TRUE_PEAK_LIMIT_DBTP: f64 = -1.0;
const FULL_OUTPUT_TRUE_PEAK_GATE_TOLERANCE_DB: f64 = 0.001;
const FULL_OUTPUT_CHAIN_BITS: u32 = 24;
const EBU_LOUDNESS_TOLERANCE_LU: f64 = 0.1;
const EBU_LRA_TOLERANCE_LU: f64 = 1.0;
const EBU_TRUE_PEAK_LOWER_TOLERANCE_DB: f64 = -0.4;
const EBU_TRUE_PEAK_UPPER_TOLERANCE_DB: f64 = 0.2;
const SATURATION_ALIAS_STRESS_FREQUENCY_HZ: f64 = 11_000.0;
const SATURATION_ALIAS_AMPLITUDE_DBFS: f64 = -2.0;
const SATURATION_ALIAS_DRIVE: f64 = 1.35;
const SATURATION_ALIAS_MIX: f64 = 1.0;
const SATURATION_CONTINUITY_THRESHOLD: f64 = 0.8;
const SATURATION_CONTINUITY_DRIVE: f64 = 1.3;
const SATURATION_CONTINUITY_OUTPUT_GAIN_DB: f64 = -3.0;
const SATURATION_CONTINUITY_EPSILON: f64 = 1.0e-6;
const LISTENING_DSP_AMPLITUDE_DBFS: f64 = -24.0;
const LISTENING_EQ_TARGET_GAIN_DB: f64 = 6.0;
const LISTENING_CROSSFEED_MIX: f64 = 0.35;
const LISTENING_CROSSFEED_CUTOFF_HZ: f64 = 700.0;
const LISTENING_CROSSFEED_LOW_HZ: f64 = 80.0;
const LISTENING_CROSSFEED_HIGH_HZ: f64 = 2_000.0;
const LISTENING_LOUDNESS_REFERENCE_DB: f64 = -15.0;
const LISTENING_LOUDNESS_LOW_DB: f64 = -40.0;
const LISTENING_LOUDNESS_STRENGTH: f64 = 1.0;
const NOISE_LOW_LEVEL_INPUT_DBFS: f64 = -140.0;
const BAUER_REFERENCE_DC_DIRECT_GAIN: f64 = 0.626_699_081_666_732;
const BAUER_REFERENCE_DC_CROSS_GAIN: f64 = 0.373_300_918_333_268;

// Gate thresholds for the synthetic (always-runs) metrics. These are deliberately
// conservative: observed values sit far inside them so the gates survive across
// CPUs, compiler versions, and debug/release builds. See
// research/benchmark-inventory.md for the per-metric margin rationale.
const GATE_RESAMPLER_THDN_MAX_DB: f64 = -100.0; // observed ~-187 dB
const GATE_PASSBAND_DEVIATION_MAX_DB: f64 = 0.10; // observed ~0.0013 dB
const GATE_ALIAS_ATTENUATION_MAX_DB: f64 = -100.0; // observed ~-295 dB (more negative is better)
const GATE_SATURATION_ALIAS_REDUCTION_MIN_DB: f64 = 6.0;
const GATE_SATURATION_FUNDAMENTAL_DELTA_MIN_DB: f64 = -0.5;
const GATE_SATURATION_THRESHOLD_JUMP_MAX: f64 = 2.0e-6;
const GATE_SATURATION_SLOPE_MISMATCH_MAX: f64 = 1.0e-3;
const GATE_EQ_TARGET_ERROR_MAX_DB: f64 = 0.50;
const GATE_CROSSFEED_LOW_BAND_MIN_DB: f64 = -20.0;
const GATE_CROSSFEED_LOW_VS_HIGH_MIN_DB: f64 = 7.0;
const GATE_CROSSFEED_REFERENCE_ERROR_MAX: f64 = 1.0e-9;
const GATE_CROSSFEED_FIRST_FRAME_DELTA_MAX: f64 = 1.0e-3;
const GATE_CROSSFEED_MIX_CHANGE_PRESERVED_MAX_DELTA: f64 = 1.0e-12;
const GATE_CROSSFEED_MIX_CHANGE_LEGACY_RESET_MIN_DELTA: f64 = 1.0e-4;
const GATE_DYNAMIC_LOUDNESS_BASS_COMPENSATION_MIN_DB: f64 = 6.0;
const GATE_LIMITER_MARGIN_MAX_DB: f64 = 0.05; // sample-peak ceiling; observed ~0.00 dB
const GATE_NOISE_SHAPER_ADVANTAGE_MIN_DB: f64 = 3.0; // observed up to ~+35 dB
const GATE_NOISE_LOW_LEVEL_CHANGED_FRACTION_MIN: f64 = 0.99;
const GATE_NOISE_STRESS_PEAK_MAX: f64 = 1.0;
const GATE_NOISE_STRESS_NON_FINITE_MAX: f64 = 0.0;
const GATE_LOUDNESS_PARITY_MAX_LU: f64 = 1.0e-6; // wrapper forwards to ebur128

// --- Parameter-transition continuity probe ---------------------------------
//
// One uniform discontinuity probe over every field of every in-scope
// `Atomic*Params`. See `measure_parameter_transitions` for the method and
// `PARAMETER_TRANSITION_CASES` for the per-field smoothing declarations.

/// Probe tone amplitude. Low enough that no in-scope processor limits or
/// saturates it on its own, so a measured discontinuity is attributable to the
/// parameter step rather than to the processor's steady-state nonlinearity.
const PARAM_STEP_AMPLITUDE_DBFS: f64 = -12.0;
/// Frames rendered before the step, so every smoother has settled and the
/// filter history is in steady state.
const PARAM_STEP_WARMUP_FRAMES: usize = 8_192;
/// Frames rendered after the step. Must exceed the longest declared smoothing
/// window (the limiter's ~493-frame attack budget at 10 ms/48 kHz) with room
/// for the trajectory to settle.
const PARAM_STEP_SETTLE_FRAMES: usize = 8_192;
/// Block size the probe renders with. The step is published between blocks,
/// which is how a real control thread reaches the callback.
const PARAM_STEP_BLOCK_FRAMES: usize = 512;
/// Minimum settled output change a case must produce for its bound to mean
/// anything. Below this the parameter has no authority over the probe signal
/// (e.g. a 16 kHz EQ band against a 200 Hz tone), so the case is reported as
/// `skipped` rather than passed — an inert probe is not evidence.
const PARAM_STEP_AUTHORITY_FLOOR: f64 = 1.0e-6;
/// Multiplier applied to the derived per-sample bound `authority /
/// smoothing_frames`. Covers the peak slope of a non-linear smoothing curve
/// (a smoothstep peaks at 1.5x its mean slope, an exponential at
/// `1/(1 - exp(-1)) ~ 1.58x`), filter phase movement during the ramp, and
/// block-boundary granularity. It is not a fudge factor for an unsmoothed
/// parameter: at `smoothing_frames = 1` the bound degenerates to the full step
/// and the case is classified `report`, not `gate`.
const PARAM_STEP_BOUND_SAFETY: f64 = 8.0;

const EBU_TRUE_PEAK_FILES: [EbuExpectedFile; 9] = [
    EbuExpectedFile::new("seq-3341-15-24bit.wav.wav", -6.0),
    EbuExpectedFile::new("seq-3341-16-24bit.wav.wav", -6.0),
    EbuExpectedFile::new("seq-3341-17-24bit.wav.wav", -6.0),
    EbuExpectedFile::new("seq-3341-18-24bit.wav.wav", -6.0),
    EbuExpectedFile::new("seq-3341-19-24bit.wav.wav", 3.0),
    EbuExpectedFile::new("seq-3341-20-24bit.wav.wav", 0.0),
    EbuExpectedFile::new("seq-3341-21-24bit.wav.wav", 0.0),
    EbuExpectedFile::new("seq-3341-22-24bit.wav.wav", 0.0),
    EbuExpectedFile::new("seq-3341-23-24bit.wav.wav", 0.0),
];

const EBU_GLOBAL_LOUDNESS_FILES: [EbuExpectedFile; 9] = [
    EbuExpectedFile::new("seq-3341-1-16bit.wav", -22.953556442089987),
    EbuExpectedFile::new("seq-3341-2-16bit.wav", -32.959860397340044),
    EbuExpectedFile::new("seq-3341-3-16bit-v02.wav", -22.995899818255047),
    EbuExpectedFile::new("seq-3341-4-16bit-v02.wav", -23.035918615414182),
    EbuExpectedFile::new("seq-3341-5-16bit-v02.wav", -22.949997446096436),
    EbuExpectedFile::new("seq-3341-6-5channels-16bit.wav", -23.017157781104373),
    EbuExpectedFile::new("seq-3341-6-6channels-WAVEEX-16bit.wav", -23.017157781104373),
    EbuExpectedFile::new("seq-3341-7_seq-3342-5-24bit.wav", -22.980242495081757),
    EbuExpectedFile::new(
        "seq-3341-2011-8_seq-3342-6-24bit-v02.wav",
        -23.009077718930545,
    ),
];

const EBU_LRA_FILES: [EbuExpectedFile; 6] = [
    EbuExpectedFile::new("seq-3342-1-16bit.wav", 10.001105488329134),
    EbuExpectedFile::new("seq-3342-2-16bit.wav", 4.999373405152218),
    EbuExpectedFile::new("seq-3342-3-16bit.wav", 19.995064067783115),
    EbuExpectedFile::new("seq-3342-4-16bit.wav", 14.999273937723455),
    EbuExpectedFile::new("seq-3341-7_seq-3342-5-24bit.wav", 4.974758587847372),
    EbuExpectedFile::new(
        "seq-3341-2011-8_seq-3342-6-24bit-v02.wav",
        14.993650849123316,
    ),
];

const EBU_MAX_MOMENTARY_FILES: [EbuExpectedFile; 20] = [
    EbuExpectedFile::new("seq-3341-13-1-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-2-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-3-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-4-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-5-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-6-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-7-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-8-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-9-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-10-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-11-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-12-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-13-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-14-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-15-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-16-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-17-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-18-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-19-24bit.wav.wav", -23.0),
    EbuExpectedFile::new("seq-3341-13-20-24bit.wav.wav", -23.0),
];

const EBU_MAX_SHORT_TERM_FILES: [EbuExpectedFile; 20] = [
    EbuExpectedFile::new("seq-3341-10-1-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-2-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-3-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-4-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-5-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-6-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-7-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-8-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-9-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-10-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-11-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-12-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-13-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-14-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-15-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-16-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-17-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-18-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-19-24bit.wav", -23.0),
    EbuExpectedFile::new("seq-3341-10-20-24bit.wav", -23.0),
];

fn main() -> Result<(), String> {
    let args = Args::parse(std::env::args().skip(1).collect::<Vec<_>>())?;
    let report = run_measurements(args.quick, &args.ebu_dir)?;

    print_report(&report)?;

    if let Some(out_path) = args.out {
        write_json(&out_path, &report, "quality measurement report")?;
    }

    if args.enforce {
        enforce_limits(&report)?;
    }

    Ok(())
}

#[derive(Debug)]
struct Args {
    quick: bool,
    enforce: bool,
    out: Option<PathBuf>,
    ebu_dir: PathBuf,
}

impl Args {
    fn parse(argv: Vec<String>) -> Result<Self, String> {
        let mut quick = false;
        let mut enforce = false;
        let mut out = None;
        let mut ebu_dir = PathBuf::from(DEFAULT_EBU_CORPUS_DIR);
        let mut index = 0usize;

        while index < argv.len() {
            let arg = &argv[index];
            match arg.as_str() {
                "--quick" => quick = true,
                "--enforce" => enforce = true,
                "--bench" => {}
                "--out" => {
                    let value = argv
                        .get(index + 1)
                        .ok_or_else(|| "--out requires a path".to_string())?;
                    out = Some(PathBuf::from(value));
                    index += 1;
                }
                "--ebu-dir" => {
                    let value = argv
                        .get(index + 1)
                        .ok_or_else(|| "--ebu-dir requires a path".to_string())?;
                    ebu_dir = PathBuf::from(value);
                    index += 1;
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => {
                    if let Some(value) = arg.strip_prefix("--out=") {
                        out = Some(PathBuf::from(value));
                    } else if let Some(value) = arg.strip_prefix("--ebu-dir=") {
                        ebu_dir = PathBuf::from(value);
                    } else {
                        return Err(format!("unknown argument: {arg}"));
                    }
                }
            }
            index += 1;
        }

        Ok(Self {
            quick,
            enforce,
            out,
            ebu_dir,
        })
    }
}

fn print_help() {
    println!(
        "Usage: cargo bench --bench audio_quality_measurements -- [--quick] [--enforce] [--out <json>]\n\
         \n\
         Optional: --ebu-dir <dir> points at an extracted EBU Tech 3341/3342 corpus.\n\
         Default: libebur128/test.\n\
         \n\
         Offline objective audio-quality measurements for the engine's native processing.\n\
         The benchmark does not use CPAL/WASAPI or analog loopback capture."
    );
}

#[derive(Clone, Copy)]
struct EbuExpectedFile {
    file_name: &'static str,
    expected: f64,
}

impl EbuExpectedFile {
    const fn new(file_name: &'static str, expected: f64) -> Self {
        Self {
            file_name,
            expected,
        }
    }
}

#[derive(Serialize)]
struct QualityReport {
    schema_version: u32,
    probe: &'static str,
    generated_unix_ms: u128,
    mode: &'static str,
    environment: BenchEnvironment,
    conditions: Conditions,
    thdn: ThdnSection,
    frequency_response: FrequencyResponseSection,
    limiter: LimiterSection,
    resampler_stopband: StopbandSection,
    saturation_continuity: SaturationContinuitySection,
    saturation_aliasing: SaturationAliasingSection,
    listening_dsp: ListeningDspSection,
    noise_shaping: NoiseShapingSection,
    loudness_reference: LoudnessReferenceSection,
    full_output_true_peak: FullOutputTruePeakSection,
    parameter_transitions: ParameterTransitionSection,
    // Machine-readable gate/report classification for every metric a reader might
    // cite. `gate` metrics fail the run under `--enforce`; `report` metrics are
    // evidence only. This makes README numbers traceable to a named, classified
    // gate and gives `--enforce` a single uniform measured-vs-threshold table.
    metrics: Vec<MetricResult>,
}

/// How a metric value is compared against its threshold to decide pass/fail.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Comparison {
    /// Passes when `measured <= threshold` (e.g. THD+N, deviation, overshoot).
    AtMost,
    /// Passes when `measured >= threshold` (e.g. an attenuation/advantage floor).
    AtLeast,
    /// Passes when `lower <= measured <= upper` (e.g. EBU true-peak tolerance band).
    Within,
}

/// Whether a metric is enforced (`gate`) or evidence-only (`report`).
#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Classification {
    Gate,
    Report,
    /// A gate whose reference inputs are absent on this machine (e.g. the EBU
    /// corpus). Reported as skipped, never a silent pass.
    Skipped,
}

/// One classified metric: its name, value, threshold, and pass/fail state.
#[derive(Clone, Serialize)]
struct MetricResult {
    name: &'static str,
    classification: Classification,
    comparison: Comparison,
    measured: f64,
    threshold: f64,
    threshold_upper: Option<f64>,
    unit: &'static str,
    passed: bool,
    detail: Option<String>,
}

impl MetricResult {
    fn evaluate(passed: bool) -> bool {
        passed
    }

    fn gate(
        name: &'static str,
        comparison: Comparison,
        measured: f64,
        threshold: f64,
        unit: &'static str,
    ) -> Self {
        let passed = compare(comparison, measured, threshold, None);
        Self {
            name,
            classification: Classification::Gate,
            comparison,
            measured,
            threshold,
            threshold_upper: None,
            unit,
            passed: Self::evaluate(passed),
            detail: None,
        }
    }

    fn gate_within(
        name: &'static str,
        measured: f64,
        lower: f64,
        upper: f64,
        unit: &'static str,
        passed: bool,
        detail: Option<String>,
    ) -> Self {
        Self {
            name,
            classification: Classification::Gate,
            comparison: Comparison::Within,
            measured,
            threshold: lower,
            threshold_upper: Some(upper),
            unit,
            passed: Self::evaluate(passed),
            detail,
        }
    }

    fn report(
        name: &'static str,
        comparison: Comparison,
        measured: f64,
        threshold: f64,
        unit: &'static str,
    ) -> Self {
        Self {
            name,
            classification: Classification::Report,
            comparison,
            measured,
            threshold,
            threshold_upper: None,
            unit,
            passed: true,
            detail: None,
        }
    }

    fn skipped(name: &'static str, unit: &'static str, detail: String) -> Self {
        Self {
            name,
            classification: Classification::Skipped,
            comparison: Comparison::AtMost,
            measured: f64::NAN,
            threshold: f64::NAN,
            threshold_upper: None,
            unit,
            passed: true,
            detail: Some(detail),
        }
    }

    /// A human-readable measured-vs-threshold string for gate diagnostics.
    fn measured_vs_threshold(&self) -> String {
        match self.comparison {
            Comparison::AtMost => format!(
                "measured {:.6} {} > threshold {:.6} {}",
                self.measured, self.unit, self.threshold, self.unit
            ),
            Comparison::AtLeast => format!(
                "measured {:.6} {} < threshold {:.6} {}",
                self.measured, self.unit, self.threshold, self.unit
            ),
            Comparison::Within => format!(
                "measured {:.6} {} outside [{:.6}, {:.6}] {}",
                self.measured,
                self.unit,
                self.threshold,
                self.threshold_upper.unwrap_or(f64::NAN),
                self.unit
            ),
        }
    }
}

fn compare(comparison: Comparison, measured: f64, threshold: f64, upper: Option<f64>) -> bool {
    match comparison {
        Comparison::AtMost => measured <= threshold,
        Comparison::AtLeast => measured >= threshold,
        Comparison::Within => measured >= threshold && measured <= upper.unwrap_or(f64::INFINITY),
    }
}

#[derive(Serialize)]
struct Conditions {
    measurement_path: &'static str,
    resampler_phase: &'static str,
    resampler_quality: &'static str,
    thdn_method: &'static str,
    frequency_response_method: &'static str,
    stopband_method: &'static str,
    saturation_continuity_method: &'static str,
    saturation_aliasing_method: &'static str,
    listening_dsp_method: &'static str,
    limiter_method: &'static str,
    noise_shaping_method: &'static str,
    loudness_reference_method: &'static str,
    parameter_transition_method: &'static str,
    full_output_true_peak_method: String,
    render_timeline: &'static str,
    unknown_tail_energy_threshold_dbfs: f64,
    unknown_tail_silence_hold_ms: u32,
    unknown_tail_max_tail_ms: u32,
}

#[derive(Serialize)]
struct ThdnSection {
    analyzer_floor_db: f64,
    resampler_44k1_to_48k_db: f64,
    limiter_below_threshold_db: f64,
    test_frequency_hz: f64,
    amplitude_dbfs: f64,
}

#[derive(Serialize)]
struct FrequencyResponseSection {
    from_rate_hz: u32,
    to_rate_hz: u32,
    points: Vec<FrequencyPoint>,
    passband_max_abs_deviation_db_20hz_to_18khz: f64,
}

#[derive(Serialize)]
struct FrequencyPoint {
    frequency_hz: f64,
    gain_db: f64,
    output_amplitude_dbfs: f64,
}

#[derive(Serialize)]
struct LimiterSection {
    threshold_dbfs: f64,
    input_peak_dbfs: f64,
    output_peak_dbfs: f64,
    output_margin_to_threshold_db: f64,
    final_gain_reduction_db: f64,
    transparent_sine_thdn_db: f64,
    // Intersample (true-peak) stress: a signal whose sample peak sits below the
    // ceiling but whose reconstructed true peak exceeds it. Compares the default
    // true-peak mode against legacy sample-peak mode to show the guarantee.
    intersample_stress_input_sample_peak_dbfs: f64,
    intersample_stress_input_true_peak_dbtp: f64,
    intersample_stress_true_peak_mode_output_dbtp: f64,
    intersample_stress_sample_peak_mode_output_dbtp: f64,
}

#[derive(Serialize)]
struct StopbandSection {
    from_rate_hz: u32,
    to_rate_hz: u32,
    points: Vec<StopbandPoint>,
    worst_alias_attenuation_db: f64,
    worst_residual_attenuation_db: f64,
}

#[derive(Serialize)]
struct StopbandPoint {
    input_frequency_hz: f64,
    folded_frequency_hz: f64,
    alias_attenuation_db: f64,
    residual_rms_attenuation_db: f64,
    output_alias_amplitude_dbfs: f64,
}

#[derive(Serialize)]
struct SaturationAliasingSection {
    sample_rate_hz: u32,
    saturation_type: &'static str,
    upgraded_quality: &'static str,
    stress_frequency_hz: f64,
    input_amplitude_dbfs: f64,
    drive: f64,
    mix: f64,
    direct_fundamental_dbfs: f64,
    upgraded_fundamental_dbfs: f64,
    fundamental_delta_db: f64,
    direct_alias_energy_dbfs: f64,
    upgraded_alias_energy_dbfs: f64,
    alias_reduction_db: f64,
    points: Vec<SaturationAliasPoint>,
}

#[derive(Serialize)]
struct SaturationContinuitySection {
    threshold: f64,
    drive: f64,
    output_gain_db: f64,
    epsilon: f64,
    points: Vec<SaturationContinuityPoint>,
    max_threshold_jump_linear: f64,
    max_first_derivative_mismatch: f64,
}

#[derive(Serialize)]
struct SaturationContinuityPoint {
    saturation_type: &'static str,
    sign: f64,
    threshold_jump_linear: f64,
    inside_first_derivative: f64,
    outside_first_derivative: f64,
    first_derivative_mismatch: f64,
}

#[derive(Serialize)]
struct SaturationAliasPoint {
    harmonic: u32,
    folded_frequency_hz: f64,
    direct_alias_dbfs: f64,
    upgraded_alias_dbfs: f64,
    reduction_db: f64,
}

#[derive(Serialize)]
struct ListeningDspSection {
    sample_rate_hz: u32,
    eq: ListeningEqSection,
    crossfeed: ListeningCrossfeedSection,
    dynamic_loudness: ListeningDynamicLoudnessSection,
}

#[derive(Serialize)]
struct ListeningEqSection {
    target_gain_db: f64,
    points: Vec<ListeningEqPoint>,
    max_abs_target_error_db: f64,
}

#[derive(Serialize)]
struct ListeningEqPoint {
    frequency_hz: f64,
    measured_gain_db: f64,
    target_gain_db: f64,
    target_error_db: f64,
}

#[derive(Serialize)]
struct ListeningCrossfeedSection {
    mix: f64,
    cutoff_hz: f64,
    low_frequency_hz: f64,
    high_frequency_hz: f64,
    low_crossfeed_db: f64,
    high_crossfeed_db: f64,
    low_vs_high_crossfeed_db: f64,
    reference_dc_direct_gain: f64,
    reference_dc_cross_gain: f64,
    reference_max_abs_error: f64,
    mix_change_first_frame_delta: f64,
    mix_change_preserved_max_delta: f64,
    mix_change_legacy_reset_max_delta: f64,
}

#[derive(Serialize)]
struct ListeningDynamicLoudnessSection {
    reference_volume_db: f64,
    low_volume_db: f64,
    strength: f64,
    bass_probe_hz: f64,
    presence_probe_hz: f64,
    bass_compensation_db: f64,
    presence_compensation_db: f64,
    reported_loudness_factor: f64,
}

#[derive(Serialize)]
struct NoiseShapingSection {
    sample_rate_hz: u32,
    channels: usize,
    bits: u32,
    stimulus_frequency_hz: f64,
    stimulus_sine_dbfs: f64,
    stimulus_dc_offset_dbfs: f64,
    fft_len: usize,
    points: Vec<NoiseShapingPoint>,
    strongest_shaped_high_minus_ear_band_advantage_db: f64,
    low_level_input_dbfs: f64,
    low_level_changed_fraction: f64,
    silence_non_zero_fraction: f64,
    stress_max_abs_output: f64,
    stress_non_finite_outputs: usize,
}

#[derive(Serialize)]
struct NoiseShapingPoint {
    curve: &'static str,
    total_noise_rms_dbfs: f64,
    ear_band_2k_to_6k_rms_dbfs: f64,
    mid_band_6k_to_10k_rms_dbfs: f64,
    high_band_14k_to_18k_rms_dbfs: f64,
    high_minus_ear_band_db: f64,
}

#[derive(Serialize)]
struct LoudnessReferenceSection {
    sample_rate_hz: u32,
    channels: usize,
    fixtures: Vec<LoudnessFixtureResult>,
    ebu_corpus: EbuLoudnessCorpusSection,
    max_integrated_delta_lu: f64,
    max_momentary_delta_lu: f64,
    max_short_term_delta_lu: f64,
    max_loudness_range_delta_lu: f64,
    max_true_peak_delta_db: f64,
}

#[derive(Serialize)]
struct LoudnessFixtureResult {
    name: &'static str,
    duration_secs: f64,
    engine_integrated_lufs: f64,
    reference_integrated_lufs: f64,
    integrated_delta_lu: f64,
    engine_momentary_lufs: f64,
    reference_momentary_lufs: f64,
    momentary_delta_lu: f64,
    engine_short_term_lufs: f64,
    reference_short_term_lufs: f64,
    short_term_delta_lu: f64,
    engine_loudness_range_lu: f64,
    reference_loudness_range_lu: f64,
    loudness_range_delta_lu: f64,
    engine_true_peak_dbtp: f64,
    reference_true_peak_dbtp: f64,
    true_peak_delta_db: f64,
}

#[derive(Serialize)]
struct EbuLoudnessCorpusSection {
    available: bool,
    source_dir: String,
    source_note: &'static str,
    missing_files: Vec<&'static str>,
    global_loudness_points: Vec<EbuCorpusPoint>,
    loudness_range_points: Vec<EbuCorpusPoint>,
    max_momentary_points: Vec<EbuCorpusPoint>,
    max_short_term_points: Vec<EbuCorpusPoint>,
    max_abs_global_error_lu: f64,
    max_abs_loudness_range_error_lu: f64,
    max_abs_max_momentary_error_lu: f64,
    max_abs_max_short_term_error_lu: f64,
}

#[derive(Serialize)]
struct EbuCorpusPoint {
    file_name: &'static str,
    sample_rate_hz: u32,
    channels: usize,
    frames: usize,
    expected: f64,
    measured: f64,
    error: f64,
    passed: bool,
}

#[derive(Serialize)]
struct FullOutputTruePeakSection {
    output_sample_rate_hz: u32,
    chain: String,
    post_render_analysis: String,
    limiter_threshold_dbfs: f64,
    final_noise_shaper_bits: u32,
    points: Vec<FullOutputTruePeakPoint>,
    ebu_true_peak_corpus: EbuTruePeakCorpusSection,
    worst_output_true_peak_dbtp: f64,
    worst_margin_to_limiter_threshold_db: f64,
}

#[derive(Serialize)]
struct FullOutputTruePeakPoint {
    name: String,
    source_kind: &'static str,
    source_sample_rate_hz: u32,
    source_channels: usize,
    source_frames: usize,
    input_sample_peak_dbfs: f64,
    input_true_peak_dbtp: f64,
    output_sample_peak_dbfs: f64,
    output_true_peak_dbtp: f64,
    output_margin_to_limiter_threshold_db: f64,
    final_limiter_gain_reduction_db: f64,
    output_frames: usize,
    rendered_frames: usize,
    algorithmic_latency_frames: usize,
    semantic_tail_frames: usize,
    tail_truncated: bool,
}

#[derive(Serialize)]
struct EbuTruePeakCorpusSection {
    available: bool,
    source_dir: String,
    missing_files: Vec<&'static str>,
    points: Vec<EbuTruePeakPoint>,
    max_abs_expected_error_db: f64,
}

#[derive(Serialize)]
struct EbuTruePeakPoint {
    file_name: &'static str,
    sample_rate_hz: u32,
    channels: usize,
    frames: usize,
    expected_dbtp: f64,
    measured_input_true_peak_dbtp: f64,
    input_error_db: f64,
    full_output_true_peak_dbtp: f64,
    full_output_margin_to_limiter_threshold_db: f64,
    output_frames: usize,
    rendered_frames: usize,
    algorithmic_latency_frames: usize,
    semantic_tail_frames: usize,
    tail_truncated: bool,
    passed_reference_tolerance: bool,
}

struct SineFit {
    amplitude: f64,
    thdn_db: f64,
}

struct NoiseSpectrumBands {
    ear_band_2k_to_6k_rms_dbfs: f64,
    mid_band_6k_to_10k_rms_dbfs: f64,
    high_band_14k_to_18k_rms_dbfs: f64,
}

struct LoudnessValues {
    integrated_lufs: f64,
    momentary_lufs: f64,
    short_term_lufs: f64,
    loudness_range_lu: f64,
    true_peak_dbtp: f64,
}

struct WavData {
    sample_rate: u32,
    channels: usize,
    samples: Vec<f64>,
}

#[derive(Clone, Copy)]
struct WavFormat {
    audio_format: u16,
    sample_rate: u32,
    channels: usize,
    bits_per_sample: usize,
    block_align: usize,
}

fn render_timeline_name(timeline: RenderTimeline) -> &'static str {
    match timeline {
        RenderTimeline::Compensated => "compensated",
        RenderTimeline::RawCausal => "raw_causal",
    }
}

fn run_measurements(quick: bool, ebu_dir: &Path) -> Result<QualityReport, String> {
    let frames = if quick { 65_536 } else { 262_144 };
    let test_frequency = 997.0;
    let amplitude = db_to_linear(AMPLITUDE_DBFS);
    let input_44k1 = sine_mono(frames, RESAMPLE_FROM, test_frequency, amplitude);
    let analyzer_fit = fit_sine(&input_44k1, RESAMPLE_FROM, test_frequency, 1024, frames / 2)?;

    let resampled = resample_mono(&input_44k1, RESAMPLE_FROM, RESAMPLE_TO)?;
    let resampler_fit = fit_sine(
        &resampled,
        RESAMPLE_TO,
        test_frequency,
        output_skip_frames(RESAMPLE_TO),
        resampled
            .len()
            .saturating_sub(output_skip_frames(RESAMPLE_TO) * 2),
    )?;

    let limiter_transparent = measure_limiter_transparent_thdn(frames, test_frequency, amplitude)?;
    let frequency_response = measure_frequency_response(frames, amplitude)?;
    let limiter = measure_limiter(frames, test_frequency, amplitude, limiter_transparent)?;
    let resampler_stopband = measure_stopband(frames)?;
    let saturation_continuity = measure_saturation_continuity();
    let saturation_aliasing = measure_saturation_aliasing(frames)?;
    let listening_dsp = measure_listening_dsp(frames)?;
    let noise_shaping = measure_noise_shaping(frames)?;
    let loudness_reference = measure_loudness_reference(ebu_dir)?;
    let render_policy = OfflineRenderPolicy::default();
    let full_output_true_peak = measure_full_output_true_peak(frames, ebu_dir, render_policy)?;
    let parameter_transitions = measure_parameter_transitions()?;

    let metrics = build_metrics(
        &resampler_fit,
        limiter_transparent,
        &frequency_response,
        &limiter,
        &resampler_stopband,
        &saturation_continuity,
        &saturation_aliasing,
        &listening_dsp,
        &noise_shaping,
        &loudness_reference,
        &full_output_true_peak,
        &parameter_transitions,
    );

    Ok(QualityReport {
        schema_version: REPORT_SCHEMA_VERSION,
        probe: "audio_quality_measurements",
        generated_unix_ms: generated_unix_ms(),
        mode: if quick { "quick" } else { "full" },
        environment: BenchEnvironment::capture(),
        conditions: Conditions {
            measurement_path: "offline f64 synthetic signal -> Rust processor modules -> numeric analysis",
            resampler_phase: "Linear",
            resampler_quality: "UltraHigh",
            thdn_method: "least-squares sine fit with DC term, THD+N = residual_rms / fitted_sine_rms",
            frequency_response_method: "single-tone amplitude fit after 44.1 kHz -> 48 kHz resampling",
            stopband_method: "96 kHz -> 48 kHz resampling of above-output-Nyquist tones; alias fit plus broad residual RMS",
            saturation_continuity_method: "one-sided finite-difference transfer probe across the soft-knee threshold for Tape/Tube/Transistor with non-unity output gain",
            saturation_aliasing_method: "11 kHz driven tube waveshaper; fit folded above-Nyquist harmonics and compare source-rate Direct vs Oversampled4x alias energy",
            listening_dsp_method: "single-tone fits through IIR EQ, libbs2b-style low-pass/high-boost Bauer crossfeed, and dynamic-loudness processors after settling; crossfeed also checks the independent 4.5 dB reference DC profile and parameter-ramp continuity",
            limiter_method: "PeakLimiter (default 4x-oversampled true-peak detection) in-place processing; reports sample-peak ceiling, below-threshold THD+N, and intersample-peak stress vs legacy sample-peak mode",
            noise_shaping_method: "16-bit NoiseShaper error signal FFT with Hann window; equal-width 2-6/6-10/14-18 kHz RMS bands plus -140 dBFS, silence, overload, and non-finite boundary probes",
            loudness_reference_method: "LoudnessMeter wrapper compared with direct ebur128 over deterministic f64 fixtures; optional EBU Tech 3341/3342 corpus expected-value checks",
            parameter_transition_method: "per-field step probe: three passes (hold pre-step / step at a block boundary / hold post-step) through one adapter; authority = max|C-A| over the settled tail, excess step = max(0, |dB| - max(|dA|,|dC|)) so the signal's own slew is not counted, bound = authority / documented_ramp_frames * safety",
            full_output_true_peak_method: format!(
                "offline render stages: {}; post-render analysis: {}",
                offline_render_stage_order_csv(),
                post_render_analysis_order_csv(),
            ),
            render_timeline: render_timeline_name(render_policy.timeline),
            unknown_tail_energy_threshold_dbfs: render_policy
                .unknown_tail
                .energy_threshold_dbfs,
            unknown_tail_silence_hold_ms: render_policy.unknown_tail.silence_hold_ms,
            unknown_tail_max_tail_ms: render_policy.unknown_tail.max_tail_ms,
        },
        thdn: ThdnSection {
            analyzer_floor_db: analyzer_fit.thdn_db,
            resampler_44k1_to_48k_db: resampler_fit.thdn_db,
            limiter_below_threshold_db: limiter_transparent,
            test_frequency_hz: test_frequency,
            amplitude_dbfs: AMPLITUDE_DBFS,
        },
        frequency_response,
        limiter,
        resampler_stopband,
        saturation_continuity,
        saturation_aliasing,
        listening_dsp,
        noise_shaping,
        loudness_reference,
        full_output_true_peak,
        parameter_transitions,
        metrics,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_metrics(
    resampler_fit: &SineFit,
    limiter_transparent_thdn_db: f64,
    frequency_response: &FrequencyResponseSection,
    limiter: &LimiterSection,
    resampler_stopband: &StopbandSection,
    saturation_continuity: &SaturationContinuitySection,
    saturation_aliasing: &SaturationAliasingSection,
    listening_dsp: &ListeningDspSection,
    noise_shaping: &NoiseShapingSection,
    loudness_reference: &LoudnessReferenceSection,
    full_output_true_peak: &FullOutputTruePeakSection,
    parameter_transitions: &ParameterTransitionSection,
) -> Vec<MetricResult> {
    let mut metrics = vec![
        // --- Deterministic / high-headroom synthetic gates (always run) ---
        MetricResult::gate(
            "resampler_thdn_44k1_to_48k",
            Comparison::AtMost,
            resampler_fit.thdn_db,
            GATE_RESAMPLER_THDN_MAX_DB,
            "dB",
        ),
        MetricResult::gate(
            "resampler_passband_max_deviation_20hz_to_18khz",
            Comparison::AtMost,
            frequency_response.passband_max_abs_deviation_db_20hz_to_18khz,
            GATE_PASSBAND_DEVIATION_MAX_DB,
            "dB",
        ),
        MetricResult::gate(
            "resampler_worst_alias_attenuation_96k_to_48k",
            Comparison::AtMost,
            resampler_stopband.worst_alias_attenuation_db,
            GATE_ALIAS_ATTENUATION_MAX_DB,
            "dB",
        ),
        MetricResult::gate(
            "saturation_threshold_transfer_jump",
            Comparison::AtMost,
            saturation_continuity.max_threshold_jump_linear,
            GATE_SATURATION_THRESHOLD_JUMP_MAX,
            "linear",
        ),
        MetricResult::gate(
            "saturation_threshold_first_derivative_mismatch",
            Comparison::AtMost,
            saturation_continuity.max_first_derivative_mismatch,
            GATE_SATURATION_SLOPE_MISMATCH_MAX,
            "linear/linear",
        ),
        MetricResult::gate(
            "saturation_oversampled4x_alias_reduction",
            Comparison::AtLeast,
            saturation_aliasing.alias_reduction_db,
            GATE_SATURATION_ALIAS_REDUCTION_MIN_DB,
            "dB",
        ),
        MetricResult::gate(
            "saturation_oversampled4x_fundamental_delta",
            Comparison::AtLeast,
            saturation_aliasing.fundamental_delta_db,
            GATE_SATURATION_FUNDAMENTAL_DELTA_MIN_DB,
            "dB",
        ),
        MetricResult::gate(
            "listening_eq_target_gain_accuracy",
            Comparison::AtMost,
            listening_dsp.eq.max_abs_target_error_db,
            GATE_EQ_TARGET_ERROR_MAX_DB,
            "dB",
        ),
        MetricResult::gate(
            "listening_crossfeed_low_band_level",
            Comparison::AtLeast,
            listening_dsp.crossfeed.low_crossfeed_db,
            GATE_CROSSFEED_LOW_BAND_MIN_DB,
            "dB",
        ),
        MetricResult::gate(
            "listening_crossfeed_low_vs_high_separation",
            Comparison::AtLeast,
            listening_dsp.crossfeed.low_vs_high_crossfeed_db,
            GATE_CROSSFEED_LOW_VS_HIGH_MIN_DB,
            "dB",
        ),
        MetricResult::gate(
            "listening_crossfeed_bauer_reference_dc_gain_error",
            Comparison::AtMost,
            listening_dsp.crossfeed.reference_max_abs_error,
            GATE_CROSSFEED_REFERENCE_ERROR_MAX,
            "linear",
        ),
        MetricResult::gate(
            "listening_crossfeed_mix_change_first_frame_delta",
            Comparison::AtMost,
            listening_dsp.crossfeed.mix_change_first_frame_delta,
            GATE_CROSSFEED_FIRST_FRAME_DELTA_MAX,
            "linear",
        ),
        MetricResult::gate(
            "listening_crossfeed_mix_change_preserves_history",
            Comparison::AtMost,
            listening_dsp.crossfeed.mix_change_preserved_max_delta,
            GATE_CROSSFEED_MIX_CHANGE_PRESERVED_MAX_DELTA,
            "linear",
        ),
        MetricResult::gate(
            "listening_crossfeed_legacy_reset_delta",
            Comparison::AtLeast,
            listening_dsp.crossfeed.mix_change_legacy_reset_max_delta,
            GATE_CROSSFEED_MIX_CHANGE_LEGACY_RESET_MIN_DELTA,
            "linear",
        ),
        MetricResult::gate(
            "listening_dynamic_loudness_bass_compensation",
            Comparison::AtLeast,
            listening_dsp.dynamic_loudness.bass_compensation_db,
            GATE_DYNAMIC_LOUDNESS_BASS_COMPENSATION_MIN_DB,
            "dB",
        ),
        MetricResult::gate(
            "limiter_output_margin_to_threshold",
            Comparison::AtMost,
            limiter.output_margin_to_threshold_db,
            GATE_LIMITER_MARGIN_MAX_DB,
            "dB",
        ),
        MetricResult::gate(
            "noise_shaper_strongest_ear_band_advantage",
            Comparison::AtLeast,
            noise_shaping.strongest_shaped_high_minus_ear_band_advantage_db,
            GATE_NOISE_SHAPER_ADVANTAGE_MIN_DB,
            "dB",
        ),
        MetricResult::gate(
            "noise_shaper_low_level_changed_fraction",
            Comparison::AtLeast,
            noise_shaping.low_level_changed_fraction,
            GATE_NOISE_LOW_LEVEL_CHANGED_FRACTION_MIN,
            "ratio",
        ),
        MetricResult::gate(
            "noise_shaper_stress_peak",
            Comparison::AtMost,
            noise_shaping.stress_max_abs_output,
            GATE_NOISE_STRESS_PEAK_MAX,
            "linear",
        ),
        MetricResult::gate(
            "noise_shaper_stress_non_finite_outputs",
            Comparison::AtMost,
            noise_shaping.stress_non_finite_outputs as f64,
            GATE_NOISE_STRESS_NON_FINITE_MAX,
            "samples",
        ),
        MetricResult::gate(
            "loudness_integrated_parity_vs_ebur128",
            Comparison::AtMost,
            loudness_reference.max_integrated_delta_lu,
            GATE_LOUDNESS_PARITY_MAX_LU,
            "LU",
        ),
        MetricResult::gate(
            "loudness_momentary_parity_vs_ebur128",
            Comparison::AtMost,
            loudness_reference.max_momentary_delta_lu,
            GATE_LOUDNESS_PARITY_MAX_LU,
            "LU",
        ),
        MetricResult::gate(
            "loudness_short_term_parity_vs_ebur128",
            Comparison::AtMost,
            loudness_reference.max_short_term_delta_lu,
            GATE_LOUDNESS_PARITY_MAX_LU,
            "LU",
        ),
        MetricResult::gate(
            "loudness_range_parity_vs_ebur128",
            Comparison::AtMost,
            loudness_reference.max_loudness_range_delta_lu,
            GATE_LOUDNESS_PARITY_MAX_LU,
            "LU",
        ),
        MetricResult::gate(
            "full_output_chain_worst_true_peak",
            Comparison::AtMost,
            full_output_true_peak.worst_output_true_peak_dbtp,
            FULL_OUTPUT_TRUE_PEAK_LIMIT_DBTP + FULL_OUTPUT_TRUE_PEAK_GATE_TOLERANCE_DB,
            "dBTP",
        ),
        // --- Report-only synthetic evidence (printed/serialized, never fails) ---
        MetricResult::report(
            "limiter_below_threshold_thdn",
            Comparison::AtMost,
            limiter_transparent_thdn_db,
            -120.0,
            "dB",
        ),
        MetricResult::report(
            "limiter_true_peak_mode_intersample_stress_output",
            Comparison::AtMost,
            limiter.intersample_stress_true_peak_mode_output_dbtp,
            LIMITER_THRESHOLD_DBFS + 0.1,
            "dBTP",
        ),
        MetricResult::report(
            "loudness_true_peak_parity_vs_ebur128",
            Comparison::AtMost,
            loudness_reference.max_true_peak_delta_db,
            0.1,
            "dB",
        ),
    ];

    // --- EBU corpus gates: enforced only when reference vectors are present ---
    build_ebu_loudness_metrics(&loudness_reference.ebu_corpus, &mut metrics);
    build_ebu_true_peak_metrics(&full_output_true_peak.ebu_true_peak_corpus, &mut metrics);

    // --- Per-parameter transition continuity: one row per Atomic*Params field ---
    metrics.extend(parameter_transition_metrics(parameter_transitions));

    metrics
}

fn build_ebu_loudness_metrics(corpus: &EbuLoudnessCorpusSection, metrics: &mut Vec<MetricResult>) {
    if !corpus.available {
        metrics.push(MetricResult::skipped(
            "ebu_loudness_corpus",
            "LU",
            format!(
                "{} reference file(s) missing under {}",
                corpus.missing_files.len(),
                corpus.source_dir
            ),
        ));
        return;
    }

    match first_failed_ebu_loudness_point(corpus) {
        Some(point) => {
            let tolerance = ebu_loudness_tolerance(corpus, point);
            metrics.push(MetricResult::gate_within(
                "ebu_loudness_corpus",
                point.error,
                -tolerance,
                tolerance,
                "LU",
                false,
                Some(format!(
                    "file {} expected {:.6} measured {:.6}",
                    point.file_name, point.expected, point.measured
                )),
            ));
        }
        None => {
            let worst = corpus
                .max_abs_global_error_lu
                .max(corpus.max_abs_loudness_range_error_lu)
                .max(corpus.max_abs_max_momentary_error_lu)
                .max(corpus.max_abs_max_short_term_error_lu);
            metrics.push(MetricResult::gate_within(
                "ebu_loudness_corpus",
                worst,
                0.0,
                EBU_LRA_TOLERANCE_LU,
                "LU",
                true,
                Some(format!(
                    "{} point(s) all within EBU tolerance",
                    ebu_loudness_point_count(corpus)
                )),
            ));
        }
    }
}

fn ebu_loudness_tolerance(corpus: &EbuLoudnessCorpusSection, point: &EbuCorpusPoint) -> f64 {
    if corpus
        .loudness_range_points
        .iter()
        .any(|candidate| std::ptr::eq(candidate, point))
    {
        EBU_LRA_TOLERANCE_LU
    } else {
        EBU_LOUDNESS_TOLERANCE_LU
    }
}

fn build_ebu_true_peak_metrics(corpus: &EbuTruePeakCorpusSection, metrics: &mut Vec<MetricResult>) {
    if !corpus.available {
        metrics.push(MetricResult::skipped(
            "ebu_true_peak_corpus",
            "dB",
            format!(
                "{} reference file(s) missing under {}",
                corpus.missing_files.len(),
                corpus.source_dir
            ),
        ));
        return;
    }

    match first_failed_ebu_true_peak_point(corpus) {
        Some(point) => metrics.push(MetricResult::gate_within(
            "ebu_true_peak_corpus",
            point.input_error_db,
            EBU_TRUE_PEAK_LOWER_TOLERANCE_DB,
            EBU_TRUE_PEAK_UPPER_TOLERANCE_DB,
            "dB",
            false,
            Some(format!(
                "file {} expected {:.3} dBTP measured {:.3} dBTP",
                point.file_name, point.expected_dbtp, point.measured_input_true_peak_dbtp
            )),
        )),
        None => metrics.push(MetricResult::gate_within(
            "ebu_true_peak_corpus",
            corpus.max_abs_expected_error_db,
            EBU_TRUE_PEAK_LOWER_TOLERANCE_DB,
            EBU_TRUE_PEAK_UPPER_TOLERANCE_DB,
            "dB",
            true,
            Some(format!(
                "{} point(s) within EBU tolerance",
                corpus.points.len()
            )),
        )),
    }
}

fn measure_frequency_response(
    frames: usize,
    amplitude: f64,
) -> Result<FrequencyResponseSection, String> {
    let frequencies = [
        20.0, 100.0, 1_000.0, 5_000.0, 10_000.0, 16_000.0, 18_000.0, 20_000.0,
    ];
    let mut points = Vec::with_capacity(frequencies.len());

    for frequency in frequencies {
        let input = sine_mono(frames, RESAMPLE_FROM, frequency, amplitude);
        let output = resample_mono(&input, RESAMPLE_FROM, RESAMPLE_TO)?;
        let fit = fit_sine(
            &output,
            RESAMPLE_TO,
            frequency,
            output_skip_frames(RESAMPLE_TO),
            output
                .len()
                .saturating_sub(output_skip_frames(RESAMPLE_TO) * 2),
        )?;
        points.push(FrequencyPoint {
            frequency_hz: frequency,
            gain_db: db_ratio(fit.amplitude, amplitude),
            output_amplitude_dbfs: dbfs(fit.amplitude),
        });
    }

    let passband_max_abs_deviation_db_20hz_to_18khz = points
        .iter()
        .filter(|point| point.frequency_hz <= 18_000.0)
        .map(|point| point.gain_db.abs())
        .fold(0.0, f64::max);

    Ok(FrequencyResponseSection {
        from_rate_hz: RESAMPLE_FROM,
        to_rate_hz: RESAMPLE_TO,
        points,
        passband_max_abs_deviation_db_20hz_to_18khz,
    })
}

fn measure_limiter_transparent_thdn(
    frames: usize,
    frequency: f64,
    amplitude: f64,
) -> Result<f64, String> {
    let mut samples = stereo_from_mono(&sine_mono(frames, SAMPLE_RATE, frequency, amplitude));
    let mut limiter = PeakLimiter::new(
        CHANNELS,
        SAMPLE_RATE,
        LIMITER_THRESHOLD_DBFS,
        LIMITER_LOOKAHEAD_MS,
        LIMITER_RELEASE_MS,
    )
    .map_err(|error| error.to_string())?;
    limiter
        .process(&mut samples, CHANNELS)
        .map_err(|error| error.to_string())?;
    let mono = extract_channel(&samples, CHANNELS, 0);
    let fit = fit_sine(
        &mono,
        SAMPLE_RATE,
        frequency,
        output_skip_frames(SAMPLE_RATE) + lookahead_frames(),
        mono.len()
            .saturating_sub((output_skip_frames(SAMPLE_RATE) + lookahead_frames()) * 2),
    )?;
    Ok(fit.thdn_db)
}

fn measure_limiter(
    frames: usize,
    frequency: f64,
    sine_amplitude: f64,
    transparent_sine_thdn_db: f64,
) -> Result<LimiterSection, String> {
    let mono = limiter_stress_signal(frames, SAMPLE_RATE, frequency, sine_amplitude);
    let input_peak = max_abs(&mono);
    let mut samples = stereo_from_mono(&mono);
    let mut limiter = PeakLimiter::new(
        CHANNELS,
        SAMPLE_RATE,
        LIMITER_THRESHOLD_DBFS,
        LIMITER_LOOKAHEAD_MS,
        LIMITER_RELEASE_MS,
    )
    .map_err(|error| error.to_string())?;
    limiter
        .process(&mut samples, CHANNELS)
        .map_err(|error| error.to_string())?;
    let output_peak = max_abs(&samples);
    let output_peak_dbfs = dbfs(output_peak);

    // Intersample-peak stress: Fs/4 sine sampled 45° off-peak so every sample
    // sits at amplitude·√½ (below the ceiling) while the reconstructed true peak
    // reaches `amplitude`. True-peak mode must pull the output true peak under
    // the ceiling; sample-peak mode leaves it above (it never engages).
    let stress_mono = intersample_stress_mono(frames, 1.0);
    let mut stress_tp = stereo_from_mono(&stress_mono);
    let mut stress_sp = stress_tp.clone();
    let mut true_peak_limiter = PeakLimiter::with_mode(
        CHANNELS,
        SAMPLE_RATE,
        LIMITER_THRESHOLD_DBFS,
        LIMITER_LOOKAHEAD_MS,
        LIMITER_RELEASE_MS,
        LimiterMode::TruePeak,
    )
    .map_err(|error| error.to_string())?;
    true_peak_limiter
        .process(&mut stress_tp, CHANNELS)
        .map_err(|error| error.to_string())?;
    let mut sample_peak_limiter = PeakLimiter::with_mode(
        CHANNELS,
        SAMPLE_RATE,
        LIMITER_THRESHOLD_DBFS,
        LIMITER_LOOKAHEAD_MS,
        LIMITER_RELEASE_MS,
        LimiterMode::SamplePeak,
    )
    .map_err(|error| error.to_string())?;
    sample_peak_limiter
        .process(&mut stress_sp, CHANNELS)
        .map_err(|error| error.to_string())?;

    Ok(LimiterSection {
        threshold_dbfs: LIMITER_THRESHOLD_DBFS,
        input_peak_dbfs: dbfs(input_peak),
        output_peak_dbfs,
        output_margin_to_threshold_db: output_peak_dbfs - LIMITER_THRESHOLD_DBFS,
        final_gain_reduction_db: limiter.gain_reduction_db(),
        transparent_sine_thdn_db,
        intersample_stress_input_sample_peak_dbfs: dbfs(max_abs(&stress_mono)),
        intersample_stress_input_true_peak_dbtp: measure_true_peak_db(
            &stereo_from_mono(&stress_mono),
            CHANNELS,
            SAMPLE_RATE,
        )?,
        intersample_stress_true_peak_mode_output_dbtp: measure_true_peak_db(
            &stress_tp,
            CHANNELS,
            SAMPLE_RATE,
        )?,
        intersample_stress_sample_peak_mode_output_dbtp: measure_true_peak_db(
            &stress_sp,
            CHANNELS,
            SAMPLE_RATE,
        )?,
    })
}

fn measure_stopband(frames: usize) -> Result<StopbandSection, String> {
    let frequencies = [30_000.0, 36_000.0, 42_000.0];
    let amplitude = db_to_linear(-1.0);
    let mut points = Vec::with_capacity(frequencies.len());

    for frequency in frequencies {
        let input = sine_mono(frames, STOPBAND_FROM, frequency, amplitude);
        let output = resample_mono(&input, STOPBAND_FROM, STOPBAND_TO)?;
        let skip = output_skip_frames(STOPBAND_TO);
        let take = output.len().saturating_sub(skip * 2);
        let folded = fold_frequency(frequency, STOPBAND_TO);
        let fit = fit_sine(&output, STOPBAND_TO, folded, skip, take)?;
        let residual = rms_window(&output, skip, take)?;
        let residual_amplitude = residual * 2.0_f64.sqrt();

        points.push(StopbandPoint {
            input_frequency_hz: frequency,
            folded_frequency_hz: folded,
            alias_attenuation_db: db_ratio(fit.amplitude, amplitude),
            residual_rms_attenuation_db: db_ratio(residual_amplitude, amplitude),
            output_alias_amplitude_dbfs: dbfs(fit.amplitude),
        });
    }

    let worst_alias_attenuation_db = points
        .iter()
        .map(|point| point.alias_attenuation_db)
        .fold(f64::NEG_INFINITY, f64::max);
    let worst_residual_attenuation_db = points
        .iter()
        .map(|point| point.residual_rms_attenuation_db)
        .fold(f64::NEG_INFINITY, f64::max);

    Ok(StopbandSection {
        from_rate_hz: STOPBAND_FROM,
        to_rate_hz: STOPBAND_TO,
        points,
        worst_alias_attenuation_db,
        worst_residual_attenuation_db,
    })
}

fn measure_saturation_continuity() -> SaturationContinuitySection {
    let mut points = Vec::new();

    for saturation_type in [
        SaturationType::Tape,
        SaturationType::Tube,
        SaturationType::Transistor,
    ] {
        for sign in [-1.0, 1.0] {
            let center = sign * SATURATION_CONTINUITY_THRESHOLD;
            let inside = center - sign * SATURATION_CONTINUITY_EPSILON;
            let outside = center + sign * SATURATION_CONTINUITY_EPSILON;
            let inside_output = saturation_transfer_sample(saturation_type, inside);
            let center_output = saturation_transfer_sample(saturation_type, center);
            let outside_output = saturation_transfer_sample(saturation_type, outside);
            let inside_first_derivative = (center_output - inside_output) / (center - inside);
            let outside_first_derivative = (outside_output - center_output) / (outside - center);

            points.push(SaturationContinuityPoint {
                saturation_type: saturation_type_name(saturation_type),
                sign,
                threshold_jump_linear: (outside_output - inside_output).abs(),
                inside_first_derivative,
                outside_first_derivative,
                first_derivative_mismatch: (outside_first_derivative - inside_first_derivative)
                    .abs(),
            });
        }
    }

    let max_threshold_jump_linear = points
        .iter()
        .map(|point| point.threshold_jump_linear)
        .fold(0.0, f64::max);
    let max_first_derivative_mismatch = points
        .iter()
        .map(|point| point.first_derivative_mismatch)
        .fold(0.0, f64::max);

    SaturationContinuitySection {
        threshold: SATURATION_CONTINUITY_THRESHOLD,
        drive: SATURATION_CONTINUITY_DRIVE,
        output_gain_db: SATURATION_CONTINUITY_OUTPUT_GAIN_DB,
        epsilon: SATURATION_CONTINUITY_EPSILON,
        points,
        max_threshold_jump_linear,
        max_first_derivative_mismatch,
    }
}

fn saturation_transfer_sample(saturation_type: SaturationType, input: f64) -> f64 {
    let mut saturation = Saturation::with_type(saturation_type);
    saturation.set_channel_count(1);
    saturation.set_quality(SaturationQuality::Direct);
    saturation.set_threshold(SATURATION_CONTINUITY_THRESHOLD);
    saturation.set_drive(SATURATION_CONTINUITY_DRIVE);
    saturation.set_mix(1.0);
    saturation.set_output_gain(SATURATION_CONTINUITY_OUTPUT_GAIN_DB);
    let latency_frames = saturation.latency_frames();
    let mut samples = vec![input; latency_frames + 1];
    saturation.process_with_channels(&mut samples, 1);
    samples[latency_frames]
}

fn saturation_type_name(saturation_type: SaturationType) -> &'static str {
    match saturation_type {
        SaturationType::Tape => "Tape",
        SaturationType::Tube => "Tube",
        SaturationType::Transistor => "Transistor",
    }
}

fn measure_saturation_aliasing(frames: usize) -> Result<SaturationAliasingSection, String> {
    let amplitude = db_to_linear(SATURATION_ALIAS_AMPLITUDE_DBFS);
    let input = sine_mono(
        frames,
        SAMPLE_RATE,
        SATURATION_ALIAS_STRESS_FREQUENCY_HZ,
        amplitude,
    );
    let direct = process_saturation_stress(&input, SaturationQuality::Direct);
    let upgraded = process_saturation_stress(&input, SaturationQuality::Oversampled4x);
    let skip = output_skip_frames(SAMPLE_RATE);
    let take = direct.len().saturating_sub(skip * 2);

    let direct_fundamental = fit_sine(
        &direct,
        SAMPLE_RATE,
        SATURATION_ALIAS_STRESS_FREQUENCY_HZ,
        skip,
        take,
    )?;
    let upgraded_fundamental = fit_sine(
        &upgraded,
        SAMPLE_RATE,
        SATURATION_ALIAS_STRESS_FREQUENCY_HZ,
        skip,
        take,
    )?;

    let mut points = Vec::new();
    let mut direct_alias_power = 0.0;
    let mut upgraded_alias_power = 0.0;
    for harmonic in [3_u32, 5, 7, 9] {
        let harmonic_frequency = SATURATION_ALIAS_STRESS_FREQUENCY_HZ * harmonic as f64;
        if harmonic_frequency <= SAMPLE_RATE as f64 / 2.0 {
            continue;
        }
        let folded_frequency = fold_frequency(harmonic_frequency, SAMPLE_RATE);
        if folded_frequency < 20.0 {
            continue;
        }

        let direct_fit = fit_sine(&direct, SAMPLE_RATE, folded_frequency, skip, take)?;
        let upgraded_fit = fit_sine(&upgraded, SAMPLE_RATE, folded_frequency, skip, take)?;
        direct_alias_power += direct_fit.amplitude * direct_fit.amplitude;
        upgraded_alias_power += upgraded_fit.amplitude * upgraded_fit.amplitude;

        points.push(SaturationAliasPoint {
            harmonic,
            folded_frequency_hz: folded_frequency,
            direct_alias_dbfs: dbfs(direct_fit.amplitude),
            upgraded_alias_dbfs: dbfs(upgraded_fit.amplitude),
            reduction_db: positive_db_ratio(direct_fit.amplitude, upgraded_fit.amplitude),
        });
    }

    if points.is_empty() {
        return Err("saturation aliasing probe produced no folded harmonics".to_string());
    }

    let direct_alias_energy = direct_alias_power.sqrt();
    let upgraded_alias_energy = upgraded_alias_power.sqrt();

    let direct_fundamental_dbfs = dbfs(direct_fundamental.amplitude);
    let upgraded_fundamental_dbfs = dbfs(upgraded_fundamental.amplitude);

    Ok(SaturationAliasingSection {
        sample_rate_hz: SAMPLE_RATE,
        saturation_type: "Tube",
        upgraded_quality: "Oversampled4x",
        stress_frequency_hz: SATURATION_ALIAS_STRESS_FREQUENCY_HZ,
        input_amplitude_dbfs: SATURATION_ALIAS_AMPLITUDE_DBFS,
        drive: SATURATION_ALIAS_DRIVE,
        mix: SATURATION_ALIAS_MIX,
        direct_fundamental_dbfs,
        upgraded_fundamental_dbfs,
        fundamental_delta_db: upgraded_fundamental_dbfs - direct_fundamental_dbfs,
        direct_alias_energy_dbfs: dbfs(direct_alias_energy),
        upgraded_alias_energy_dbfs: dbfs(upgraded_alias_energy),
        alias_reduction_db: positive_db_ratio(direct_alias_energy, upgraded_alias_energy),
        points,
    })
}

fn process_saturation_stress(input: &[f64], quality: SaturationQuality) -> Vec<f64> {
    let mut saturation = Saturation::with_type(SaturationType::Tube);
    saturation.set_channel_count(1);
    saturation.set_sample_rate(SAMPLE_RATE as f64);
    saturation.set_quality(quality);
    saturation.set_threshold(0.0);
    saturation.set_drive(SATURATION_ALIAS_DRIVE);
    saturation.set_mix(SATURATION_ALIAS_MIX);
    saturation.set_input_gain(0.0);
    saturation.set_output_gain(0.0);

    let mut output = input.to_vec();
    saturation.process_with_channels(&mut output, 1);
    output
}

fn measure_listening_dsp(frames: usize) -> Result<ListeningDspSection, String> {
    Ok(ListeningDspSection {
        sample_rate_hz: SAMPLE_RATE,
        eq: measure_listening_eq(frames)?,
        crossfeed: measure_listening_crossfeed(frames)?,
        dynamic_loudness: measure_listening_dynamic_loudness(frames)?,
    })
}

fn measure_listening_eq(frames: usize) -> Result<ListeningEqSection, String> {
    let frequencies = [62.0, 1_000.0, 8_000.0];
    let bands = [1usize, 5usize, 8usize];
    let amplitude = db_to_linear(LISTENING_DSP_AMPLITUDE_DBFS);
    let mut points = Vec::with_capacity(frequencies.len());

    for (frequency, band) in frequencies.into_iter().zip(bands) {
        let input_mono = sine_mono(frames, SAMPLE_RATE, frequency, amplitude);
        let mut samples = stereo_from_mono(&input_mono);
        let mut eq = Equalizer::new(CHANNELS, SAMPLE_RATE as f64);
        let mut gains = [0.0; EQ_BANDS];
        gains[band] = LISTENING_EQ_TARGET_GAIN_DB;
        eq.set_enabled(true);
        eq.set_all_bands(&gains, SAMPLE_RATE as f64)
            .map_err(|err| err.to_string())?;
        eq.process(&mut samples);

        let left = extract_channel(&samples, CHANNELS, 0);
        let skip = output_skip_frames(SAMPLE_RATE);
        let fit = fit_sine(
            &left,
            SAMPLE_RATE,
            frequency,
            skip,
            left.len().saturating_sub(skip * 2),
        )?;
        let measured_gain_db = db_ratio(fit.amplitude, amplitude);
        let target_error_db = measured_gain_db - LISTENING_EQ_TARGET_GAIN_DB;
        points.push(ListeningEqPoint {
            frequency_hz: frequency,
            measured_gain_db,
            target_gain_db: LISTENING_EQ_TARGET_GAIN_DB,
            target_error_db,
        });
    }

    let max_abs_target_error_db = points
        .iter()
        .map(|point| point.target_error_db.abs())
        .fold(0.0, f64::max);

    Ok(ListeningEqSection {
        target_gain_db: LISTENING_EQ_TARGET_GAIN_DB,
        points,
        max_abs_target_error_db,
    })
}

fn measure_listening_crossfeed(frames: usize) -> Result<ListeningCrossfeedSection, String> {
    let low_crossfeed_db = measure_crossfeed_right_gain(frames, LISTENING_CROSSFEED_LOW_HZ)?;
    let high_crossfeed_db = measure_crossfeed_right_gain(frames, LISTENING_CROSSFEED_HIGH_HZ)?;
    let (reference_dc_direct_gain, reference_dc_cross_gain, reference_max_abs_error) =
        measure_crossfeed_dc_reference();
    let continuity = measure_crossfeed_mix_change_continuity()?;

    Ok(ListeningCrossfeedSection {
        mix: LISTENING_CROSSFEED_MIX,
        cutoff_hz: LISTENING_CROSSFEED_CUTOFF_HZ,
        low_frequency_hz: LISTENING_CROSSFEED_LOW_HZ,
        high_frequency_hz: LISTENING_CROSSFEED_HIGH_HZ,
        low_crossfeed_db,
        high_crossfeed_db,
        low_vs_high_crossfeed_db: low_crossfeed_db - high_crossfeed_db,
        reference_dc_direct_gain,
        reference_dc_cross_gain,
        reference_max_abs_error,
        mix_change_first_frame_delta: continuity.first_frame_delta,
        mix_change_preserved_max_delta: continuity.preserved_max_delta,
        mix_change_legacy_reset_max_delta: continuity.legacy_reset_max_delta,
    })
}

fn measure_crossfeed_dc_reference() -> (f64, f64, f64) {
    let mut samples = vec![0.0; 8_192 * CHANNELS];
    for frame in samples.chunks_exact_mut(CHANNELS) {
        frame[0] = 1.0;
    }
    let mut crossfeed =
        Crossfeed::with_params(SAMPLE_RATE as f64, LISTENING_CROSSFEED_CUTOFF_HZ, 1.0);
    crossfeed.process(&mut samples, CHANNELS);
    let direct = samples[samples.len() - 2];
    let cross = samples[samples.len() - 1];
    let error = (direct - BAUER_REFERENCE_DC_DIRECT_GAIN)
        .abs()
        .max((cross - BAUER_REFERENCE_DC_CROSS_GAIN).abs());
    (direct, cross, error)
}

fn measure_crossfeed_right_gain(frames: usize, frequency: f64) -> Result<f64, String> {
    let amplitude = db_to_linear(LISTENING_DSP_AMPLITUDE_DBFS);
    let left = sine_mono(frames, SAMPLE_RATE, frequency, amplitude);
    let mut samples = Vec::with_capacity(frames * CHANNELS);
    for sample in left {
        samples.push(sample);
        samples.push(0.0);
    }

    let mut crossfeed = Crossfeed::with_params(
        SAMPLE_RATE as f64,
        LISTENING_CROSSFEED_CUTOFF_HZ,
        LISTENING_CROSSFEED_MIX,
    );
    crossfeed.process(&mut samples, CHANNELS);

    let right = extract_channel(&samples, CHANNELS, 1);
    let skip = output_skip_frames(SAMPLE_RATE);
    let fit = fit_sine(
        &right,
        SAMPLE_RATE,
        frequency,
        skip,
        right.len().saturating_sub(skip * 2),
    )?;
    Ok(db_ratio(fit.amplitude, amplitude))
}

struct CrossfeedContinuityResult {
    first_frame_delta: f64,
    preserved_max_delta: f64,
    legacy_reset_max_delta: f64,
}

/// How a parameter's transition behaviour is *documented* to work. This is a
/// declaration of intent read from the implementation, not a measurement: the
/// probe's job is to check the code still matches it.
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SmoothingKind {
    /// The processor ramps this parameter over a documented window, so the
    /// per-sample step is bounded by `authority / frames`. Gated.
    Smoothed,
    /// The processor applies this parameter as a hard switch by design (bypass
    /// gating, quantizer geometry, detector topology). The measured step is
    /// recorded as evidence of its size, not as a pass/fail. Report-only.
    HardSwitch,
    /// The parameter changes a rate or a mapping rather than an output level,
    /// so it has no direct step of its own. Report-only: the number is here so
    /// that a future change which *does* introduce a step becomes visible.
    RateOnly,
    /// A continuous parameter that reaches the signal path with **no** ramp,
    /// and whose discontinuity is not a documented design decision. This is a
    /// finding, not a contract. Report-only here because fixing smoothing
    /// behaviour is out of this probe's scope; the recorded number is the size
    /// of the step a listener would hear.
    Unsmoothed,
}

/// One (processor, field) transition probe.
struct ParameterTransitionCase {
    /// Full, stable metric name. Written out per case rather than formatted from
    /// a prefix because [`MetricResult::name`] is `&'static str`.
    key: &'static str,
    /// Which processor owns the parameter, for the report and the docs table.
    processor: &'static str,
    /// Which adapter to instantiate for this case.
    proc: ProcKind,
    /// The `Atomic*Params` field being stepped.
    field: &'static str,
    kind: SmoothingKind,
    /// Documented ramp length in frames at [`SAMPLE_RATE`]. `1` means the value
    /// takes effect on the next sample.
    smoothing_frames: f64,
    /// Where the documented window comes from, for the docs table.
    smoothing_source: &'static str,
    /// Probe tone frequency. Chosen per case so the parameter has authority
    /// over the signal; an inert choice shows up as a `skipped` case.
    probe_hz: f64,
    /// Probe tone level. The limiter cases need a level above the ceiling for
    /// the gain path to engage at all, and the noise-shaper cases need a level
    /// where quantization is audible.
    probe_dbfs: f64,
    /// Whether the probe is hard-panned (left only). The crossfeed cases need
    /// this: crossfeed acts on inter-channel bleed, so a dual-mono probe leaves
    /// `mix` and `cutoff_hz` with no authority at all.
    panned: bool,
    /// Puts the processor into the state where this field has authority, before
    /// any audio runs and before the adapter is built. Most processors ship
    /// disabled (`Saturation` also ships unarmed), so without this the probe
    /// would measure a bypassed path and land in `skipped`.
    ///
    /// This publishes the *pre-step* state, so it must not touch the field under
    /// test; `apply(Stage::From)` owns that.
    setup: fn(&ParamHandles),
    /// Publishes either the pre-step or the post-step value of this one field.
    /// A function pointer rather than a pair of `f64`s so each case states its
    /// two values in the field's own units and types. The `usize` carries the
    /// EQ band index; every other case ignores it.
    apply: fn(&ParamHandles, Stage, usize),
    /// EQ band index. Unused (`0`) for the other five processors.
    index: usize,
    /// Human-readable "from -> to", for the report and the docs table.
    transition: &'static str,
}

/// Which adapter a case drives. The adapters have different constructors, so the
/// probe body dispatches on this rather than on the display name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProcKind {
    Eq,
    Saturation,
    Crossfeed,
    Limiter,
    Volume,
    NoiseShaper,
}

/// No-op setup, for the processors that already ship in a state where the field
/// under test has authority (`PeakLimiter` and `Volume`).
fn param_step_no_setup(_: &ParamHandles) {}

/// Which end of a case's transition to publish.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    From,
    To,
}

/// The six in-scope parameter publishers, all live at once so one probe body
/// serves every case.
struct ParamHandles {
    eq: Arc<AtomicEqParams>,
    saturation: Arc<AtomicSaturationParams>,
    crossfeed: Arc<AtomicCrossfeedParams>,
    limiter: Arc<AtomicPeakLimiterParams>,
    volume: Arc<AtomicVolumeParams>,
    noise_shaper: Arc<AtomicNoiseShaperParams>,
}

/// EQ band centre frequencies, mirroring the private `Equalizer::FREQUENCIES`
/// table so each band can be probed where it has authority.
///
/// A drift between this list and the real one only moves the probe tone off the
/// band centre, which *lowers* the measured authority and can push a case below
/// [`PARAM_STEP_AUTHORITY_FLOOR`] into `skipped`. It cannot turn a continuous
/// parameter into a falsely passing gate, so the duplication is safe in the one
/// direction that matters.
const PARAM_STEP_EQ_BAND_HZ: [f64; EQ_BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// Every field of every in-scope `Atomic*Params`, one case each.
///
/// Exhaustiveness is enforced by `parameter_transition_cases_cover_every_field`
/// in `tests/`, which destructures each snapshot so a newly added field is a
/// compile error until it appears here.
fn parameter_transition_cases() -> Vec<ParameterTransitionCase> {
    let mut cases = Vec::new();

    // --- Equalizer -------------------------------------------------------
    // Per-band gains crossfade over EQ_SMOOTH_SAMPLES; `enabled` is a bypass
    // gate in `process_fixed_1_to_1` and steps.
    for (band, &hz) in PARAM_STEP_EQ_BAND_HZ.iter().enumerate() {
        cases.push(ParameterTransitionCase {
            key: PARAM_STEP_EQ_BAND_KEYS[band],
            processor: "Equalizer",
            proc: ProcKind::Eq,
            field: "gains[n]",
            kind: SmoothingKind::Smoothed,
            smoothing_frames: 1_024.0,
            smoothing_source: "EQ_SMOOTH_SAMPLES (eq.rs)",
            probe_hz: hz,
            probe_dbfs: PARAM_STEP_AMPLITUDE_DBFS,
            panned: false,
            // Ships disabled; the bypass gate would hide the band gain entirely.
            setup: |h| h.eq.set_enabled(true),
            apply: |h, stage, band| {
                h.eq.set_band_gain(band, if stage == Stage::From { 0.0 } else { 9.0 });
            },
            index: band,
            transition: "0 -> +9 dB",
        });
    }
    cases.push(ParameterTransitionCase {
        key: "param_step_eq_enabled",
        processor: "Equalizer",
        proc: ProcKind::Eq,
        field: "enabled",
        kind: SmoothingKind::HardSwitch,
        smoothing_frames: 1.0,
        smoothing_source: "process_fixed_1_to_1 bypass gate",
        probe_hz: 1_000.0,
        probe_dbfs: PARAM_STEP_AMPLITUDE_DBFS,
        panned: false,
        // A flat EQ would make the bypass inaudible, so give the 1 kHz band gain
        // for the switch to reveal. `enabled` itself stays with `apply`.
        setup: |h| h.eq.set_band_gain(5, 9.0),
        apply: |h, stage, _| h.eq.set_enabled(stage == Stage::To),
        index: 0,
        transition: "false -> true (with +9 dB at 1 kHz)",
    });

    cases.extend(parameter_transition_saturation_cases());
    cases.extend(parameter_transition_remaining_cases());
    cases
}

/// Stable per-band metric names. Written out rather than formatted so they stay
/// `&'static str`, which `MetricResult` requires.
const PARAM_STEP_EQ_BAND_KEYS: [&str; EQ_BANDS] = [
    "param_step_eq_gain_band0",
    "param_step_eq_gain_band1",
    "param_step_eq_gain_band2",
    "param_step_eq_gain_band3",
    "param_step_eq_gain_band4",
    "param_step_eq_gain_band5",
    "param_step_eq_gain_band6",
    "param_step_eq_gain_band7",
    "param_step_eq_gain_band8",
    "param_step_eq_gain_band9",
];

/// Probe level for the saturation cases: 0.794 linear.
const PARAM_STEP_SATURATION_DBFS: f64 = -2.0;

/// Knee threshold the saturation probes run against.
///
/// `apply_thresholded_saturation` returns its input untouched while
/// `|input| <= threshold`, and the shipped default is `0.88` — above the probe's
/// 0.794, so at the default the waveshaper never engages and `drive`, `mix`,
/// `sat_type` and `quality` all measure exactly zero authority. Lowering the
/// knee to 0.5 puts the probe well inside it (the knee is 0.05 wide).
const PARAM_STEP_SATURATION_THRESHOLD: f64 = 0.5;

/// Puts `Saturation` where its continuous parameters reach the waveshaper.
///
/// `enabled` and `armed` already ship `true`; setting them explicitly keeps the
/// probe independent of the shipped defaults. The threshold is what actually
/// matters — see [`PARAM_STEP_SATURATION_THRESHOLD`].
fn param_step_saturation_setup(h: &ParamHandles) {
    h.saturation.set_enabled(true);
    h.saturation.set_armed(true);
    h.saturation.set_threshold(PARAM_STEP_SATURATION_THRESHOLD);
}

/// `AtomicSaturationParams`: 11 fields.
///
/// Six of them (`drive`, `threshold`, `mix`, `input_gain_db`, `output_gain_db`,
/// `highpass_cutoff`) reach the waveshaper with no ramp at all — `set_drive` and
/// friends in `saturation.rs:395-446` sanitize and assign. Those are recorded as
/// [`SmoothingKind::Unsmoothed`] findings; this probe measures, it does not
/// redesign the ramps.
fn parameter_transition_saturation_cases() -> Vec<ParameterTransitionCase> {
    vec![
        ParameterTransitionCase {
            key: "param_step_saturation_drive",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "drive",
            kind: SmoothingKind::Unsmoothed,
            smoothing_frames: 1.0,
            smoothing_source: "none (set_drive assigns directly)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            setup: param_step_saturation_setup,
            apply: |h, stage, _| {
                h.saturation
                    .set_drive(if stage == Stage::From { 1.0 } else { 2.0 });
            },
            index: 0,
            transition: "1.0 -> 2.0",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_threshold",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "threshold",
            kind: SmoothingKind::Unsmoothed,
            smoothing_frames: 1.0,
            smoothing_source: "none (set_threshold assigns directly)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            // `threshold` is the field under test, so setup must not set it.
            // Both ends sit below the probe's 0.794, so the knee is engaged
            // throughout and the step moves how deep into it the signal sits.
            setup: |h| {
                h.saturation.set_enabled(true);
                h.saturation.set_armed(true);
            },
            apply: |h, stage, _| {
                h.saturation
                    .set_threshold(if stage == Stage::From { 0.7 } else { 0.4 });
            },
            index: 0,
            transition: "0.7 -> 0.4",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_mix",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "mix",
            kind: SmoothingKind::Unsmoothed,
            smoothing_frames: 1.0,
            smoothing_source: "none (set_mix assigns directly)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            setup: param_step_saturation_setup,
            apply: |h, stage, _| {
                h.saturation
                    .set_mix(if stage == Stage::From { 0.5 } else { 1.0 });
            },
            index: 0,
            transition: "0.5 -> 1.0 (dry/wet)",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_input_gain_db",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "input_gain_db",
            kind: SmoothingKind::Unsmoothed,
            smoothing_frames: 1.0,
            smoothing_source: "none (set_input_gain assigns directly)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            setup: param_step_saturation_setup,
            apply: |h, stage, _| {
                h.saturation
                    .set_input_gain(if stage == Stage::From { 0.0 } else { 6.0 });
            },
            index: 0,
            transition: "0 -> +6 dB",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_output_gain_db",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "output_gain_db",
            kind: SmoothingKind::Unsmoothed,
            smoothing_frames: 1.0,
            smoothing_source: "none (set_output_gain assigns directly)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            setup: param_step_saturation_setup,
            apply: |h, stage, _| {
                h.saturation
                    .set_output_gain(if stage == Stage::From { 0.0 } else { -6.0 });
            },
            index: 0,
            transition: "0 -> -6 dB",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_highpass_cutoff",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "highpass_cutoff",
            kind: SmoothingKind::Unsmoothed,
            smoothing_frames: 1.0,
            smoothing_source: "none (set_highpass_cutoff assigns directly)",
            // Between the two corner positions, so the step moves the probe from
            // inside the saturated band to outside it.
            probe_hz: 3_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            // The cutoff only reaches the signal path with highpass mode on.
            // Both ends must stay inside
            // [SATURATION_HIGHPASS_CUTOFF_HZ_MIN, ..MAX] = [1000, 12000] or the
            // setter clamps them to the same value and the case reads zero.
            setup: |h| {
                param_step_saturation_setup(h);
                h.saturation.set_highpass_mode(true);
                h.saturation.set_highpass_cutoff(1_500.0);
            },
            apply: |h, stage, _| {
                h.saturation.set_highpass_cutoff(if stage == Stage::From {
                    1_500.0
                } else {
                    6_000.0
                });
            },
            index: 0,
            transition: "1500 -> 6000 Hz",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_sat_type",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "sat_type",
            kind: SmoothingKind::HardSwitch,
            smoothing_frames: 1.0,
            smoothing_source: "curve identity swap (no crossfade)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            setup: param_step_saturation_setup,
            apply: |h, stage, _| {
                h.saturation.set_sat_type(if stage == Stage::From {
                    SaturationTypeValue::Tape
                } else {
                    SaturationTypeValue::Transistor
                });
            },
            index: 0,
            transition: "Tape -> Transistor",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_quality",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "quality",
            kind: SmoothingKind::Smoothed,
            smoothing_frames: 32.0,
            smoothing_source: "SATURATION_TRANSITION_FRAMES (adapters.rs)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            setup: param_step_saturation_setup,
            apply: |h, stage, _| {
                h.saturation.set_quality(if stage == Stage::From {
                    SaturationQualityValue::Direct
                } else {
                    SaturationQualityValue::Oversampled4x
                });
            },
            index: 0,
            transition: "Direct -> Oversampled4x",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_highpass_mode",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "highpass_mode",
            kind: SmoothingKind::HardSwitch,
            smoothing_frames: 1.0,
            smoothing_source: "HPF topology insert (no crossfade)",
            // Below the corner: fullband mode saturates this probe, highpass mode
            // largely does not, which is the authority the switch has.
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            setup: |h| {
                param_step_saturation_setup(h);
                h.saturation.set_highpass_cutoff(4_000.0);
            },
            apply: |h, stage, _| h.saturation.set_highpass_mode(stage == Stage::To),
            index: 0,
            transition: "false -> true (4 kHz corner)",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_enabled",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "enabled",
            kind: SmoothingKind::Smoothed,
            smoothing_frames: 32.0,
            smoothing_source: "SATURATION_TRANSITION_FRAMES (adapters.rs)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            // Armed but soft-disabled: `enabled` is the field under test. Armed
            // means the core keeps running, so this is a weight crossfade with no
            // latency change.
            setup: |h| {
                h.saturation.set_armed(true);
                h.saturation.set_threshold(PARAM_STEP_SATURATION_THRESHOLD);
            },
            apply: |h, stage, _| h.saturation.set_enabled(stage == Stage::To),
            index: 0,
            transition: "false -> true",
        },
        ParameterTransitionCase {
            key: "param_step_saturation_armed",
            processor: "Saturation",
            proc: ProcKind::Saturation,
            field: "armed",
            kind: SmoothingKind::HardSwitch,
            smoothing_frames: 1.0,
            smoothing_source: "arming gate ahead of the effect transition",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_SATURATION_DBFS,
            panned: false,
            // Enabled but unarmed: `armed` is the field under test.
            setup: |h| {
                h.saturation.set_enabled(true);
                h.saturation.set_threshold(PARAM_STEP_SATURATION_THRESHOLD);
            },
            apply: |h, stage, _| h.saturation.set_armed(stage == Stage::To),
            index: 0,
            // `armed` is a setup-time decision, so a mid-stream step is refused:
            // the adapter latches `stream_started` on any call that moves frames,
            // bypassed or not, and `sync_params` then declines to reconcile
            // `hard_bypassed`. Run B therefore matches its control bit-for-bit and
            // records a 0.0 excess step. The publish is remembered and applies at
            // the next reset, which is what `set_armed` documents.
            //
            // This row stays `report`, not `gate`, and always will: passes A and C
            // sit on either side of the arming boundary, so `latency_shift_frames`
            // is `SATURATION_LATENCY_FRAMES` by construction and the latency-shift
            // rule classifies before reaching `kind`. Enforcement of the refusal
            // lives in the adapter unit tests; this row is evidence, not a gate.
            transition:
                "false -> true (refused mid-stream; A/C straddle the 0 -> 4 frame latency step)",
        },
    ]
}

/// Probe level for the limiter cases: above the default -1 dBFS ceiling, so the
/// gain-reduction path actually engages and the threshold has authority.
const PARAM_STEP_LIMITER_DBFS: f64 = 2.0;

/// Probe level for the noise-shaper cases. Quantization to 16 bits is inaudible
/// against a -12 dBFS tone; near the LSB it dominates, which is where `bits` and
/// `curve` have authority. Matches the existing noise-shaping section's level.
const PARAM_STEP_NOISE_SHAPER_DBFS: f64 = -90.0;

/// `AtomicCrossfeedParams` (3), `AtomicPeakLimiterParams` (4),
/// `AtomicVolumeParams` (2), and `AtomicNoiseShaperParams` (3).
fn parameter_transition_remaining_cases() -> Vec<ParameterTransitionCase> {
    vec![
        // --- Crossfeed ---------------------------------------------------
        // `mix` and `cutoff_hz` retarget over PARAMETER_RAMP_MS; `enabled` is a
        // bypass gate. All three need a hard-panned probe.
        ParameterTransitionCase {
            key: "param_step_crossfeed_mix",
            processor: "Crossfeed",
            proc: ProcKind::Crossfeed,
            field: "mix",
            kind: SmoothingKind::Smoothed,
            smoothing_frames: 480.0,
            smoothing_source: "PARAMETER_RAMP_MS = 10 ms (crossfeed.rs)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_AMPLITUDE_DBFS,
            panned: true,
            setup: |h| h.crossfeed.set_enabled(true),
            apply: |h, stage, _| {
                h.crossfeed
                    .set_mix(if stage == Stage::From { 0.3 } else { 0.7 });
            },
            index: 0,
            transition: "0.3 -> 0.7",
        },
        ParameterTransitionCase {
            key: "param_step_crossfeed_cutoff_hz",
            processor: "Crossfeed",
            proc: ProcKind::Crossfeed,
            field: "cutoff_hz",
            kind: SmoothingKind::Smoothed,
            smoothing_frames: 480.0,
            smoothing_source: "PARAMETER_RAMP_MS = 10 ms (crossfeed.rs)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_AMPLITUDE_DBFS,
            panned: true,
            setup: |h| {
                h.crossfeed.set_enabled(true);
                h.crossfeed.set_cutoff(700.0);
            },
            apply: |h, stage, _| {
                h.crossfeed
                    .set_cutoff(if stage == Stage::From { 700.0 } else { 1_500.0 });
            },
            index: 0,
            transition: "700 -> 1500 Hz",
        },
        ParameterTransitionCase {
            key: "param_step_crossfeed_enabled",
            processor: "Crossfeed",
            proc: ProcKind::Crossfeed,
            field: "enabled",
            kind: SmoothingKind::HardSwitch,
            smoothing_frames: 1.0,
            smoothing_source: "bypass gate (no crossfade)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_AMPLITUDE_DBFS,
            panned: true,
            setup: param_step_no_setup,
            apply: |h, stage, _| h.crossfeed.set_enabled(stage == Stage::To),
            index: 0,
            transition: "false -> true",
        },
        // --- PeakLimiter -------------------------------------------------
        // `threshold_db` moves the ceiling, which the 08-11 attack ramp covers
        // over the true-peak reconstruction delay. `release_ms` is a rate.
        ParameterTransitionCase {
            key: "param_step_limiter_threshold_db",
            processor: "PeakLimiter",
            proc: ProcKind::Limiter,
            field: "threshold_db",
            kind: SmoothingKind::Smoothed,
            smoothing_frames: 479.0,
            smoothing_source: "attack_frames_for(TRUE_PEAK_DELAY) (limiter.rs)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_LIMITER_DBFS,
            panned: false,
            setup: param_step_no_setup,
            apply: |h, stage, _| {
                h.limiter
                    .set_threshold(if stage == Stage::From { -1.0 } else { -6.0 });
            },
            index: 0,
            transition: "-1 -> -6 dBFS",
        },
        ParameterTransitionCase {
            key: "param_step_limiter_release_ms",
            processor: "PeakLimiter",
            proc: ProcKind::Limiter,
            field: "release_ms",
            kind: SmoothingKind::RateOnly,
            smoothing_frames: 1.0,
            smoothing_source: "release coefficient (rate, not level)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_LIMITER_DBFS,
            panned: false,
            setup: param_step_no_setup,
            apply: |h, stage, _| {
                h.limiter
                    .set_release(if stage == Stage::From { 100.0 } else { 20.0 });
            },
            index: 0,
            transition: "100 -> 20 ms",
        },
        ParameterTransitionCase {
            key: "param_step_limiter_mode",
            processor: "PeakLimiter",
            proc: ProcKind::Limiter,
            field: "mode",
            kind: SmoothingKind::HardSwitch,
            smoothing_frames: 1.0,
            smoothing_source: "detector topology and output delay change",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_LIMITER_DBFS,
            panned: false,
            setup: param_step_no_setup,
            apply: |h, stage, _| {
                h.limiter.set_mode(if stage == Stage::From {
                    LimiterMode::TruePeak
                } else {
                    LimiterMode::SamplePeak
                });
            },
            index: 0,
            transition: "TruePeak -> SamplePeak",
        },
        ParameterTransitionCase {
            key: "param_step_limiter_enabled",
            processor: "PeakLimiter",
            proc: ProcKind::Limiter,
            field: "enabled",
            kind: SmoothingKind::HardSwitch,
            smoothing_frames: 1.0,
            smoothing_source: "bypass gate (no crossfade)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_LIMITER_DBFS,
            panned: false,
            // Ships enabled; `enabled` is the field under test, so start it off.
            setup: |h| h.limiter.set_enabled(false),
            apply: |h, stage, _| h.limiter.set_enabled(stage == Stage::To),
            index: 0,
            transition: "false -> true",
        },
        // --- Volume ------------------------------------------------------
        // Both fields run through the 5 ms exponential smoother. `muted` is
        // documented in the adapter as a smoothed gain change, not a bypass.
        ParameterTransitionCase {
            key: "param_step_volume_volume",
            processor: "Volume",
            proc: ProcKind::Volume,
            field: "volume",
            kind: SmoothingKind::Smoothed,
            smoothing_frames: 240.0,
            smoothing_source: "5 ms exponential smoother (adapters.rs)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_AMPLITUDE_DBFS,
            panned: false,
            setup: param_step_no_setup,
            apply: |h, stage, _| {
                h.volume
                    .set_volume(if stage == Stage::From { 1.0 } else { 0.25 });
            },
            index: 0,
            transition: "1.0 -> 0.25",
        },
        ParameterTransitionCase {
            key: "param_step_volume_muted",
            processor: "Volume",
            proc: ProcKind::Volume,
            field: "muted",
            kind: SmoothingKind::Smoothed,
            smoothing_frames: 240.0,
            smoothing_source: "5 ms exponential smoother (adapters.rs)",
            probe_hz: 1_000.0,
            probe_dbfs: PARAM_STEP_AMPLITUDE_DBFS,
            panned: false,
            setup: param_step_no_setup,
            apply: |h, stage, _| h.volume.set_muted(stage == Stage::To),
            index: 0,
            transition: "false -> true",
        },
        // --- NoiseShaper -------------------------------------------------
        // All three fields are immediate by design: the quantizer geometry and
        // the noise-transfer filter cannot be crossfaded without dithering twice.
        ParameterTransitionCase {
            key: "param_step_noise_shaper_enabled",
            processor: "NoiseShaper",
            proc: ProcKind::NoiseShaper,
            field: "enabled",
            kind: SmoothingKind::HardSwitch,
            smoothing_frames: 1.0,
            smoothing_source: "quantizer bypass (no crossfade)",
            probe_hz: NOISE_STIMULUS_FREQUENCY_HZ,
            probe_dbfs: PARAM_STEP_NOISE_SHAPER_DBFS,
            panned: false,
            // The shipped default is 24 bits, whose LSB (~1.2e-7) sits below
            // PARAM_STEP_AUTHORITY_FLOOR. 16 bits gives the quantizer authority
            // the probe can actually see. `enabled` stays the field under test.
            setup: |h| h.noise_shaper.set_bits(NOISE_SHAPER_BITS),
            apply: |h, stage, _| h.noise_shaper.set_enabled(stage == Stage::To),
            index: 0,
            // The TPDF stream only advances while the shaper is enabled, so the
            // steady runs hold uncorrelated dither. That inflates `authority` and
            // `natural_step` for this one case; it is report-only, and the
            // recorded number is still the size of the audible change.
            transition: "false -> true (dither streams desync; report-only)",
        },
        ParameterTransitionCase {
            key: "param_step_noise_shaper_bits",
            processor: "NoiseShaper",
            proc: ProcKind::NoiseShaper,
            field: "bits",
            kind: SmoothingKind::HardSwitch,
            smoothing_frames: 1.0,
            smoothing_source: "quantizer step size (no crossfade)",
            probe_hz: NOISE_STIMULUS_FREQUENCY_HZ,
            probe_dbfs: PARAM_STEP_NOISE_SHAPER_DBFS,
            panned: false,
            setup: |h| h.noise_shaper.set_enabled(true),
            apply: |h, stage, _| {
                h.noise_shaper
                    .set_bits(if stage == Stage::From { 16 } else { 8 });
            },
            index: 0,
            transition: "16 -> 8 bits",
        },
        ParameterTransitionCase {
            key: "param_step_noise_shaper_curve",
            processor: "NoiseShaper",
            proc: ProcKind::NoiseShaper,
            field: "curve",
            kind: SmoothingKind::HardSwitch,
            smoothing_frames: 1.0,
            smoothing_source: "noise-transfer filter swap (no crossfade)",
            probe_hz: NOISE_STIMULUS_FREQUENCY_HZ,
            probe_dbfs: PARAM_STEP_NOISE_SHAPER_DBFS,
            panned: false,
            // 16 bits so the curve difference clears the authority floor.
            setup: |h| {
                h.noise_shaper.set_enabled(true);
                h.noise_shaper.set_bits(NOISE_SHAPER_BITS);
            },
            apply: |h, stage, _| {
                h.noise_shaper.set_curve(if stage == Stage::From {
                    NoiseShaperCurve::Lipshitz5
                } else {
                    NoiseShaperCurve::FWeighted9
                });
            },
            index: 0,
            transition: "Lipshitz5 -> FWeighted9",
        },
    ]
}

/// Per-case measured result.
#[derive(Clone, Serialize)]
struct ParameterTransitionResult {
    key: &'static str,
    processor: &'static str,
    field: &'static str,
    kind: SmoothingKind,
    smoothing_frames: f64,
    smoothing_source: &'static str,
    probe_hz: f64,
    /// Largest settled output change the step produces, `max |C - A|`. This is
    /// the size of the transition the smoother has to cover.
    authority: f64,
    /// Worst per-sample step in the stepped run that is *not* explained by
    /// either endpoint's own slew: `max(0, |dB| - max(|dA|, |dC|))`.
    excess_step: f64,
    /// `authority / smoothing_frames * PARAM_STEP_BOUND_SAFETY`.
    bound: f64,
    /// Natural slew of the steady runs, for context in the report.
    natural_step: f64,
    /// Documented ramp length this case's bound was derived from.
    smoothing_frames_used: f64,
    /// Human-readable "from -> to".
    transition: &'static str,
    /// Reported latency difference between the pre-step and post-step
    /// configurations, in frames.
    ///
    /// Non-zero means the two steady runs are on different timelines, so `A` and
    /// `C` are compared while phase-shifted and `authority` measures that shift
    /// rather than a level change. Such a case cannot be gated; it is recorded
    /// with the shift stated so nobody reads the number as a step size.
    latency_shift_frames: i64,
}

/// One pass's captured output plus the latency the processor reported for it.
struct ParameterTransitionPass {
    samples: Vec<f64>,
    latency_frames: i64,
}

/// Runs one case's three passes and reduces them to a [`ParameterTransitionResult`].
///
/// * **A** holds the pre-step value for the whole run.
/// * **B** is identical to A until the step is published at the block boundary
///   on frame [`PARAM_STEP_WARMUP_FRAMES`], then continues.
/// * **C** holds the post-step value for the whole run, so it is fully settled.
///
/// `authority = max |C - A|` is how far the output has to travel. Subtracting
/// *both* steady runs' own per-sample slew from B's is what separates the step
/// from the signal's natural motion: a 16 kHz tone at -12 dBFS already moves
/// ~0.2 per sample, which would swamp any fixed threshold.
fn measure_parameter_transition(
    case: &ParameterTransitionCase,
) -> Result<ParameterTransitionResult, String> {
    let pass_a = run_parameter_transition_pass(case, false)?;
    let pass_b = run_parameter_transition_pass(case, true)?;
    let pass_c = run_parameter_transition_pass_settled(case)?;
    let latency_shift_frames = pass_c.latency_frames - pass_a.latency_frames;
    let (a, b, c) = (pass_a.samples, pass_b.samples, pass_c.samples);

    // The step lands at PARAM_STEP_WARMUP_FRAMES. Start one frame earlier so the
    // first differences that straddle the boundary are inside the window.
    let start = PARAM_STEP_WARMUP_FRAMES.saturating_sub(1);
    let mut authority = 0.0_f64;
    let mut natural_step = 0.0_f64;
    let mut excess_step = 0.0_f64;

    for ch in 0..CHANNELS {
        let a_ch = extract_channel(&a, CHANNELS, ch);
        let b_ch = extract_channel(&b, CHANNELS, ch);
        let c_ch = extract_channel(&c, CHANNELS, ch);
        if a_ch.len() != b_ch.len() || a_ch.len() != c_ch.len() {
            return Err(format!(
                "{}: parameter-transition passes returned different lengths",
                case.key
            ));
        }

        // Authority is measured over the settled tail, not across the ramp.
        let settle_from = PARAM_STEP_WARMUP_FRAMES.min(a_ch.len());
        for n in settle_from..a_ch.len() {
            authority = authority.max((c_ch[n] - a_ch[n]).abs());
        }

        for n in start..a_ch.len().saturating_sub(1) {
            let da = (a_ch[n + 1] - a_ch[n]).abs();
            let dc = (c_ch[n + 1] - c_ch[n]).abs();
            let db = (b_ch[n + 1] - b_ch[n]).abs();
            let natural = da.max(dc);
            natural_step = natural_step.max(natural);
            excess_step = excess_step.max((db - natural).max(0.0));
        }
    }

    Ok(ParameterTransitionResult {
        key: case.key,
        processor: case.processor,
        field: case.field,
        kind: case.kind,
        smoothing_frames: case.smoothing_frames,
        smoothing_source: case.smoothing_source,
        probe_hz: case.probe_hz,
        authority,
        excess_step,
        bound: authority / case.smoothing_frames * PARAM_STEP_BOUND_SAFETY,
        natural_step,
        smoothing_frames_used: case.smoothing_frames,
        transition: case.transition,
        latency_shift_frames,
    })
}

/// One pass. `step` selects run B (publish mid-stream) over run A (hold).
fn run_parameter_transition_pass(
    case: &ParameterTransitionCase,
    step: bool,
) -> Result<ParameterTransitionPass, String> {
    let handles = ParamHandles::new();
    (case.setup)(&handles);
    (case.apply)(&handles, Stage::From, case.index);
    let mut proc = build_parameter_transition_processor(case, &handles)?;

    let total = PARAM_STEP_WARMUP_FRAMES + PARAM_STEP_SETTLE_FRAMES;
    let mut captured = Vec::with_capacity(total * CHANNELS);
    let mut frame = 0usize;
    while frame < total {
        if step && frame == PARAM_STEP_WARMUP_FRAMES {
            (case.apply)(&handles, Stage::To, case.index);
        }
        let frames = PARAM_STEP_BLOCK_FRAMES.min(total - frame);
        let mut block = parameter_transition_probe(case, frames, frame);
        process_adapter_block(&mut *proc, &mut block, CHANNELS)?;
        captured.extend_from_slice(&block);
        frame += frames;
    }
    Ok(ParameterTransitionPass {
        samples: captured,
        latency_frames: proc.latency().frames() as i64,
    })
}

/// Run C: the post-step value is published before the adapter is built, so the
/// processor starts already settled at the destination.
fn run_parameter_transition_pass_settled(
    case: &ParameterTransitionCase,
) -> Result<ParameterTransitionPass, String> {
    let handles = ParamHandles::new();
    (case.setup)(&handles);
    (case.apply)(&handles, Stage::To, case.index);
    let mut proc = build_parameter_transition_processor(case, &handles)?;

    let total = PARAM_STEP_WARMUP_FRAMES + PARAM_STEP_SETTLE_FRAMES;
    let mut captured = Vec::with_capacity(total * CHANNELS);
    let mut frame = 0usize;
    while frame < total {
        let frames = PARAM_STEP_BLOCK_FRAMES.min(total - frame);
        let mut block = parameter_transition_probe(case, frames, frame);
        process_adapter_block(&mut *proc, &mut block, CHANNELS)?;
        captured.extend_from_slice(&block);
        frame += frames;
    }
    Ok(ParameterTransitionPass {
        samples: captured,
        latency_frames: proc.latency().frames() as i64,
    })
}

impl ParamHandles {
    /// All six publishers at their shipped defaults.
    fn new() -> Self {
        Self {
            eq: Arc::new(AtomicEqParams::new()),
            saturation: Arc::new(AtomicSaturationParams::new()),
            crossfeed: Arc::new(AtomicCrossfeedParams::new()),
            limiter: Arc::new(AtomicPeakLimiterParams::new()),
            volume: Arc::new(AtomicVolumeParams::new()),
            noise_shaper: Arc::new(AtomicNoiseShaperParams::new()),
        }
    }
}

fn build_parameter_transition_processor(
    case: &ParameterTransitionCase,
    handles: &ParamHandles,
) -> Result<Box<dyn StreamingProcessor>, String> {
    Ok(match case.proc {
        ProcKind::Eq => Box::new(EqProcessor::new(
            CHANNELS,
            SAMPLE_RATE as f64,
            Arc::clone(&handles.eq),
        )),
        ProcKind::Saturation => {
            // `SaturationProcessor::new` takes no rate and defaults to 44.1 kHz,
            // which its high-pass coefficients depend on. A real caller sets the
            // rate during setup, and two cases probe highpass mode.
            let mut proc = SaturationProcessor::new(CHANNELS, Arc::clone(&handles.saturation));
            proc.set_sample_rate(SAMPLE_RATE)
                .map_err(|err| err.to_string())?;
            Box::new(proc)
        }
        ProcKind::Crossfeed => Box::new(CrossfeedProcessor::new(
            SAMPLE_RATE as f64,
            Arc::clone(&handles.crossfeed),
        )),
        ProcKind::Limiter => Box::new(
            PeakLimiterProcessor::new(CHANNELS, SAMPLE_RATE, Arc::clone(&handles.limiter))
                .map_err(|err| err.to_string())?,
        ),
        ProcKind::Volume => {
            // Same: the constructor fixes the 5 ms smoother at 44.1 kHz
            // (220.5 frames). Without this the running smoother would not match
            // the `smoothing_frames` the bound is derived from.
            let mut proc = VolumeProcessor::new(Arc::clone(&handles.volume));
            proc.set_sample_rate(SAMPLE_RATE)
                .map_err(|err| err.to_string())?;
            Box::new(proc)
        }
        ProcKind::NoiseShaper => Box::new(
            NoiseShaperProcessor::new(CHANNELS, SAMPLE_RATE, Arc::clone(&handles.noise_shaper))
                .map_err(|err| err.to_string())?,
        ),
    })
}

#[derive(Serialize)]
struct ParameterTransitionSection {
    warmup_frames: usize,
    settle_frames: usize,
    block_frames: usize,
    bound_safety_factor: f64,
    authority_floor: f64,
    cases: Vec<ParameterTransitionResult>,
}

/// Every metric name the case table is required to contain, derived by
/// destructuring the six in-scope snapshots.
///
/// This is what makes PRD requirement 4 ("every parameter, not one per
/// processor") mechanical rather than a promise. Each `let ... = snapshot` below
/// is exhaustive with no `..` rest pattern, so adding a field to any
/// `Atomic*Params` snapshot stops this function compiling until the field is
/// named here, and [`assert_parameter_transition_coverage`] then fails until it
/// also has a case. `cargo build --benches` runs in CI, so the compile half of
/// that fence is enforced on every push.
///
/// Deliberately excludes `AtomicDynamicLoudnessParams`: the PRD scopes it out.
fn parameter_transition_required_keys() -> Vec<&'static str> {
    let mut required = Vec::new();

    let EqParamsSnapshot { gains, enabled: _ } = EqParamsSnapshot::default();
    for (band, _) in gains.iter().enumerate() {
        required.push(PARAM_STEP_EQ_BAND_KEYS[band]);
    }
    required.push("param_step_eq_enabled");

    let SaturationParamsSnapshot {
        drive: _,
        threshold: _,
        mix: _,
        sat_type: _,
        quality: _,
        input_gain_db: _,
        output_gain_db: _,
        highpass_mode: _,
        highpass_cutoff: _,
        enabled: _,
        armed: _,
    } = SaturationParamsSnapshot::default();
    required.extend_from_slice(&[
        "param_step_saturation_drive",
        "param_step_saturation_threshold",
        "param_step_saturation_mix",
        "param_step_saturation_sat_type",
        "param_step_saturation_quality",
        "param_step_saturation_input_gain_db",
        "param_step_saturation_output_gain_db",
        "param_step_saturation_highpass_mode",
        "param_step_saturation_highpass_cutoff",
        "param_step_saturation_enabled",
        "param_step_saturation_armed",
    ]);

    let CrossfeedParamsSnapshot {
        mix: _,
        cutoff_hz: _,
        enabled: _,
    } = CrossfeedParamsSnapshot::default();
    required.extend_from_slice(&[
        "param_step_crossfeed_mix",
        "param_step_crossfeed_cutoff_hz",
        "param_step_crossfeed_enabled",
    ]);

    let PeakLimiterParamsSnapshot {
        threshold_db: _,
        release_ms: _,
        enabled: _,
        mode: _,
    } = PeakLimiterParamsSnapshot::default();
    required.extend_from_slice(&[
        "param_step_limiter_threshold_db",
        PARAM_STEP_LIMITER_RELEASE_KEY,
        "param_step_limiter_enabled",
        "param_step_limiter_mode",
    ]);

    let VolumeParamsSnapshot {
        volume: _,
        muted: _,
    } = VolumeParamsSnapshot::default();
    required.extend_from_slice(&["param_step_volume_volume", "param_step_volume_muted"]);

    let NoiseShaperParamsSnapshot {
        enabled: _,
        bits: _,
        curve: _,
    } = NoiseShaperParamsSnapshot::default();
    required.extend_from_slice(&[
        "param_step_noise_shaper_enabled",
        "param_step_noise_shaper_bits",
        "param_step_noise_shaper_curve",
    ]);

    required
}

/// Fails the run if a required field has no case, or a case is duplicated.
fn assert_parameter_transition_coverage(cases: &[ParameterTransitionCase]) -> Result<(), String> {
    let present: Vec<&str> = cases.iter().map(|case| case.key).collect();

    let missing: Vec<&str> = parameter_transition_required_keys()
        .into_iter()
        .filter(|key| !present.contains(key))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "parameter-transition case table is missing {} field(s): {}. Every field of every \
             in-scope Atomic*Params needs a case, so a newly added unsmoothed parameter cannot \
             slip through.",
            missing.len(),
            missing.join(", ")
        ));
    }

    for (index, key) in present.iter().enumerate() {
        if present[index + 1..].contains(key) {
            return Err(format!(
                "parameter-transition case table has a duplicate metric name: {key}"
            ));
        }
    }
    Ok(())
}

fn measure_parameter_transitions() -> Result<ParameterTransitionSection, String> {
    let table = parameter_transition_cases();
    assert_parameter_transition_coverage(&table)?;
    let mut cases = Vec::new();
    for case in table {
        cases.push(measure_parameter_transition(&case)?);
    }
    Ok(ParameterTransitionSection {
        warmup_frames: PARAM_STEP_WARMUP_FRAMES,
        settle_frames: PARAM_STEP_SETTLE_FRAMES,
        block_frames: PARAM_STEP_BLOCK_FRAMES,
        bound_safety_factor: PARAM_STEP_BOUND_SAFETY,
        authority_floor: PARAM_STEP_AUTHORITY_FLOOR,
        cases,
    })
}

/// One metric row per case.
///
/// * No authority (the parameter could not reach the signal path in this
///   configuration) is `skipped`, never a silent pass.
/// * A documented ramp is a `gate` against `authority / frames * safety`.
/// * A hard switch, a rate parameter, or an unsmoothed finding is `report`: the
///   number records the size of the step rather than asserting it is small.
fn parameter_transition_metrics(section: &ParameterTransitionSection) -> Vec<MetricResult> {
    section
        .cases
        .iter()
        .map(|case| {
            let detail = format!(
                "{} {} {}: authority {:.6}, natural slew {:.6}, smoothing {} frames ({})",
                case.processor,
                case.field,
                case.transition,
                case.authority,
                case.natural_step,
                case.smoothing_frames_used,
                case.smoothing_source,
            );
            if case.authority < PARAM_STEP_AUTHORITY_FLOOR {
                return MetricResult::skipped(
                    case.key,
                    "amplitude",
                    format!(
                        "probe has no authority over this field (settled delta {:.3e} < floor \
                         {:.3e}); {}",
                        case.authority, PARAM_STEP_AUTHORITY_FLOOR, detail
                    ),
                );
            }
            // A field that changes the processor's reported latency puts the two
            // steady runs on different timelines, so `authority` is a phase
            // offset rather than a level change and the derived bound is
            // meaningless. Report it with the shift stated; never gate it.
            if case.latency_shift_frames != 0 {
                let mut metric = MetricResult::report(
                    case.key,
                    Comparison::AtMost,
                    case.excess_step,
                    case.bound,
                    "amplitude",
                );
                metric.detail = Some(format!(
                    "latency shifts {:+} frames across this step, so authority is a timeline \
                     offset, not a level change; not gated. {}",
                    case.latency_shift_frames, detail
                ));
                return metric;
            }
            let mut metric = match case.kind {
                SmoothingKind::Smoothed => MetricResult::gate(
                    case.key,
                    Comparison::AtMost,
                    case.excess_step,
                    case.bound,
                    "amplitude",
                ),
                SmoothingKind::HardSwitch | SmoothingKind::RateOnly | SmoothingKind::Unsmoothed => {
                    MetricResult::report(
                        case.key,
                        Comparison::AtMost,
                        case.excess_step,
                        case.bound,
                        "amplitude",
                    )
                }
            };
            metric.detail = Some(detail);
            metric
        })
        .collect()
}

/// Interleaved probe for `frames` frames starting at absolute frame
/// `start_frame`, so a run's phase is continuous across block boundaries and
/// identical between the three passes.
fn parameter_transition_probe(
    case: &ParameterTransitionCase,
    frames: usize,
    start_frame: usize,
) -> Vec<f64> {
    let amplitude = db_to_linear(case.probe_dbfs);
    let omega = 2.0 * PI * case.probe_hz / SAMPLE_RATE as f64;
    let burst = case.key == PARAM_STEP_LIMITER_RELEASE_KEY;
    let mut stereo = Vec::with_capacity(frames * CHANNELS);
    for frame in start_frame..start_frame + frames {
        let mut sample = amplitude * (omega * frame as f64).sin();
        if burst {
            sample *= param_step_burst_envelope(frame);
        }
        stereo.push(sample);
        stereo.push(if case.panned { 0.0 } else { sample });
    }
    stereo
}

/// Metric name of the one case that needs a bursted probe.
const PARAM_STEP_LIMITER_RELEASE_KEY: &str = "param_step_limiter_release_ms";

/// Burst period in frames: 2048 frames over the ceiling, then 6144 under it.
const PARAM_STEP_BURST_PERIOD_FRAMES: usize = 8_192;

/// Gate for the `release_ms` probe.
///
/// `release_ms` sets a recovery *rate*, so a continuous tone gives it no
/// authority at all: the limiter settles at a fixed gain reduction and the
/// settled outputs of the two release times are identical. Driving the limiter
/// over the ceiling and then dropping below it makes release govern the recovery
/// slope, which is the only place the parameter is observable.
///
/// The quiet span is 6144 frames (128 ms at 48 kHz), long enough for a 20 ms
/// release to finish recovering while a 100 ms one is still climbing.
fn param_step_burst_envelope(frame: usize) -> f64 {
    if frame % PARAM_STEP_BURST_PERIOD_FRAMES < 2_048 {
        1.0
    } else {
        // Well under the -1 dBFS ceiling, so the limiter releases rather than
        // holding gain reduction.
        db_to_linear(-18.0)
    }
}

fn process_adapter_block<P: StreamingProcessor + ?Sized>(
    processor: &mut P,
    samples: &mut [f64],
    channels: usize,
) -> Result<(), String> {
    let block = AudioBlockMut::new(samples, channels).map_err(|err| err.to_string())?;
    let _ = process_checked(processor, ProcessBuffers::in_place(block))
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn measure_crossfeed_mix_change_continuity() -> Result<CrossfeedContinuityResult, String> {
    let params = Arc::new(AtomicCrossfeedParams::new());
    let mut proc = CrossfeedProcessor::new(SAMPLE_RATE as f64, Arc::clone(&params));
    let mut reference = Crossfeed::with_params(
        SAMPLE_RATE as f64,
        LISTENING_CROSSFEED_CUTOFF_HZ,
        LISTENING_CROSSFEED_MIX,
    );
    let mut legacy_reset = Crossfeed::with_params(
        SAMPLE_RATE as f64,
        LISTENING_CROSSFEED_CUTOFF_HZ,
        LISTENING_CROSSFEED_MIX,
    );
    let mut old_mix_reference = Crossfeed::with_params(
        SAMPLE_RATE as f64,
        LISTENING_CROSSFEED_CUTOFF_HZ,
        LISTENING_CROSSFEED_MIX,
    );

    let warm = hard_panned_sine_for_frame_range(4096, 0, LISTENING_CROSSFEED_HIGH_HZ);
    let mut proc_warm = warm.clone();
    let mut reference_warm = warm.clone();
    let mut legacy_warm = warm;
    let mut old_mix_warm = legacy_warm.clone();
    process_adapter_block(&mut proc, &mut proc_warm, CHANNELS)?;
    reference.process(&mut reference_warm, CHANNELS);
    legacy_reset.process(&mut legacy_warm, CHANNELS);
    old_mix_reference.process(&mut old_mix_warm, CHANNELS);

    let changed_mix = 0.7;
    params.set_mix(changed_mix);
    reference.set_mix(changed_mix);
    legacy_reset.set_mix(changed_mix);
    legacy_reset.set_sample_rate(SAMPLE_RATE as f64, LISTENING_CROSSFEED_CUTOFF_HZ);

    let next = hard_panned_sine_for_frame_range(512, 4096, LISTENING_CROSSFEED_HIGH_HZ);
    let mut proc_next = next.clone();
    let mut reference_next = next.clone();
    let mut legacy_next = next;
    let mut old_mix_next = legacy_next.clone();
    process_adapter_block(&mut proc, &mut proc_next, CHANNELS)?;
    reference.process(&mut reference_next, CHANNELS);
    legacy_reset.process(&mut legacy_next, CHANNELS);
    old_mix_reference.process(&mut old_mix_next, CHANNELS);

    Ok(CrossfeedContinuityResult {
        first_frame_delta: max_abs_delta(&proc_next[..CHANNELS], &old_mix_next[..CHANNELS]),
        preserved_max_delta: max_abs_delta(&proc_next, &reference_next),
        legacy_reset_max_delta: max_abs_delta(&proc_next, &legacy_next),
    })
}

fn measure_listening_dynamic_loudness(
    frames: usize,
) -> Result<ListeningDynamicLoudnessSection, String> {
    let bass_probe_hz = 40.0;
    let presence_probe_hz = 3_000.0;
    let bass_compensation_db = measure_dynamic_loudness_compensation(frames, bass_probe_hz)?;
    let presence_compensation_db =
        measure_dynamic_loudness_compensation(frames, presence_probe_hz)?;

    let mut telemetry_probe =
        DynamicLoudness::new(CHANNELS, SAMPLE_RATE as f64).map_err(|error| error.to_string())?;
    telemetry_probe.set_strength(LISTENING_LOUDNESS_STRENGTH);
    telemetry_probe.set_volume_db(LISTENING_LOUDNESS_LOW_DB);

    Ok(ListeningDynamicLoudnessSection {
        reference_volume_db: LISTENING_LOUDNESS_REFERENCE_DB,
        low_volume_db: LISTENING_LOUDNESS_LOW_DB,
        strength: LISTENING_LOUDNESS_STRENGTH,
        bass_probe_hz,
        presence_probe_hz,
        bass_compensation_db,
        presence_compensation_db,
        reported_loudness_factor: telemetry_probe.loudness_factor(),
    })
}

fn measure_dynamic_loudness_compensation(frames: usize, frequency: f64) -> Result<f64, String> {
    let reference =
        measure_dynamic_loudness_gain(frames, frequency, LISTENING_LOUDNESS_REFERENCE_DB)?;
    let low = measure_dynamic_loudness_gain(frames, frequency, LISTENING_LOUDNESS_LOW_DB)?;
    Ok(low - reference)
}

fn measure_dynamic_loudness_gain(
    frames: usize,
    frequency: f64,
    volume_db: f64,
) -> Result<f64, String> {
    let amplitude = db_to_linear(LISTENING_DSP_AMPLITUDE_DBFS);
    let mut samples = stereo_from_mono(&sine_mono(frames, SAMPLE_RATE, frequency, amplitude));
    let mut dynamic_loudness =
        DynamicLoudness::new(CHANNELS, SAMPLE_RATE as f64).map_err(|error| error.to_string())?;
    dynamic_loudness.set_strength(LISTENING_LOUDNESS_STRENGTH);
    dynamic_loudness.set_volume_db(volume_db);

    for chunk in samples.chunks_mut(4096 * CHANNELS) {
        dynamic_loudness
            .process(chunk, CHANNELS)
            .map_err(|error| error.to_string())?;
    }

    let left = extract_channel(&samples, CHANNELS, 0);
    let skip = output_skip_frames(SAMPLE_RATE);
    let fit = fit_sine(
        &left,
        SAMPLE_RATE,
        frequency,
        skip,
        left.len().saturating_sub(skip * 2),
    )?;
    Ok(db_ratio(fit.amplitude, amplitude))
}

fn measure_noise_shaping(frames: usize) -> Result<NoiseShapingSection, String> {
    let input = biased_sine_mono(
        frames,
        SAMPLE_RATE,
        NOISE_STIMULUS_FREQUENCY_HZ,
        db_to_linear(NOISE_STIMULUS_SINE_DBFS),
        db_to_linear(NOISE_STIMULUS_DC_OFFSET_DBFS),
    );
    let analysis_len = NOISE_SPECTRUM_FFT_LEN.min(input.len());
    if analysis_len < 1024 {
        return Err(format!(
            "not enough samples for noise-shaping FFT: {analysis_len}"
        ));
    }

    let curves = [
        NoiseShaperCurve::TpdfOnly,
        NoiseShaperCurve::Lipshitz5,
        NoiseShaperCurve::FWeighted9,
        NoiseShaperCurve::ModifiedE9,
        NoiseShaperCurve::ImprovedE9,
    ];
    let mut points = Vec::with_capacity(curves.len());

    for curve in curves {
        let mut output = input.clone();
        let mut shaper = NoiseShaper::new(1, SAMPLE_RATE, NOISE_SHAPER_BITS)
            .map_err(|error| error.to_string())?;
        shaper.set_curve(curve);
        shaper
            .process(&mut output, 1)
            .map_err(|error| error.to_string())?;

        let error = output
            .iter()
            .zip(input.iter())
            .map(|(processed, original)| processed - original)
            .collect::<Vec<_>>();
        let start = error.len().saturating_sub(analysis_len);
        let analysis = &error[start..];
        let bands = analyze_noise_spectrum(analysis, SAMPLE_RATE)?;
        let total_noise_rms_dbfs = dbfs(rms_window(analysis, 0, analysis.len())?);
        let high_minus_ear_band_db =
            bands.high_band_14k_to_18k_rms_dbfs - bands.ear_band_2k_to_6k_rms_dbfs;

        points.push(NoiseShapingPoint {
            curve: curve_name(curve),
            total_noise_rms_dbfs,
            ear_band_2k_to_6k_rms_dbfs: bands.ear_band_2k_to_6k_rms_dbfs,
            mid_band_6k_to_10k_rms_dbfs: bands.mid_band_6k_to_10k_rms_dbfs,
            high_band_14k_to_18k_rms_dbfs: bands.high_band_14k_to_18k_rms_dbfs,
            high_minus_ear_band_db,
        });
    }

    let tpdf_high_minus_ear = points
        .iter()
        .find(|point| point.curve == "TpdfOnly")
        .map(|point| point.high_minus_ear_band_db)
        .ok_or_else(|| "missing TpdfOnly noise-shaping point".to_string())?;
    let strongest_shaped_high_minus_ear_band_advantage_db = points
        .iter()
        .filter(|point| point.curve != "TpdfOnly")
        .map(|point| point.high_minus_ear_band_db - tpdf_high_minus_ear)
        .fold(f64::NEG_INFINITY, f64::max);
    let boundaries = measure_noise_shaper_boundaries();

    Ok(NoiseShapingSection {
        sample_rate_hz: SAMPLE_RATE,
        channels: 1,
        bits: NOISE_SHAPER_BITS,
        stimulus_frequency_hz: NOISE_STIMULUS_FREQUENCY_HZ,
        stimulus_sine_dbfs: NOISE_STIMULUS_SINE_DBFS,
        stimulus_dc_offset_dbfs: NOISE_STIMULUS_DC_OFFSET_DBFS,
        fft_len: analysis_len,
        points,
        strongest_shaped_high_minus_ear_band_advantage_db,
        low_level_input_dbfs: NOISE_LOW_LEVEL_INPUT_DBFS,
        low_level_changed_fraction: boundaries.low_level_changed_fraction,
        silence_non_zero_fraction: boundaries.silence_non_zero_fraction,
        stress_max_abs_output: boundaries.stress_max_abs_output,
        stress_non_finite_outputs: boundaries.stress_non_finite_outputs,
    })
}

struct NoiseShaperBoundaryResult {
    low_level_changed_fraction: f64,
    silence_non_zero_fraction: f64,
    stress_max_abs_output: f64,
    stress_non_finite_outputs: usize,
}

fn measure_noise_shaper_boundaries() -> NoiseShaperBoundaryResult {
    const PROBE_SAMPLES: usize = 16_384;

    let low_level = db_to_linear(NOISE_LOW_LEVEL_INPUT_DBFS);
    let mut low_level_shaper = NoiseShaper::new(1, SAMPLE_RATE, NOISE_SHAPER_BITS)
        .expect("fixed noise-shaper benchmark geometry must be valid");
    let low_level_changed = (0..PROBE_SAMPLES)
        .filter(|_| low_level_shaper.process_sample(low_level, 0).to_bits() != low_level.to_bits())
        .count();

    let mut silence_shaper = NoiseShaper::new(1, SAMPLE_RATE, NOISE_SHAPER_BITS)
        .expect("fixed noise-shaper benchmark geometry must be valid");
    silence_shaper.set_curve(NoiseShaperCurve::TpdfOnly);
    let silence_non_zero = (0..PROBE_SAMPLES)
        .filter(|_| silence_shaper.process_sample(0.0, 0) != 0.0)
        .count();

    let curves = [
        NoiseShaperCurve::TpdfOnly,
        NoiseShaperCurve::Lipshitz5,
        NoiseShaperCurve::FWeighted9,
        NoiseShaperCurve::ModifiedE9,
        NoiseShaperCurve::ImprovedE9,
    ];
    let mut stress_max_abs_output = 0.0_f64;
    let mut stress_non_finite_outputs = 0;
    for curve in curves {
        let mut shaper = NoiseShaper::new(1, SAMPLE_RATE, NOISE_SHAPER_BITS)
            .expect("fixed noise-shaper benchmark geometry must be valid");
        shaper.set_curve(curve);
        let mut seed = 0xA076_1D64_78BD_642F_u64;
        for index in 0..PROBE_SAMPLES {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let unit = seed as f64 / u64::MAX as f64;
            let input = match index % 4096 {
                0 => f64::NAN,
                1 => f64::INFINITY,
                2 => f64::NEG_INFINITY,
                3 => 4.0,
                4 => -4.0,
                _ => unit * 2.4 - 1.2,
            };
            let output = shaper.process_sample(input, 0);
            if output.is_finite() {
                stress_max_abs_output = stress_max_abs_output.max(output.abs());
            } else {
                stress_non_finite_outputs += 1;
            }
        }
    }

    NoiseShaperBoundaryResult {
        low_level_changed_fraction: low_level_changed as f64 / PROBE_SAMPLES as f64,
        silence_non_zero_fraction: silence_non_zero as f64 / PROBE_SAMPLES as f64,
        stress_max_abs_output,
        stress_non_finite_outputs,
    }
}

fn analyze_noise_spectrum(samples: &[f64], sample_rate: u32) -> Result<NoiseSpectrumBands, String> {
    let fft_len = samples.len();
    if fft_len < 1024 {
        return Err(format!("noise spectrum FFT too short: {fft_len}"));
    }

    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(fft_len);
    let mut window_power_sum = 0.0;
    let mut spectrum = Vec::with_capacity(fft_len);
    for (index, sample) in samples.iter().enumerate() {
        let window = hann_window(index, fft_len);
        window_power_sum += window * window;
        spectrum.push(Complex::new(sample * window, 0.0));
    }
    fft.process(&mut spectrum);

    let window_power_mean = window_power_sum / fft_len as f64;
    Ok(NoiseSpectrumBands {
        ear_band_2k_to_6k_rms_dbfs: fft_band_rms_dbfs(
            &spectrum,
            sample_rate,
            2_000.0,
            6_000.0,
            window_power_mean,
        )?,
        mid_band_6k_to_10k_rms_dbfs: fft_band_rms_dbfs(
            &spectrum,
            sample_rate,
            6_000.0,
            10_000.0,
            window_power_mean,
        )?,
        high_band_14k_to_18k_rms_dbfs: fft_band_rms_dbfs(
            &spectrum,
            sample_rate,
            14_000.0,
            18_000.0,
            window_power_mean,
        )?,
    })
}

#[allow(clippy::needless_range_loop)]
fn fft_band_rms_dbfs(
    spectrum: &[Complex<f64>],
    sample_rate: u32,
    low_hz: f64,
    high_hz: f64,
    window_power_mean: f64,
) -> Result<f64, String> {
    let fft_len = spectrum.len();
    let nyquist_bin = fft_len / 2;
    let bin_hz = sample_rate as f64 / fft_len as f64;
    let start_bin = (low_hz / bin_hz).ceil().max(0.0) as usize;
    let end_bin = ((high_hz / bin_hz).floor() as usize).min(nyquist_bin);
    if start_bin > end_bin {
        return Err(format!(
            "empty FFT band {low_hz:.1}-{high_hz:.1} Hz for len={fft_len}"
        ));
    }

    let mut power = 0.0;
    for bin in start_bin..=end_bin {
        let mut bin_power = spectrum[bin].norm_sqr();
        if bin != 0 && bin != nyquist_bin {
            bin_power *= 2.0;
        }
        power += bin_power;
    }

    let denom = fft_len as f64 * fft_len as f64 * window_power_mean;
    Ok(dbfs((power / denom).sqrt()))
}

fn measure_loudness_reference(ebu_dir: &Path) -> Result<LoudnessReferenceSection, String> {
    let mut fixtures = Vec::new();

    let sine = stereo_from_mono(&sine_mono(
        frames_for_duration(LOUDNESS_SINE_DURATION_SECS),
        SAMPLE_RATE,
        1_000.0,
        db_to_linear(-23.0),
    ));
    fixtures.push(measure_loudness_fixture(
        "sine_1khz_minus_23_dbfs_10s",
        LOUDNESS_SINE_DURATION_SECS,
        &sine,
    )?);

    let stepped = loudness_stepped_fixture(LOUDNESS_STEPPED_DURATION_SECS);
    fixtures.push(measure_loudness_fixture(
        "stepped_sine_minus_30_to_minus_12_dbfs_12s",
        LOUDNESS_STEPPED_DURATION_SECS,
        &stepped,
    )?);

    let ebu_corpus = measure_ebu_loudness_corpus(ebu_dir)?;

    let max_integrated_delta_lu = fixtures
        .iter()
        .map(|fixture| fixture.integrated_delta_lu)
        .fold(0.0, f64::max);
    let max_momentary_delta_lu = fixtures
        .iter()
        .map(|fixture| fixture.momentary_delta_lu)
        .fold(0.0, f64::max);
    let max_short_term_delta_lu = fixtures
        .iter()
        .map(|fixture| fixture.short_term_delta_lu)
        .fold(0.0, f64::max);
    let max_loudness_range_delta_lu = fixtures
        .iter()
        .map(|fixture| fixture.loudness_range_delta_lu)
        .fold(0.0, f64::max);
    let max_true_peak_delta_db = fixtures
        .iter()
        .map(|fixture| fixture.true_peak_delta_db)
        .fold(0.0, f64::max);

    Ok(LoudnessReferenceSection {
        sample_rate_hz: SAMPLE_RATE,
        channels: CHANNELS,
        fixtures,
        ebu_corpus,
        max_integrated_delta_lu,
        max_momentary_delta_lu,
        max_short_term_delta_lu,
        max_loudness_range_delta_lu,
        max_true_peak_delta_db,
    })
}

fn measure_ebu_loudness_corpus(ebu_dir: &Path) -> Result<EbuLoudnessCorpusSection, String> {
    let mut missing_files = Vec::new();
    collect_missing_expected_files(&EBU_GLOBAL_LOUDNESS_FILES, ebu_dir, &mut missing_files);
    collect_missing_expected_files(&EBU_LRA_FILES, ebu_dir, &mut missing_files);
    collect_missing_expected_files(&EBU_MAX_MOMENTARY_FILES, ebu_dir, &mut missing_files);
    collect_missing_expected_files(&EBU_MAX_SHORT_TERM_FILES, ebu_dir, &mut missing_files);
    missing_files.sort_unstable();
    missing_files.dedup();

    if !missing_files.is_empty() {
        return Ok(EbuLoudnessCorpusSection {
            available: false,
            source_dir: ebu_dir.display().to_string(),
            source_note: "EBU Tech 3341/3342 files from libebur128 test corpus; unzip ebu-loudness-test-setv05.zip into source_dir to enable",
            missing_files,
            global_loudness_points: Vec::new(),
            loudness_range_points: Vec::new(),
            max_momentary_points: Vec::new(),
            max_short_term_points: Vec::new(),
            max_abs_global_error_lu: 0.0,
            max_abs_loudness_range_error_lu: 0.0,
            max_abs_max_momentary_error_lu: 0.0,
            max_abs_max_short_term_error_lu: 0.0,
        });
    }

    let global_loudness_points = measure_ebu_expected_files(
        ebu_dir,
        &EBU_GLOBAL_LOUDNESS_FILES,
        EbuLoudnessMetric::Global,
    )?;
    let loudness_range_points =
        measure_ebu_expected_files(ebu_dir, &EBU_LRA_FILES, EbuLoudnessMetric::Range)?;
    let max_momentary_points = measure_ebu_expected_files(
        ebu_dir,
        &EBU_MAX_MOMENTARY_FILES,
        EbuLoudnessMetric::MaxMomentary,
    )?;
    let max_short_term_points = measure_ebu_expected_files(
        ebu_dir,
        &EBU_MAX_SHORT_TERM_FILES,
        EbuLoudnessMetric::MaxShortTerm,
    )?;

    Ok(EbuLoudnessCorpusSection {
        available: true,
        source_dir: ebu_dir.display().to_string(),
        source_note: "EBU Tech 3341/3342 files from libebur128 test corpus",
        max_abs_global_error_lu: max_abs_error(&global_loudness_points),
        max_abs_loudness_range_error_lu: max_abs_error(&loudness_range_points),
        max_abs_max_momentary_error_lu: max_abs_error(&max_momentary_points),
        max_abs_max_short_term_error_lu: max_abs_error(&max_short_term_points),
        missing_files,
        global_loudness_points,
        loudness_range_points,
        max_momentary_points,
        max_short_term_points,
    })
}

#[derive(Clone, Copy)]
enum EbuLoudnessMetric {
    Global,
    Range,
    MaxMomentary,
    MaxShortTerm,
}

fn measure_ebu_expected_files(
    ebu_dir: &Path,
    files: &[EbuExpectedFile],
    metric: EbuLoudnessMetric,
) -> Result<Vec<EbuCorpusPoint>, String> {
    let mut points = Vec::with_capacity(files.len());
    for expected_file in files {
        let wav = read_pcm_wav(&ebu_dir.join(expected_file.file_name))?;
        let measured = match metric {
            EbuLoudnessMetric::Global => {
                measure_ebu_global_loudness(&wav.samples, wav.channels, wav.sample_rate)?
            }
            EbuLoudnessMetric::Range => {
                measure_ebu_loudness_range(&wav.samples, wav.channels, wav.sample_rate)?
            }
            EbuLoudnessMetric::MaxMomentary => {
                measure_ebu_max_momentary(&wav.samples, wav.channels, wav.sample_rate)?
            }
            EbuLoudnessMetric::MaxShortTerm => {
                measure_ebu_max_short_term(&wav.samples, wav.channels, wav.sample_rate)?
            }
        };
        let error = measured - expected_file.expected;
        let tolerance = match metric {
            EbuLoudnessMetric::Range => EBU_LRA_TOLERANCE_LU,
            _ => EBU_LOUDNESS_TOLERANCE_LU,
        };
        points.push(EbuCorpusPoint {
            file_name: expected_file.file_name,
            sample_rate_hz: wav.sample_rate,
            channels: wav.channels,
            frames: wav.samples.len() / wav.channels,
            expected: expected_file.expected,
            measured,
            error,
            passed: error.abs() <= tolerance,
        });
    }
    Ok(points)
}

fn measure_ebu_global_loudness(
    samples: &[f64],
    channels: usize,
    sample_rate: u32,
) -> Result<f64, String> {
    let mut meter = new_ebu_meter(channels, sample_rate, ebur128::Mode::I)?;
    for chunk in samples.chunks(channels * sample_rate as usize) {
        meter
            .add_frames_f64(chunk)
            .map_err(|err| format!("failed to add EBU global frames: {err:?}"))?;
    }
    meter
        .loudness_global()
        .map_err(|err| format!("failed to read EBU global loudness: {err:?}"))
}

fn measure_ebu_loudness_range(
    samples: &[f64],
    channels: usize,
    sample_rate: u32,
) -> Result<f64, String> {
    let mut meter = new_ebu_meter(channels, sample_rate, ebur128::Mode::LRA)?;
    for chunk in samples.chunks(channels * sample_rate as usize) {
        meter
            .add_frames_f64(chunk)
            .map_err(|err| format!("failed to add EBU LRA frames: {err:?}"))?;
    }
    meter
        .loudness_range()
        .map_err(|err| format!("failed to read EBU loudness range: {err:?}"))
}

fn measure_ebu_max_momentary(
    samples: &[f64],
    channels: usize,
    sample_rate: u32,
) -> Result<f64, String> {
    let mut meter = new_ebu_meter(channels, sample_rate, ebur128::Mode::M)?;
    let frames_per_chunk = (sample_rate as usize / 100).max(1);
    let valid_after_frames = (4 * sample_rate as usize) / 10;
    let mut frames_read = 0usize;
    let mut max_momentary = f64::NEG_INFINITY;

    for chunk in samples.chunks(channels * frames_per_chunk) {
        let frames = chunk.len() / channels;
        if frames == 0 {
            continue;
        }
        meter
            .add_frames_f64(chunk)
            .map_err(|err| format!("failed to add EBU momentary frames: {err:?}"))?;
        frames_read += frames;
        if frames_read >= valid_after_frames {
            let value = meter
                .loudness_momentary()
                .map_err(|err| format!("failed to read EBU momentary loudness: {err:?}"))?;
            if value.is_finite() {
                max_momentary = max_momentary.max(value);
            }
        }
    }

    Ok(max_momentary)
}

fn measure_ebu_max_short_term(
    samples: &[f64],
    channels: usize,
    sample_rate: u32,
) -> Result<f64, String> {
    let mut meter = new_ebu_meter(channels, sample_rate, ebur128::Mode::S)?;
    let frames_per_chunk = (sample_rate as usize / 10).max(1);
    let valid_after_frames = 3 * sample_rate as usize;
    let mut frames_read = 0usize;
    let mut max_short_term = f64::NEG_INFINITY;

    for chunk in samples.chunks(channels * frames_per_chunk) {
        let frames = chunk.len() / channels;
        if frames == 0 {
            continue;
        }
        meter
            .add_frames_f64(chunk)
            .map_err(|err| format!("failed to add EBU short-term frames: {err:?}"))?;
        frames_read += frames;
        if frames_read >= valid_after_frames {
            let value = meter
                .loudness_shortterm()
                .map_err(|err| format!("failed to read EBU short-term loudness: {err:?}"))?;
            if value.is_finite() {
                max_short_term = max_short_term.max(value);
            }
        }
    }

    Ok(max_short_term)
}

fn measure_loudness_fixture(
    name: &'static str,
    duration_secs: f64,
    samples: &[f64],
) -> Result<LoudnessFixtureResult, String> {
    let engine = measure_engine_loudness(samples);
    let reference = measure_reference_loudness(samples)?;

    Ok(LoudnessFixtureResult {
        name,
        duration_secs,
        engine_integrated_lufs: engine.integrated_lufs,
        reference_integrated_lufs: reference.integrated_lufs,
        integrated_delta_lu: abs_delta(engine.integrated_lufs, reference.integrated_lufs),
        engine_momentary_lufs: engine.momentary_lufs,
        reference_momentary_lufs: reference.momentary_lufs,
        momentary_delta_lu: abs_delta(engine.momentary_lufs, reference.momentary_lufs),
        engine_short_term_lufs: engine.short_term_lufs,
        reference_short_term_lufs: reference.short_term_lufs,
        short_term_delta_lu: abs_delta(engine.short_term_lufs, reference.short_term_lufs),
        engine_loudness_range_lu: engine.loudness_range_lu,
        reference_loudness_range_lu: reference.loudness_range_lu,
        loudness_range_delta_lu: abs_delta(engine.loudness_range_lu, reference.loudness_range_lu),
        engine_true_peak_dbtp: engine.true_peak_dbtp,
        reference_true_peak_dbtp: reference.true_peak_dbtp,
        true_peak_delta_db: abs_delta(engine.true_peak_dbtp, reference.true_peak_dbtp),
    })
}

fn measure_engine_loudness(samples: &[f64]) -> LoudnessValues {
    let mut meter = LoudnessMeter::new(CHANNELS, SAMPLE_RATE)
        .expect("quality fixture uses valid loudness meter geometry");
    for chunk in samples.chunks(CHANNELS * 1024) {
        meter
            .process(chunk)
            .expect("quality fixture contains complete interleaved frames");
    }
    LoudnessValues {
        integrated_lufs: meter.integrated_loudness(),
        momentary_lufs: meter.momentary_loudness(),
        short_term_lufs: meter.short_term_loudness(),
        loudness_range_lu: meter.loudness_range(),
        true_peak_dbtp: meter.true_peak(),
    }
}

fn measure_reference_loudness(samples: &[f64]) -> Result<LoudnessValues, String> {
    let mut meter = ebur128::EbuR128::new(CHANNELS as u32, SAMPLE_RATE, ebur128::Mode::all())
        .map_err(|err| format!("failed to create ebur128 reference meter: {err:?}"))?;
    meter
        .add_frames_f64(samples)
        .map_err(|err| format!("failed to add frames to ebur128 reference meter: {err:?}"))?;

    let mut true_peak = 0.0;
    for channel in 0..CHANNELS {
        let channel_peak = meter.true_peak(channel as u32).map_err(|err| {
            format!("failed to read ebur128 true peak for channel {channel}: {err:?}")
        })?;
        if channel_peak > true_peak {
            true_peak = channel_peak;
        }
    }

    Ok(LoudnessValues {
        integrated_lufs: meter
            .loudness_global()
            .map_err(|err| format!("failed to read ebur128 integrated loudness: {err:?}"))?,
        momentary_lufs: meter
            .loudness_momentary()
            .map_err(|err| format!("failed to read ebur128 momentary loudness: {err:?}"))?,
        short_term_lufs: meter
            .loudness_shortterm()
            .map_err(|err| format!("failed to read ebur128 short-term loudness: {err:?}"))?,
        loudness_range_lu: meter
            .loudness_range()
            .map_err(|err| format!("failed to read ebur128 loudness range: {err:?}"))?,
        true_peak_dbtp: dbfs(true_peak),
    })
}

fn measure_full_output_true_peak(
    frames: usize,
    ebu_dir: &Path,
    render_policy: OfflineRenderPolicy,
) -> Result<FullOutputTruePeakSection, String> {
    let mut points = Vec::new();
    let synthetic = synthetic_intersample_stress(frames, RESAMPLE_FROM);
    points.push(measure_full_output_true_peak_point(
        "synthetic_44k1_near_nyquist_resampled".to_string(),
        "synthetic",
        &synthetic,
        RESAMPLE_FROM,
        CHANNELS,
        render_policy,
    )?);

    let ebu_corpus = measure_ebu_true_peak_corpus(ebu_dir, render_policy)?;
    for point in &ebu_corpus.points {
        points.push(FullOutputTruePeakPoint {
            name: format!("ebu_{}", point.file_name),
            source_kind: "EBU Tech 3341",
            source_sample_rate_hz: point.sample_rate_hz,
            source_channels: point.channels,
            source_frames: point.frames,
            input_sample_peak_dbfs: f64::NAN,
            input_true_peak_dbtp: point.measured_input_true_peak_dbtp,
            output_sample_peak_dbfs: f64::NAN,
            output_true_peak_dbtp: point.full_output_true_peak_dbtp,
            output_margin_to_limiter_threshold_db: point.full_output_margin_to_limiter_threshold_db,
            final_limiter_gain_reduction_db: f64::NAN,
            output_frames: point.output_frames,
            rendered_frames: point.rendered_frames,
            algorithmic_latency_frames: point.algorithmic_latency_frames,
            semantic_tail_frames: point.semantic_tail_frames,
            tail_truncated: point.tail_truncated,
        });
    }

    let worst_output_true_peak_dbtp = points
        .iter()
        .map(|point| point.output_true_peak_dbtp)
        .filter(|value| value.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    let worst_margin_to_limiter_threshold_db = points
        .iter()
        .map(|point| point.output_margin_to_limiter_threshold_db)
        .filter(|value| value.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);

    Ok(FullOutputTruePeakSection {
        output_sample_rate_hz: RESAMPLE_TO,
        chain: offline_render_stage_order_csv(),
        post_render_analysis: post_render_analysis_order_csv(),
        limiter_threshold_dbfs: FULL_OUTPUT_TRUE_PEAK_LIMIT_DBTP,
        final_noise_shaper_bits: FULL_OUTPUT_CHAIN_BITS,
        points,
        ebu_true_peak_corpus: ebu_corpus,
        worst_output_true_peak_dbtp,
        worst_margin_to_limiter_threshold_db,
    })
}

fn measure_ebu_true_peak_corpus(
    ebu_dir: &Path,
    render_policy: OfflineRenderPolicy,
) -> Result<EbuTruePeakCorpusSection, String> {
    let mut missing_files = Vec::new();
    collect_missing_expected_files(&EBU_TRUE_PEAK_FILES, ebu_dir, &mut missing_files);
    missing_files.sort_unstable();
    missing_files.dedup();

    if !missing_files.is_empty() {
        return Ok(EbuTruePeakCorpusSection {
            available: false,
            source_dir: ebu_dir.display().to_string(),
            missing_files,
            points: Vec::new(),
            max_abs_expected_error_db: 0.0,
        });
    }

    let mut points = Vec::with_capacity(EBU_TRUE_PEAK_FILES.len());
    for expected_file in EBU_TRUE_PEAK_FILES {
        let wav = read_pcm_wav(&ebu_dir.join(expected_file.file_name))?;
        let measured_input_true_peak_dbtp =
            measure_true_peak_db(&wav.samples, wav.channels, wav.sample_rate)?;
        let rendered =
            render_full_output_chain(&wav.samples, wav.sample_rate, wav.channels, render_policy)?;
        let full_output_true_peak_dbtp =
            measure_true_peak_db(&rendered.samples, wav.channels, RESAMPLE_TO)?;
        let output_frames = rendered.samples.len() / wav.channels;
        let input_error_db = measured_input_true_peak_dbtp - expected_file.expected;
        points.push(EbuTruePeakPoint {
            file_name: expected_file.file_name,
            sample_rate_hz: wav.sample_rate,
            channels: wav.channels,
            frames: wav.samples.len() / wav.channels,
            expected_dbtp: expected_file.expected,
            measured_input_true_peak_dbtp,
            input_error_db,
            full_output_true_peak_dbtp,
            full_output_margin_to_limiter_threshold_db: full_output_true_peak_dbtp
                - FULL_OUTPUT_TRUE_PEAK_LIMIT_DBTP,
            output_frames,
            rendered_frames: rendered.rendered_frames,
            algorithmic_latency_frames: rendered.algorithmic_latency_frames,
            semantic_tail_frames: rendered.semantic_tail_frames,
            tail_truncated: rendered.tail_truncated,
            passed_reference_tolerance: (EBU_TRUE_PEAK_LOWER_TOLERANCE_DB
                ..=EBU_TRUE_PEAK_UPPER_TOLERANCE_DB)
                .contains(&input_error_db),
        });
    }

    Ok(EbuTruePeakCorpusSection {
        available: true,
        source_dir: ebu_dir.display().to_string(),
        max_abs_expected_error_db: points
            .iter()
            .map(|point| point.input_error_db.abs())
            .fold(0.0, f64::max),
        missing_files,
        points,
    })
}

fn measure_full_output_true_peak_point(
    name: String,
    source_kind: &'static str,
    samples: &[f64],
    source_sample_rate: u32,
    channels: usize,
    render_policy: OfflineRenderPolicy,
) -> Result<FullOutputTruePeakPoint, String> {
    let rendered = render_full_output_chain(samples, source_sample_rate, channels, render_policy)?;
    let output_true_peak_dbtp = measure_true_peak_db(&rendered.samples, channels, RESAMPLE_TO)?;
    let output_frames = rendered.samples.len() / channels;

    Ok(FullOutputTruePeakPoint {
        name,
        source_kind,
        source_sample_rate_hz: source_sample_rate,
        source_channels: channels,
        source_frames: samples.len() / channels,
        input_sample_peak_dbfs: dbfs(max_abs(samples)),
        input_true_peak_dbtp: measure_true_peak_db(samples, channels, source_sample_rate)?,
        output_sample_peak_dbfs: dbfs(max_abs(&rendered.samples)),
        output_true_peak_dbtp,
        output_margin_to_limiter_threshold_db: output_true_peak_dbtp
            - FULL_OUTPUT_TRUE_PEAK_LIMIT_DBTP,
        final_limiter_gain_reduction_db: rendered.final_limiter_gain_reduction_db,
        output_frames,
        rendered_frames: rendered.rendered_frames,
        algorithmic_latency_frames: rendered.algorithmic_latency_frames,
        semantic_tail_frames: rendered.semantic_tail_frames,
        tail_truncated: rendered.tail_truncated,
    })
}

fn render_full_output_chain(
    samples: &[f64],
    source_sample_rate: u32,
    channels: usize,
    render_policy: OfflineRenderPolicy,
) -> Result<RenderedOutput, String> {
    let eq_params = Arc::new(AtomicEqParams::new());
    let saturation_params = Arc::new(AtomicSaturationParams::new());
    let crossfeed_params = Arc::new(AtomicCrossfeedParams::new());
    let limiter_params = Arc::new(AtomicPeakLimiterParams::new());
    let volume_params = Arc::new(AtomicVolumeParams::new());
    let noise_shaper_params = Arc::new(AtomicNoiseShaperParams::new());
    let dynamic_loudness_params = Arc::new(AtomicDynamicLoudnessParams::new());

    eq_params.write(&[0.0; EQ_BANDS], false);
    saturation_params.set_enabled(false);
    saturation_params.set_armed(false);
    crossfeed_params.set_enabled(false);
    limiter_params.set_threshold(FULL_OUTPUT_TRUE_PEAK_LIMIT_DBTP);
    limiter_params.set_release(LIMITER_RELEASE_MS);
    limiter_params.set_enabled(true);
    volume_params.set_volume(1.0);
    volume_params.set_muted(false);
    dynamic_loudness_params.set_enabled(false);
    noise_shaper_params.set_enabled(true);
    noise_shaper_params.set_bits(FULL_OUTPUT_CHAIN_BITS);
    noise_shaper_params.set_curve(NoiseShaperCurve::auto_select(RESAMPLE_TO));

    let mut chain = OutputChainBuilder::new(OutputChainParams {
        channels,
        output_sample_rate: RESAMPLE_TO,
        eq_params,
        saturation_params,
        crossfeed_params,
        convolver_control: ConvolverControl::default(),
        volume_params,
        dynamic_loudness_params,
        dynamic_loudness_telemetry: Arc::new(AtomicDynamicLoudnessTelemetry::new()),
        limiter_params,
        noise_shaper_params,
    })
    .build_render_chain_with_policy(source_sample_rate, render_policy)
    .map_err(|error| error.to_string())?;

    chain
        .render_with_policy(samples, render_policy)
        .map_err(|err| err.to_string())
}

fn resample_mono(input: &[f64], from_rate: u32, to_rate: u32) -> Result<Vec<f64>, String> {
    let mut resampler = StreamingResampler::with_quality(
        1,
        from_rate,
        to_rate,
        PhaseResponse::Linear,
        ResampleQuality::UltraHigh,
    )
    .map_err(|err| format!("failed to create resampler {from_rate}->{to_rate}: {err}"))?;

    let estimated_len = ((input.len() as f64 * to_rate as f64 / from_rate as f64).ceil() as usize)
        .saturating_add(256);
    let mut output = Vec::with_capacity(estimated_len);
    let mut scratch = vec![0.0; 4096];
    for chunk in input.chunks(4096) {
        let mut consumed = 0;
        while consumed < chunk.len() {
            let input_block =
                AudioBlockRef::new(&chunk[consumed..], 1).map_err(|error| error.to_string())?;
            let output_block =
                AudioBlockMut::new(&mut scratch, 1).map_err(|error| error.to_string())?;
            let buffers = ProcessBuffers::out_of_place(input_block, output_block)
                .map_err(|error| error.to_string())?;
            let progress =
                process_checked(&mut resampler, buffers).map_err(|error| error.to_string())?;
            consumed += progress.consumed_frames();
            output.extend_from_slice(&scratch[..progress.produced_frames()]);
        }
    }
    loop {
        let output_block =
            AudioBlockMut::new(&mut scratch, 1).map_err(|error| error.to_string())?;
        let progress =
            finish_checked(&mut resampler, output_block).map_err(|error| error.to_string())?;
        output.extend_from_slice(&scratch[..progress.produced_frames()]);
        if progress.state() == ProcessState::Finished {
            break;
        }
    }
    Ok(output)
}

fn synthetic_intersample_stress(frames: usize, sample_rate: u32) -> Vec<f64> {
    let amplitude = db_to_linear(-1.05);
    let left = sine_mono(frames, sample_rate, 18_700.0, amplitude);
    let right = sine_mono(frames, sample_rate, 19_100.0, amplitude);
    let mut stereo = Vec::with_capacity(frames * CHANNELS);
    for frame in 0..frames {
        stereo.push(left[frame]);
        stereo.push(right[frame]);
    }
    stereo
}

fn measure_true_peak_db(samples: &[f64], channels: usize, sample_rate: u32) -> Result<f64, String> {
    let mut meter = LoudnessMeter::new(channels, sample_rate).map_err(|error| error.to_string())?;
    for chunk in samples.chunks(channels * 4096) {
        meter.process(chunk).map_err(|error| error.to_string())?;
    }
    let true_peak = meter.true_peak();
    if true_peak.is_finite() {
        Ok(true_peak)
    } else {
        Err("true-peak measurement returned a non-finite value".to_string())
    }
}

fn new_ebu_meter(
    channels: usize,
    sample_rate: u32,
    mode: ebur128::Mode,
) -> Result<ebur128::EbuR128, String> {
    let mut meter = ebur128::EbuR128::new(channels as u32, sample_rate, mode)
        .map_err(|err| format!("failed to create ebur128 meter: {err:?}"))?;
    configure_ebu_channel_map(&mut meter, channels)?;
    Ok(meter)
}

fn configure_ebu_channel_map(meter: &mut ebur128::EbuR128, channels: usize) -> Result<(), String> {
    if channels == 5 {
        let map = [
            Channel::Left,
            Channel::Right,
            Channel::Center,
            Channel::LeftSurround,
            Channel::RightSurround,
        ];
        meter
            .set_channel_map(&map)
            .map_err(|err| format!("failed to set 5-channel EBU channel map: {err:?}"))?;
    }
    Ok(())
}

fn collect_missing_expected_files(
    files: &[EbuExpectedFile],
    dir: &Path,
    missing: &mut Vec<&'static str>,
) {
    for file in files {
        if !dir.join(file.file_name).is_file() {
            missing.push(file.file_name);
        }
    }
}

fn max_abs_error(points: &[EbuCorpusPoint]) -> f64 {
    points
        .iter()
        .map(|point| point.error.abs())
        .fold(0.0, f64::max)
}

fn read_pcm_wav(path: &Path) -> Result<WavData, String> {
    let bytes =
        fs::read(path).map_err(|err| format!("failed to read WAV '{}': {err}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("'{}' is not a RIFF/WAVE file", path.display()));
    }

    let mut cursor = 12usize;
    let mut format: Option<WavFormat> = None;
    let mut data_range: Option<(usize, usize)> = None;

    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_len = read_u32_le(&bytes, cursor + 4)? as usize;
        cursor += 8;
        if cursor + chunk_len > bytes.len() {
            return Err(format!(
                "WAV chunk in '{}' extends past end of file",
                path.display()
            ));
        }

        match chunk_id {
            b"fmt " => format = Some(read_wav_format(&bytes[cursor..cursor + chunk_len], path)?),
            b"data" => data_range = Some((cursor, chunk_len)),
            _ => {}
        }

        cursor += chunk_len + (chunk_len & 1);
    }

    let format = format.ok_or_else(|| format!("WAV '{}' is missing fmt chunk", path.display()))?;
    let (data_start, data_len) =
        data_range.ok_or_else(|| format!("WAV '{}' is missing data chunk", path.display()))?;
    decode_pcm_samples(&bytes[data_start..data_start + data_len], format, path)
}

fn read_wav_format(chunk: &[u8], path: &Path) -> Result<WavFormat, String> {
    if chunk.len() < 16 {
        return Err(format!("WAV '{}' has a short fmt chunk", path.display()));
    }

    let audio_format = read_u16_le(chunk, 0)?;
    let channels = read_u16_le(chunk, 2)? as usize;
    let sample_rate = read_u32_le(chunk, 4)?;
    let block_align = read_u16_le(chunk, 12)? as usize;
    let bits_per_sample = read_u16_le(chunk, 14)? as usize;

    if audio_format != 1 && audio_format != 0xFFFE {
        return Err(format!(
            "WAV '{}' uses unsupported format {}; expected PCM or WAVE_FORMAT_EXTENSIBLE PCM",
            path.display(),
            audio_format
        ));
    }
    if audio_format == 0xFFFE && !is_wave_extensible_pcm(chunk) {
        return Err(format!(
            "WAV '{}' uses unsupported WAVE_FORMAT_EXTENSIBLE subformat",
            path.display()
        ));
    }
    if channels == 0 {
        return Err(format!("WAV '{}' has zero channels", path.display()));
    }
    if !matches!(bits_per_sample, 16 | 24 | 32) {
        return Err(format!(
            "WAV '{}' uses unsupported PCM depth {}",
            path.display(),
            bits_per_sample
        ));
    }

    Ok(WavFormat {
        audio_format,
        sample_rate,
        channels,
        bits_per_sample,
        block_align,
    })
}

fn is_wave_extensible_pcm(chunk: &[u8]) -> bool {
    if chunk.len() < 40 {
        return false;
    }
    let subformat = &chunk[24..40];
    subformat
        == [
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38,
            0x9B, 0x71,
        ]
}

fn decode_pcm_samples(data: &[u8], format: WavFormat, path: &Path) -> Result<WavData, String> {
    let bytes_per_sample = format.bits_per_sample / 8;
    let expected_block_align = bytes_per_sample * format.channels;
    if format.audio_format != 1 && format.audio_format != 0xFFFE {
        return Err(format!(
            "WAV '{}' has unsupported audio format {}",
            path.display(),
            format.audio_format
        ));
    }
    if format.block_align != expected_block_align {
        return Err(format!(
            "WAV '{}' has block_align {}, expected {}",
            path.display(),
            format.block_align,
            expected_block_align
        ));
    }
    if !data.len().is_multiple_of(format.block_align) {
        return Err(format!(
            "WAV '{}' data length is not frame-aligned",
            path.display()
        ));
    }

    let mut samples = Vec::with_capacity(data.len() / bytes_per_sample);
    for sample_bytes in data.chunks_exact(bytes_per_sample) {
        samples.push(match format.bits_per_sample {
            16 => i16::from_le_bytes([sample_bytes[0], sample_bytes[1]]) as f64 / 32768.0,
            24 => {
                let unsigned =
                    u32::from_le_bytes([sample_bytes[0], sample_bytes[1], sample_bytes[2], 0]);
                let signed = ((unsigned << 8) as i32) >> 8;
                signed as f64 / 8_388_608.0
            }
            32 => {
                i32::from_le_bytes([
                    sample_bytes[0],
                    sample_bytes[1],
                    sample_bytes[2],
                    sample_bytes[3],
                ]) as f64
                    / 2_147_483_648.0
            }
            _ => unreachable!(),
        });
    }

    Ok(WavData {
        sample_rate: format.sample_rate,
        channels: format.channels,
        samples,
    })
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let Some(data) = bytes.get(offset..offset + 2) else {
        return Err("unexpected end of little-endian u16".to_string());
    };
    Ok(u16::from_le_bytes([data[0], data[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let Some(data) = bytes.get(offset..offset + 4) else {
        return Err("unexpected end of little-endian u32".to_string());
    };
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

fn fit_sine(
    samples: &[f64],
    sample_rate: u32,
    frequency: f64,
    skip: usize,
    take: usize,
) -> Result<SineFit, String> {
    if samples.is_empty() {
        return Err("cannot fit sine on an empty sample buffer".to_string());
    }
    let start = skip.min(samples.len());
    let available = samples.len().saturating_sub(start);
    let count = take.min(available);
    if count < 32 {
        return Err(format!(
            "not enough samples for sine fit: count={count}, skip={skip}, len={}",
            samples.len()
        ));
    }

    let omega = 2.0 * PI * frequency / sample_rate as f64;
    let mut matrix = [[0.0; 3]; 3];
    let mut rhs = [0.0; 3];

    for local in 0..count {
        let n = (start + local) as f64;
        let basis = [(omega * n).sin(), (omega * n).cos(), 1.0];
        let sample = samples[start + local];
        for row in 0..3 {
            rhs[row] += basis[row] * sample;
            for col in 0..3 {
                matrix[row][col] += basis[row] * basis[col];
            }
        }
    }

    let coeffs = solve_3x3(matrix, rhs)?;
    let mut residual_sum = 0.0;
    for local in 0..count {
        let n = (start + local) as f64;
        let fitted = coeffs[0] * (omega * n).sin() + coeffs[1] * (omega * n).cos() + coeffs[2];
        let error = samples[start + local] - fitted;
        residual_sum += error * error;
    }

    let amplitude = (coeffs[0] * coeffs[0] + coeffs[1] * coeffs[1]).sqrt();
    let signal_rms = amplitude / 2.0_f64.sqrt();
    let residual_rms = (residual_sum / count as f64).sqrt();
    let thdn_db = db_ratio(residual_rms, signal_rms);

    Ok(SineFit { amplitude, thdn_db })
}

#[allow(clippy::needless_range_loop)]
fn solve_3x3(mut matrix: [[f64; 3]; 3], mut rhs: [f64; 3]) -> Result<[f64; 3], String> {
    for pivot in 0..3 {
        let mut best_row = pivot;
        let mut best_abs = matrix[pivot][pivot].abs();
        for row in (pivot + 1)..3 {
            let candidate = matrix[row][pivot].abs();
            if candidate > best_abs {
                best_abs = candidate;
                best_row = row;
            }
        }
        if best_abs < 1.0e-24 {
            return Err("singular sine-fit matrix".to_string());
        }
        if best_row != pivot {
            matrix.swap(pivot, best_row);
            rhs.swap(pivot, best_row);
        }
        let pivot_value = matrix[pivot][pivot];
        for col in pivot..3 {
            matrix[pivot][col] /= pivot_value;
        }
        rhs[pivot] /= pivot_value;

        for row in 0..3 {
            if row == pivot {
                continue;
            }
            let factor = matrix[row][pivot];
            for col in pivot..3 {
                matrix[row][col] -= factor * matrix[pivot][col];
            }
            rhs[row] -= factor * rhs[pivot];
        }
    }
    Ok(rhs)
}

fn sine_mono(frames: usize, sample_rate: u32, frequency: f64, amplitude: f64) -> Vec<f64> {
    let omega = 2.0 * PI * frequency / sample_rate as f64;
    (0..frames)
        .map(|frame| amplitude * (omega * frame as f64).sin())
        .collect()
}

fn biased_sine_mono(
    frames: usize,
    sample_rate: u32,
    frequency: f64,
    amplitude: f64,
    dc_offset: f64,
) -> Vec<f64> {
    let omega = 2.0 * PI * frequency / sample_rate as f64;
    (0..frames)
        .map(|frame| dc_offset + amplitude * (omega * frame as f64).sin())
        .collect()
}

fn loudness_stepped_fixture(duration_secs: f64) -> Vec<f64> {
    let frames = frames_for_duration(duration_secs);
    let segment_frames = frames / 3;
    let mut mono = Vec::with_capacity(frames);
    for frame in 0..frames {
        let (frequency, amplitude_dbfs) = if frame < segment_frames {
            (440.0, -30.0)
        } else if frame < segment_frames * 2 {
            (997.0, -18.0)
        } else {
            (1_760.0, -12.0)
        };
        let omega = 2.0 * PI * frequency / SAMPLE_RATE as f64;
        mono.push(db_to_linear(amplitude_dbfs) * (omega * frame as f64).sin());
    }
    stereo_from_mono(&mono)
}

fn limiter_stress_signal(
    frames: usize,
    sample_rate: u32,
    frequency: f64,
    sine_amplitude: f64,
) -> Vec<f64> {
    let omega = 2.0 * PI * frequency / sample_rate as f64;
    let mut samples = Vec::with_capacity(frames);
    for frame in 0..frames {
        let mut sample = sine_amplitude * (omega * frame as f64).sin();
        if frame % 4096 == 2048 {
            sample = 1.8;
        } else if frame % 4096 == 2052 {
            sample = -1.6;
        }
        samples.push(sample);
    }
    samples
}

/// Fs/4 sine sampled 45° off-peak: samples sit at amplitude·√½ (below a -1 dBTP
/// ceiling) while the reconstructed intersample peak reaches `amplitude`.
fn intersample_stress_mono(frames: usize, amplitude: f64) -> Vec<f64> {
    let mut mono = Vec::with_capacity(frames);
    for n in 0..frames {
        mono.push(amplitude * (PI * (n as f64 + 0.5) / 2.0).sin());
    }
    mono
}

fn stereo_from_mono(mono: &[f64]) -> Vec<f64> {
    let mut stereo = Vec::with_capacity(mono.len() * CHANNELS);
    for &sample in mono {
        stereo.push(sample);
        stereo.push(sample);
    }
    stereo
}

fn hard_panned_sine_for_frame_range(frames: usize, start_frame: usize, frequency: f64) -> Vec<f64> {
    let amplitude = db_to_linear(LISTENING_DSP_AMPLITUDE_DBFS);
    let omega = 2.0 * PI * frequency / SAMPLE_RATE as f64;
    let mut stereo = Vec::with_capacity(frames * CHANNELS);
    for frame in start_frame..start_frame + frames {
        stereo.push(amplitude * (omega * frame as f64).sin());
        stereo.push(0.0);
    }
    stereo
}

fn extract_channel(samples: &[f64], channels: usize, channel: usize) -> Vec<f64> {
    samples
        .chunks_exact(channels)
        .map(|frame| frame.get(channel).copied().unwrap_or(0.0))
        .collect()
}

fn rms_window(samples: &[f64], skip: usize, take: usize) -> Result<f64, String> {
    let start = skip.min(samples.len());
    let available = samples.len().saturating_sub(start);
    let count = take.min(available);
    if count == 0 {
        return Err("cannot compute RMS on an empty window".to_string());
    }
    let sum = samples[start..start + count]
        .iter()
        .map(|sample| sample * sample)
        .sum::<f64>();
    Ok((sum / count as f64).sqrt())
}

fn max_abs(samples: &[f64]) -> f64 {
    samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0, f64::max)
}

fn max_abs_delta(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max)
}

fn fold_frequency(frequency: f64, sample_rate: u32) -> f64 {
    let rate = sample_rate as f64;
    let folded = frequency.rem_euclid(rate);
    if folded > rate / 2.0 {
        rate - folded
    } else {
        folded
    }
}

fn output_skip_frames(sample_rate: u32) -> usize {
    (sample_rate as usize / 10).max(4096)
}

fn frames_for_duration(duration_secs: f64) -> usize {
    (duration_secs * SAMPLE_RATE as f64).round() as usize
}

fn hann_window(index: usize, len: usize) -> f64 {
    if len <= 1 {
        1.0
    } else {
        0.5 - 0.5 * (2.0 * PI * index as f64 / (len - 1) as f64).cos()
    }
}

fn curve_name(curve: NoiseShaperCurve) -> &'static str {
    match curve {
        NoiseShaperCurve::Lipshitz5 => "Lipshitz5",
        NoiseShaperCurve::FWeighted9 => "FWeighted9",
        NoiseShaperCurve::ModifiedE9 => "ModifiedE9",
        NoiseShaperCurve::ImprovedE9 => "ImprovedE9",
        NoiseShaperCurve::TpdfOnly => "TpdfOnly",
    }
}

fn lookahead_frames() -> usize {
    ((LIMITER_LOOKAHEAD_MS / 1000.0) * SAMPLE_RATE as f64).ceil() as usize
}

fn db_to_linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

fn db_ratio(numerator: f64, denominator: f64) -> f64 {
    if numerator <= 0.0 || denominator <= 0.0 {
        -400.0
    } else {
        (20.0 * (numerator / denominator).log10()).max(-400.0)
    }
}

fn positive_db_ratio(numerator: f64, denominator: f64) -> f64 {
    if numerator <= 0.0 && denominator <= 0.0 {
        0.0
    } else if denominator <= 0.0 {
        400.0
    } else if numerator <= 0.0 {
        -400.0
    } else {
        20.0 * (numerator / denominator).log10()
    }
}

fn dbfs(amplitude: f64) -> f64 {
    if amplitude <= 0.0 {
        -400.0
    } else {
        (20.0 * amplitude.log10()).max(-400.0)
    }
}

fn abs_delta(left: f64, right: f64) -> f64 {
    if left.is_finite() && right.is_finite() {
        (left - right).abs()
    } else if left == right {
        0.0
    } else {
        f64::INFINITY
    }
}

fn ebu_loudness_point_count(corpus: &EbuLoudnessCorpusSection) -> usize {
    corpus.global_loudness_points.len()
        + corpus.loudness_range_points.len()
        + corpus.max_momentary_points.len()
        + corpus.max_short_term_points.len()
}

fn first_failed_ebu_loudness_point(corpus: &EbuLoudnessCorpusSection) -> Option<&EbuCorpusPoint> {
    corpus
        .global_loudness_points
        .iter()
        .chain(corpus.loudness_range_points.iter())
        .chain(corpus.max_momentary_points.iter())
        .chain(corpus.max_short_term_points.iter())
        .find(|point| !point.passed)
}

fn first_failed_ebu_true_peak_point(
    corpus: &EbuTruePeakCorpusSection,
) -> Option<&EbuTruePeakPoint> {
    corpus
        .points
        .iter()
        .find(|point| !point.passed_reference_tolerance)
}

fn full_output_true_peak_over_limit_count(section: &FullOutputTruePeakSection) -> usize {
    section
        .points
        .iter()
        .filter(|point| point.output_margin_to_limiter_threshold_db > 0.0)
        .count()
}

fn print_report(report: &QualityReport) -> Result<(), String> {
    println!(
        "audio_quality_measurements schema_version={} mode={} path={}",
        report.schema_version, report.mode, report.conditions.measurement_path
    );
    println!(
        "benchmark_environment {}",
        environment_json(&report.environment)?
    );
    let gate_count = report
        .metrics
        .iter()
        .filter(|metric| metric.classification == Classification::Gate)
        .count();
    let failed_gate_count = report
        .metrics
        .iter()
        .filter(|metric| metric.classification == Classification::Gate && !metric.passed)
        .count();
    let report_count = report
        .metrics
        .iter()
        .filter(|metric| metric.classification == Classification::Report)
        .count();
    let skipped_count = report
        .metrics
        .iter()
        .filter(|metric| metric.classification == Classification::Skipped)
        .count();
    println!(
        "quality_metric_summary gates={} gate_passed={} gate_failed={} reports={} skipped={}",
        gate_count,
        gate_count - failed_gate_count,
        failed_gate_count,
        report_count,
        skipped_count
    );
    println!(
        "quality_thdn analyzer_floor_db={:.2} resampler_44k1_to_48k_db={:.2} limiter_below_threshold_db={:.2} frequency_hz={:.1} amplitude_dbfs={:.1}",
        report.thdn.analyzer_floor_db,
        report.thdn.resampler_44k1_to_48k_db,
        report.thdn.limiter_below_threshold_db,
        report.thdn.test_frequency_hz,
        report.thdn.amplitude_dbfs
    );
    println!(
        "quality_frequency_response from_rate={} to_rate={} passband_max_abs_deviation_db_20hz_to_18khz={:.4}",
        report.frequency_response.from_rate_hz,
        report.frequency_response.to_rate_hz,
        report
            .frequency_response
            .passband_max_abs_deviation_db_20hz_to_18khz
    );
    for point in &report.frequency_response.points {
        println!(
            "quality_frequency_point frequency_hz={:.1} gain_db={:.4} output_amplitude_dbfs={:.2}",
            point.frequency_hz, point.gain_db, point.output_amplitude_dbfs
        );
    }
    println!(
        "quality_limiter threshold_dbfs={:.2} input_peak_dbfs={:.2} output_peak_dbfs={:.2} margin_db={:.4} final_gain_reduction_db={:.2} transparent_sine_thdn_db={:.2}",
        report.limiter.threshold_dbfs,
        report.limiter.input_peak_dbfs,
        report.limiter.output_peak_dbfs,
        report.limiter.output_margin_to_threshold_db,
        report.limiter.final_gain_reduction_db,
        report.limiter.transparent_sine_thdn_db
    );
    println!(
        "quality_limiter_intersample_stress input_sample_peak_dbfs={:.2} input_true_peak_dbtp={:.2} true_peak_mode_output_dbtp={:.2} sample_peak_mode_output_dbtp={:.2}",
        report.limiter.intersample_stress_input_sample_peak_dbfs,
        report.limiter.intersample_stress_input_true_peak_dbtp,
        report.limiter.intersample_stress_true_peak_mode_output_dbtp,
        report.limiter.intersample_stress_sample_peak_mode_output_dbtp
    );
    println!(
        "quality_stopband from_rate={} to_rate={} worst_alias_attenuation_db={:.2} worst_residual_attenuation_db={:.2}",
        report.resampler_stopband.from_rate_hz,
        report.resampler_stopband.to_rate_hz,
        report.resampler_stopband.worst_alias_attenuation_db,
        report.resampler_stopband.worst_residual_attenuation_db
    );
    for point in &report.resampler_stopband.points {
        println!(
            "quality_stopband_point input_frequency_hz={:.1} folded_frequency_hz={:.1} alias_attenuation_db={:.2} residual_rms_attenuation_db={:.2} output_alias_amplitude_dbfs={:.2}",
            point.input_frequency_hz,
            point.folded_frequency_hz,
            point.alias_attenuation_db,
            point.residual_rms_attenuation_db,
            point.output_alias_amplitude_dbfs
        );
    }
    println!(
        "quality_saturation_continuity threshold={:.3} drive={:.2} output_gain_db={:.1} epsilon={:.1e} max_threshold_jump_linear={:.3e} max_first_derivative_mismatch={:.3e}",
        report.saturation_continuity.threshold,
        report.saturation_continuity.drive,
        report.saturation_continuity.output_gain_db,
        report.saturation_continuity.epsilon,
        report.saturation_continuity.max_threshold_jump_linear,
        report.saturation_continuity.max_first_derivative_mismatch
    );
    for point in &report.saturation_continuity.points {
        println!(
            "quality_saturation_continuity_point type={} sign={:.0} threshold_jump_linear={:.3e} inside_first_derivative={:.9} outside_first_derivative={:.9} first_derivative_mismatch={:.3e}",
            point.saturation_type,
            point.sign,
            point.threshold_jump_linear,
            point.inside_first_derivative,
            point.outside_first_derivative,
            point.first_derivative_mismatch
        );
    }
    println!(
        "quality_saturation_aliasing type={} quality={} stress_frequency_hz={:.1} direct_alias_energy_dbfs={:.2} upgraded_alias_energy_dbfs={:.2} alias_reduction_db={:.2} direct_fundamental_dbfs={:.2} upgraded_fundamental_dbfs={:.2} fundamental_delta_db={:.2}",
        report.saturation_aliasing.saturation_type,
        report.saturation_aliasing.upgraded_quality,
        report.saturation_aliasing.stress_frequency_hz,
        report.saturation_aliasing.direct_alias_energy_dbfs,
        report.saturation_aliasing.upgraded_alias_energy_dbfs,
        report.saturation_aliasing.alias_reduction_db,
        report.saturation_aliasing.direct_fundamental_dbfs,
        report.saturation_aliasing.upgraded_fundamental_dbfs,
        report.saturation_aliasing.fundamental_delta_db
    );
    for point in &report.saturation_aliasing.points {
        println!(
            "quality_saturation_alias_point harmonic={} folded_frequency_hz={:.1} direct_alias_dbfs={:.2} upgraded_alias_dbfs={:.2} reduction_db={:.2}",
            point.harmonic,
            point.folded_frequency_hz,
            point.direct_alias_dbfs,
            point.upgraded_alias_dbfs,
            point.reduction_db
        );
    }
    println!(
        "quality_listening_eq sample_rate={} target_gain_db={:.2} max_abs_target_error_db={:.4}",
        report.listening_dsp.sample_rate_hz,
        report.listening_dsp.eq.target_gain_db,
        report.listening_dsp.eq.max_abs_target_error_db
    );
    for point in &report.listening_dsp.eq.points {
        println!(
            "quality_listening_eq_point frequency_hz={:.1} measured_gain_db={:.4} target_gain_db={:.2} target_error_db={:.4}",
            point.frequency_hz,
            point.measured_gain_db,
            point.target_gain_db,
            point.target_error_db
        );
    }
    println!(
        "quality_listening_crossfeed mix={:.2} cutoff_hz={:.1} low_frequency_hz={:.1} high_frequency_hz={:.1} low_crossfeed_db={:.2} high_crossfeed_db={:.2} low_minus_high_crossfeed_db={:.2} reference_dc_direct_gain={:.9} reference_dc_cross_gain={:.9} reference_max_abs_error={:.3e} mix_change_first_frame_delta={:.3e} mix_change_preserved_max_delta={:.3e} mix_change_legacy_reset_max_delta={:.3e}",
        report.listening_dsp.crossfeed.mix,
        report.listening_dsp.crossfeed.cutoff_hz,
        report.listening_dsp.crossfeed.low_frequency_hz,
        report.listening_dsp.crossfeed.high_frequency_hz,
        report.listening_dsp.crossfeed.low_crossfeed_db,
        report.listening_dsp.crossfeed.high_crossfeed_db,
        report.listening_dsp.crossfeed.low_vs_high_crossfeed_db,
        report.listening_dsp.crossfeed.reference_dc_direct_gain,
        report.listening_dsp.crossfeed.reference_dc_cross_gain,
        report.listening_dsp.crossfeed.reference_max_abs_error,
        report
            .listening_dsp
            .crossfeed
            .mix_change_first_frame_delta,
        report
            .listening_dsp
            .crossfeed
            .mix_change_preserved_max_delta,
        report
            .listening_dsp
            .crossfeed
            .mix_change_legacy_reset_max_delta
    );
    println!(
        "quality_listening_dynamic_loudness reference_volume_db={:.1} low_volume_db={:.1} strength={:.2} bass_probe_hz={:.1} bass_compensation_db={:.2} presence_probe_hz={:.1} presence_compensation_db={:.2} reported_loudness_factor={:.3}",
        report.listening_dsp.dynamic_loudness.reference_volume_db,
        report.listening_dsp.dynamic_loudness.low_volume_db,
        report.listening_dsp.dynamic_loudness.strength,
        report.listening_dsp.dynamic_loudness.bass_probe_hz,
        report.listening_dsp.dynamic_loudness.bass_compensation_db,
        report.listening_dsp.dynamic_loudness.presence_probe_hz,
        report.listening_dsp.dynamic_loudness.presence_compensation_db,
        report.listening_dsp.dynamic_loudness.reported_loudness_factor
    );
    println!(
        "quality_noise_shaping sample_rate={} bits={} fft_len={} strongest_shaped_high_minus_ear_band_advantage_db={:.2}",
        report.noise_shaping.sample_rate_hz,
        report.noise_shaping.bits,
        report.noise_shaping.fft_len,
        report
            .noise_shaping
            .strongest_shaped_high_minus_ear_band_advantage_db
    );
    for point in &report.noise_shaping.points {
        println!(
            "quality_noise_shaping_point curve={} total_noise_rms_dbfs={:.2} ear_band_2k_to_6k_rms_dbfs={:.2} mid_band_6k_to_10k_rms_dbfs={:.2} high_band_14k_to_18k_rms_dbfs={:.2} high_minus_ear_band_db={:.2}",
            point.curve,
            point.total_noise_rms_dbfs,
            point.ear_band_2k_to_6k_rms_dbfs,
            point.mid_band_6k_to_10k_rms_dbfs,
            point.high_band_14k_to_18k_rms_dbfs,
            point.high_minus_ear_band_db
        );
    }
    println!(
        "quality_noise_shaping_boundaries low_level_input_dbfs={:.1} low_level_changed_fraction={:.6} silence_non_zero_fraction={:.6} stress_max_abs_output={:.9} stress_non_finite_outputs={}",
        report.noise_shaping.low_level_input_dbfs,
        report.noise_shaping.low_level_changed_fraction,
        report.noise_shaping.silence_non_zero_fraction,
        report.noise_shaping.stress_max_abs_output,
        report.noise_shaping.stress_non_finite_outputs
    );
    println!(
        "quality_loudness_reference fixtures={} max_integrated_delta_lu={:.9} max_momentary_delta_lu={:.9} max_short_term_delta_lu={:.9} max_loudness_range_delta_lu={:.9} max_true_peak_delta_db={:.6}",
        report.loudness_reference.fixtures.len(),
        report.loudness_reference.max_integrated_delta_lu,
        report.loudness_reference.max_momentary_delta_lu,
        report.loudness_reference.max_short_term_delta_lu,
        report.loudness_reference.max_loudness_range_delta_lu,
        report.loudness_reference.max_true_peak_delta_db
    );
    for fixture in &report.loudness_reference.fixtures {
        println!(
            "quality_loudness_fixture name={} integrated_lufs={:.6}/{:.6} momentary_lufs={:.6}/{:.6} short_term_lufs={:.6}/{:.6} lra_lu={:.6}/{:.6} true_peak_dbtp={:.6}/{:.6}",
            fixture.name,
            fixture.engine_integrated_lufs,
            fixture.reference_integrated_lufs,
            fixture.engine_momentary_lufs,
            fixture.reference_momentary_lufs,
            fixture.engine_short_term_lufs,
            fixture.reference_short_term_lufs,
            fixture.engine_loudness_range_lu,
            fixture.reference_loudness_range_lu,
            fixture.engine_true_peak_dbtp,
            fixture.reference_true_peak_dbtp
        );
    }
    let ebu_loudness = &report.loudness_reference.ebu_corpus;
    if ebu_loudness.available {
        println!(
            "quality_ebu_loudness_corpus source_dir={} files={} max_global_error_lu={:.6} max_lra_error_lu={:.6} max_momentary_error_lu={:.6} max_short_term_error_lu={:.6} passed={}",
            ebu_loudness.source_dir,
            ebu_loudness_point_count(ebu_loudness),
            ebu_loudness.max_abs_global_error_lu,
            ebu_loudness.max_abs_loudness_range_error_lu,
            ebu_loudness.max_abs_max_momentary_error_lu,
            ebu_loudness.max_abs_max_short_term_error_lu,
            first_failed_ebu_loudness_point(ebu_loudness).is_none()
        );
    } else {
        println!(
            "quality_ebu_loudness_corpus source_dir={} available=false missing_files={}",
            ebu_loudness.source_dir,
            ebu_loudness.missing_files.len()
        );
    }
    let full_output = &report.full_output_true_peak;
    println!(
        "quality_full_output_true_peak output_rate={} limiter_threshold_dbfs={:.2} final_noise_shaper_bits={} timeline={} unknown_tail_threshold_dbfs={:.1} unknown_tail_hold_ms={} unknown_tail_max_ms={} worst_output_true_peak_dbtp={:.3} worst_margin_to_limiter_db={:.3} over_limit_points={}",
        full_output.output_sample_rate_hz,
        full_output.limiter_threshold_dbfs,
        full_output.final_noise_shaper_bits,
        report.conditions.render_timeline,
        report.conditions.unknown_tail_energy_threshold_dbfs,
        report.conditions.unknown_tail_silence_hold_ms,
        report.conditions.unknown_tail_max_tail_ms,
        full_output.worst_output_true_peak_dbtp,
        full_output.worst_margin_to_limiter_threshold_db,
        full_output_true_peak_over_limit_count(full_output)
    );
    for point in &full_output.points {
        println!(
            "quality_full_output_point name={} source_kind={} source_rate={} source_channels={} source_frames={} output_frames={} rendered_frames={} algorithmic_latency_frames={} semantic_tail_frames={} tail_truncated={} output_true_peak_dbtp={:.3} limiter_margin_db={:.3}",
            point.name,
            point.source_kind,
            point.source_sample_rate_hz,
            point.source_channels,
            point.source_frames,
            point.output_frames,
            point.rendered_frames,
            point.algorithmic_latency_frames,
            point.semantic_tail_frames,
            point.tail_truncated,
            point.output_true_peak_dbtp,
            point.output_margin_to_limiter_threshold_db
        );
    }
    let ebu_true_peak = &full_output.ebu_true_peak_corpus;
    if ebu_true_peak.available {
        println!(
            "quality_ebu_true_peak_corpus source_dir={} files={} max_abs_expected_error_db={:.6} passed={}",
            ebu_true_peak.source_dir,
            ebu_true_peak.points.len(),
            ebu_true_peak.max_abs_expected_error_db,
            first_failed_ebu_true_peak_point(ebu_true_peak).is_none()
        );
    } else {
        println!(
            "quality_ebu_true_peak_corpus source_dir={} available=false missing_files={}",
            ebu_true_peak.source_dir,
            ebu_true_peak.missing_files.len()
        );
    }

    Ok(())
}

fn enforce_limits(report: &QualityReport) -> Result<(), String> {
    let failures: Vec<&MetricResult> = report
        .metrics
        .iter()
        .filter(|metric| metric.classification == Classification::Gate && !metric.passed)
        .collect();

    if failures.is_empty() {
        return Ok(());
    }

    let mut message = format!(
        "{} quality gate(s) failed (report-only metrics are not enforced):",
        failures.len()
    );
    for metric in failures {
        message.push_str(&format!(
            "\n  - gate '{}': {}",
            metric.name,
            metric.measured_vs_threshold()
        ));
        if let Some(detail) = &metric.detail {
            message.push_str(&format!(" ({detail})"));
        }
    }
    Err(message)
}
