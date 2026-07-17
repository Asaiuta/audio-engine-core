use std::f64::consts::PI;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use audio_engine_core::config::{PhaseResponse, ResampleQuality};
use audio_engine_core::processor::{
    finish_checked, offline_stage_order_csv, process_checked, AtomicCrossfeedParams,
    AtomicDynamicLoudnessParams, AtomicDynamicLoudnessTelemetry, AtomicEqParams,
    AtomicNoiseShaperParams, AtomicPeakLimiterParams, AtomicSaturationParams, AtomicVolumeParams,
    AudioBlockMut, AudioBlockRef, Crossfeed, CrossfeedProcessor, DynamicLoudness, Equalizer,
    FFTConvolver, LimiterMode, LoudnessMeter, NoiseShaper, NoiseShaperCurve, OfflineRenderPolicy,
    OutputChainBuilder, OutputChainParams, PeakLimiter, ProcessBuffers, ProcessState,
    RenderTimeline, RenderedOutput, Saturation, SaturationQuality, SaturationType,
    StreamingProcessor, StreamingResampler, EQ_BANDS,
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
const FULL_OUTPUT_CHAIN_BITS: u32 = 24;
const EBU_LOUDNESS_TOLERANCE_LU: f64 = 0.1;
const EBU_LRA_TOLERANCE_LU: f64 = 1.0;
const EBU_TRUE_PEAK_LOWER_TOLERANCE_DB: f64 = -0.4;
const EBU_TRUE_PEAK_UPPER_TOLERANCE_DB: f64 = 0.2;
const SATURATION_ALIAS_STRESS_FREQUENCY_HZ: f64 = 11_000.0;
const SATURATION_ALIAS_AMPLITUDE_DBFS: f64 = -2.0;
const SATURATION_ALIAS_DRIVE: f64 = 1.35;
const SATURATION_ALIAS_MIX: f64 = 1.0;
const LISTENING_DSP_AMPLITUDE_DBFS: f64 = -24.0;
const LISTENING_EQ_TARGET_GAIN_DB: f64 = 6.0;
const LISTENING_CROSSFEED_MIX: f64 = 0.35;
const LISTENING_CROSSFEED_CUTOFF_HZ: f64 = 700.0;
const LISTENING_CROSSFEED_LOW_HZ: f64 = 80.0;
const LISTENING_CROSSFEED_HIGH_HZ: f64 = 2_000.0;
const LISTENING_LOUDNESS_REFERENCE_DB: f64 = -15.0;
const LISTENING_LOUDNESS_LOW_DB: f64 = -40.0;
const LISTENING_LOUDNESS_STRENGTH: f64 = 1.0;

// Gate thresholds for the synthetic (always-runs) metrics. These are deliberately
// conservative: observed values sit far inside them so the gates survive across
// CPUs, compiler versions, and debug/release builds. See
// research/benchmark-inventory.md for the per-metric margin rationale.
const GATE_RESAMPLER_THDN_MAX_DB: f64 = -100.0; // observed ~-187 dB
const GATE_PASSBAND_DEVIATION_MAX_DB: f64 = 0.10; // observed ~0.0013 dB
const GATE_ALIAS_ATTENUATION_MAX_DB: f64 = -100.0; // observed ~-295 dB (more negative is better)
const GATE_SATURATION_ALIAS_REDUCTION_MIN_DB: f64 = 6.0;
const GATE_EQ_TARGET_ERROR_MAX_DB: f64 = 0.50;
const GATE_CROSSFEED_HIGH_BAND_MIN_DB: f64 = -13.0;
const GATE_CROSSFEED_LOW_VS_HIGH_MAX_DB: f64 = -12.0;
const GATE_CROSSFEED_MIX_CHANGE_PRESERVED_MAX_DELTA: f64 = 1.0e-12;
const GATE_CROSSFEED_MIX_CHANGE_LEGACY_RESET_MIN_DELTA: f64 = 1.0e-4;
const GATE_DYNAMIC_LOUDNESS_BASS_COMPENSATION_MIN_DB: f64 = 6.0;
const GATE_LIMITER_MARGIN_MAX_DB: f64 = 0.05; // sample-peak ceiling; observed ~0.00 dB
const GATE_NOISE_SHAPER_ADVANTAGE_MIN_DB: f64 = 3.0; // observed up to ~+35 dB
const GATE_LOUDNESS_PARITY_MAX_LU: f64 = 1.0e-6; // wrapper forwards to ebur128

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
    saturation_aliasing: SaturationAliasingSection,
    listening_dsp: ListeningDspSection,
    noise_shaping: NoiseShapingSection,
    loudness_reference: LoudnessReferenceSection,
    full_output_true_peak: FullOutputTruePeakSection,
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
    saturation_aliasing_method: &'static str,
    listening_dsp_method: &'static str,
    limiter_method: &'static str,
    noise_shaping_method: &'static str,
    loudness_reference_method: &'static str,
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
    direct_alias_energy_dbfs: f64,
    upgraded_alias_energy_dbfs: f64,
    alias_reduction_db: f64,
    points: Vec<SaturationAliasPoint>,
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
    let saturation_aliasing = measure_saturation_aliasing(frames)?;
    let listening_dsp = measure_listening_dsp(frames)?;
    let noise_shaping = measure_noise_shaping(frames)?;
    let loudness_reference = measure_loudness_reference(ebu_dir)?;
    let render_policy = OfflineRenderPolicy::default();
    let full_output_true_peak = measure_full_output_true_peak(frames, ebu_dir, render_policy)?;

    let metrics = build_metrics(
        &resampler_fit,
        limiter_transparent,
        &frequency_response,
        &limiter,
        &resampler_stopband,
        &saturation_aliasing,
        &listening_dsp,
        &noise_shaping,
        &loudness_reference,
        &full_output_true_peak,
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
            saturation_aliasing_method: "11 kHz driven tube waveshaper; fit folded above-Nyquist harmonics and compare source-rate Direct vs Oversampled4x alias energy",
            listening_dsp_method: "single-tone fits through IIR EQ, Bauer crossfeed, and dynamic-loudness processors after settling; reports target response/effect size rather than external conformance",
            limiter_method: "PeakLimiter (default 4x-oversampled true-peak detection) in-place processing; reports sample-peak ceiling, below-threshold THD+N, and intersample-peak stress vs legacy sample-peak mode",
            noise_shaping_method: "16-bit NoiseShaper error signal FFT with Hann window; equal-width 2-6/6-10/14-18 kHz RMS bands",
            loudness_reference_method: "LoudnessMeter wrapper compared with direct ebur128 over deterministic f64 fixtures; optional EBU Tech 3341/3342 corpus expected-value checks",
            full_output_true_peak_method: format!(
                "offline canonical output chain: {} -> LoudnessMeter true-peak analysis",
                offline_stage_order_csv()
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
        saturation_aliasing,
        listening_dsp,
        noise_shaping,
        loudness_reference,
        full_output_true_peak,
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
    saturation_aliasing: &SaturationAliasingSection,
    listening_dsp: &ListeningDspSection,
    noise_shaping: &NoiseShapingSection,
    loudness_reference: &LoudnessReferenceSection,
    full_output_true_peak: &FullOutputTruePeakSection,
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
            "saturation_oversampled4x_alias_reduction",
            Comparison::AtLeast,
            saturation_aliasing.alias_reduction_db,
            GATE_SATURATION_ALIAS_REDUCTION_MIN_DB,
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
            "listening_crossfeed_high_band_level",
            Comparison::AtLeast,
            listening_dsp.crossfeed.high_crossfeed_db,
            GATE_CROSSFEED_HIGH_BAND_MIN_DB,
            "dB",
        ),
        MetricResult::gate(
            "listening_crossfeed_low_vs_high_attenuation",
            Comparison::AtMost,
            listening_dsp.crossfeed.low_vs_high_crossfeed_db,
            GATE_CROSSFEED_LOW_VS_HIGH_MAX_DB,
            "dB",
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
        MetricResult::report(
            "full_output_chain_worst_true_peak",
            Comparison::AtMost,
            full_output_true_peak.worst_output_true_peak_dbtp,
            FULL_OUTPUT_TRUE_PEAK_LIMIT_DBTP,
            "dBTP",
        ),
    ];

    // --- EBU corpus gates: enforced only when reference vectors are present ---
    build_ebu_loudness_metrics(&loudness_reference.ebu_corpus, &mut metrics);
    build_ebu_true_peak_metrics(&full_output_true_peak.ebu_true_peak_corpus, &mut metrics);

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
    );
    limiter.process(&mut samples);
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
    );
    limiter.process(&mut samples);
    let output_peak = max_abs(&samples);
    let output_peak_dbfs = dbfs(output_peak);

    // Intersample-peak stress: Fs/4 sine sampled 45° off-peak so every sample
    // sits at amplitude·√½ (below the ceiling) while the reconstructed true peak
    // reaches `amplitude`. True-peak mode must pull the output true peak under
    // the ceiling; sample-peak mode leaves it above (it never engages).
    let stress_mono = intersample_stress_mono(frames, 1.0);
    let mut stress_tp = stereo_from_mono(&stress_mono);
    let mut stress_sp = stress_tp.clone();
    PeakLimiter::with_mode(
        CHANNELS,
        SAMPLE_RATE,
        LIMITER_THRESHOLD_DBFS,
        LIMITER_LOOKAHEAD_MS,
        LIMITER_RELEASE_MS,
        LimiterMode::TruePeak,
    )
    .process(&mut stress_tp);
    PeakLimiter::with_mode(
        CHANNELS,
        SAMPLE_RATE,
        LIMITER_THRESHOLD_DBFS,
        LIMITER_LOOKAHEAD_MS,
        LIMITER_RELEASE_MS,
        LimiterMode::SamplePeak,
    )
    .process(&mut stress_sp);

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

    Ok(SaturationAliasingSection {
        sample_rate_hz: SAMPLE_RATE,
        saturation_type: "Tube",
        upgraded_quality: "Oversampled4x",
        stress_frequency_hz: SATURATION_ALIAS_STRESS_FREQUENCY_HZ,
        input_amplitude_dbfs: SATURATION_ALIAS_AMPLITUDE_DBFS,
        drive: SATURATION_ALIAS_DRIVE,
        mix: SATURATION_ALIAS_MIX,
        direct_fundamental_dbfs: dbfs(direct_fundamental.amplitude),
        upgraded_fundamental_dbfs: dbfs(upgraded_fundamental.amplitude),
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
        eq.set_all_bands(&gains, SAMPLE_RATE as f64);
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
    let continuity = measure_crossfeed_mix_change_continuity()?;

    Ok(ListeningCrossfeedSection {
        mix: LISTENING_CROSSFEED_MIX,
        cutoff_hz: LISTENING_CROSSFEED_CUTOFF_HZ,
        low_frequency_hz: LISTENING_CROSSFEED_LOW_HZ,
        high_frequency_hz: LISTENING_CROSSFEED_HIGH_HZ,
        low_crossfeed_db,
        high_crossfeed_db,
        low_vs_high_crossfeed_db: low_crossfeed_db - high_crossfeed_db,
        mix_change_preserved_max_delta: continuity.preserved_max_delta,
        mix_change_legacy_reset_max_delta: continuity.legacy_reset_max_delta,
    })
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
    preserved_max_delta: f64,
    legacy_reset_max_delta: f64,
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

    let warm = hard_panned_sine_for_frame_range(4096, 0, LISTENING_CROSSFEED_HIGH_HZ);
    let mut proc_warm = warm.clone();
    let mut reference_warm = warm.clone();
    let mut legacy_warm = warm;
    process_adapter_block(&mut proc, &mut proc_warm, CHANNELS)?;
    reference.process(&mut reference_warm, CHANNELS);
    legacy_reset.process(&mut legacy_warm, CHANNELS);

    let changed_mix = 0.7;
    params.set_mix(changed_mix);
    reference.set_mix(changed_mix);
    legacy_reset.set_mix(changed_mix);
    legacy_reset.set_sample_rate(SAMPLE_RATE as f64, LISTENING_CROSSFEED_CUTOFF_HZ);

    let next = hard_panned_sine_for_frame_range(512, 4096, LISTENING_CROSSFEED_HIGH_HZ);
    let mut proc_next = next.clone();
    let mut reference_next = next.clone();
    let mut legacy_next = next;
    process_adapter_block(&mut proc, &mut proc_next, CHANNELS)?;
    reference.process(&mut reference_next, CHANNELS);
    legacy_reset.process(&mut legacy_next, CHANNELS);

    Ok(CrossfeedContinuityResult {
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

    let mut telemetry_probe = DynamicLoudness::new(CHANNELS, SAMPLE_RATE as f64);
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
    let mut dynamic_loudness = DynamicLoudness::new(CHANNELS, SAMPLE_RATE as f64);
    dynamic_loudness.set_strength(LISTENING_LOUDNESS_STRENGTH);
    dynamic_loudness.set_volume_db(volume_db);

    for chunk in samples.chunks_mut(4096 * CHANNELS) {
        dynamic_loudness.process(chunk);
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
        let mut shaper = NoiseShaper::new(1, SAMPLE_RATE, NOISE_SHAPER_BITS);
        shaper.set_curve(curve);
        shaper.process(&mut output, 1);

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
    })
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
    let mut meter = LoudnessMeter::new(CHANNELS, SAMPLE_RATE);
    for chunk in samples.chunks(CHANNELS * 1024) {
        meter.process(chunk);
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
        chain: offline_stage_order_csv(),
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
        source_sample_rate,
        output_sample_rate: RESAMPLE_TO,
        eq_params,
        saturation_params,
        crossfeed_params,
        convolver_swap: Arc::new(ArcSwapOption::<FFTConvolver>::empty()),
        convolver_enabled: Arc::new(AtomicBool::new(false)),
        volume_params,
        dynamic_loudness_params,
        dynamic_loudness_telemetry: Arc::new(AtomicDynamicLoudnessTelemetry::new()),
        limiter_params,
        noise_shaper_params,
    })
    .build_render_chain_with_policy(render_policy)?;

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
    let mut meter = LoudnessMeter::new(channels, sample_rate);
    for chunk in samples.chunks(channels * 4096) {
        meter.process(chunk);
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
        "quality_saturation_aliasing type={} quality={} stress_frequency_hz={:.1} direct_alias_energy_dbfs={:.2} upgraded_alias_energy_dbfs={:.2} alias_reduction_db={:.2} direct_fundamental_dbfs={:.2} upgraded_fundamental_dbfs={:.2}",
        report.saturation_aliasing.saturation_type,
        report.saturation_aliasing.upgraded_quality,
        report.saturation_aliasing.stress_frequency_hz,
        report.saturation_aliasing.direct_alias_energy_dbfs,
        report.saturation_aliasing.upgraded_alias_energy_dbfs,
        report.saturation_aliasing.alias_reduction_db,
        report.saturation_aliasing.direct_fundamental_dbfs,
        report.saturation_aliasing.upgraded_fundamental_dbfs
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
        "quality_listening_crossfeed mix={:.2} cutoff_hz={:.1} low_frequency_hz={:.1} high_frequency_hz={:.1} low_crossfeed_db={:.2} high_crossfeed_db={:.2} low_vs_high_crossfeed_db={:.2} mix_change_preserved_max_delta={:.3e} mix_change_legacy_reset_max_delta={:.3e}",
        report.listening_dsp.crossfeed.mix,
        report.listening_dsp.crossfeed.cutoff_hz,
        report.listening_dsp.crossfeed.low_frequency_hz,
        report.listening_dsp.crossfeed.high_frequency_hz,
        report.listening_dsp.crossfeed.low_crossfeed_db,
        report.listening_dsp.crossfeed.high_crossfeed_db,
        report.listening_dsp.crossfeed.low_vs_high_crossfeed_db,
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
