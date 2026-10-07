//! Versioned, bounded reports over core's mono L1 measurements.

use audio_engine_core::analysis::{
    DescriptorAnalyzer, DescriptorConfig, SignalMeasurements, WindowFunction,
};
use audio_engine_core::audio_block::AudioBlockRef;
use audio_engine_core::processor::ProcessError;
use serde::Serialize;

use crate::{check_cancel, AutomixAnalysis, AutomixError};

const FFT_SIZE: usize = 4096;
const HOP_SIZE: usize = 1024;
const FLATNESS_POWER_FLOOR: f64 = 1e-20;
const CLIPPING_THRESHOLD: f64 = 1.0;

/// Existing AutoMix analysis plus opt-in, independently versioned descriptors.
#[derive(Clone, Debug, Serialize)]
pub struct AutomixDescriptorAnalysis {
    /// Existing v4 analysis, with the same computation as the original entry point.
    pub analysis: AutomixAnalysis,
    /// Mono diagnostics over the actual decoded head and planned tail.
    pub descriptors: DescriptorReport,
}

/// Native-rate mono descriptors for independently accumulated source intervals.
///
/// These are diagnostic measurements, not whole-track or transition-quality
/// scores. Compare spectral values only when schema, profile, sample rate,
/// source channel count and configuration all match; keep coverage visible.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DescriptorReport {
    /// Descriptor wire schema, currently 1; independent of analysis version.
    pub schema_version: u32,
    /// Extraction and aggregation identity, currently `native-mono-l1-v1`.
    pub extractor_profile: &'static str,
    /// Native source sample rate in Hz; no resampling is performed.
    pub sample_rate_hz: u32,
    /// Source channels averaged in source order with f64 arithmetic.
    pub source_channels: usize,
    /// Fixed v1 extraction configuration.
    pub config: DescriptorReportConfig,
    /// Actual head and, when planned, tail coverage in source-time order.
    pub segments: Vec<DescriptorSegment>,
}

impl DescriptorReport {
    pub(super) fn new(sample_rate: u32, source_channels: usize) -> Self {
        Self {
            schema_version: 1,
            extractor_profile: "native-mono-l1-v1",
            sample_rate_hz: sample_rate,
            source_channels,
            config: DescriptorReportConfig {
                channel_policy: "arithmetic_mean_f64",
                fft_size: FFT_SIZE,
                hop_size: HOP_SIZE,
                window: "hann_periodic",
                flatness_power_floor: FLATNESS_POWER_FLOOR,
                clipping_threshold: CLIPPING_THRESHOLD,
            },
            segments: Vec::with_capacity(2),
        }
    }
}

/// Fixed v1 extraction identity; supplied by the producer, not user-configurable.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DescriptorReportConfig {
    /// `arithmetic_mean_f64`: ordered source-channel sum divided by channel count.
    pub channel_policy: &'static str,
    /// Complete FFT window size in mono samples, currently 4096.
    pub fft_size: usize,
    /// Samples between complete spectral publications, currently 1024.
    pub hop_size: usize,
    /// `hann_periodic`; each frame's unwindowed DC mean is removed first.
    pub window: &'static str,
    /// Absolute power floor for both means in spectral flatness, currently 1e-20.
    pub flatness_power_floor: f64,
    /// Inclusive absolute mono amplitude threshold for clipping counts, currently 1.
    pub clipping_threshold: f64,
}

/// Location of a planned analysis interval; not an intro/outro classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptorSegmentRole {
    /// The bounded interval starting at the source origin.
    Head,
    /// The independently accumulated trailing interval, when planned.
    Tail,
}

/// Whether decoding reached this interval's planned exclusive end.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptorCoverageStatus {
    /// Actual and planned ends match; this does not assert whole-track coverage.
    Complete,
    /// Source EOF arrived before the planned end, including an empty interval.
    ShortEof,
}

/// Signal and spectrum coverage of one actual decoded source interval.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DescriptorSegment {
    /// Planned head or tail role.
    pub role: DescriptorSegmentRole,
    /// Absolute inclusive source-frame origin.
    pub start_frame: u64,
    /// Planned absolute exclusive source-frame end.
    pub planned_end_frame: u64,
    /// Actual exclusive end, equal to start plus the mono sample count.
    pub end_frame: u64,
    /// Whether the actual interval reached its planned end.
    pub coverage_status: DescriptorCoverageStatus,
    /// Input frames with any nonfinite source channel or nonfinite downmix.
    pub nonfinite_input_frame_count: u64,
    /// Cumulative unwindowed mono measurements, including the partial FFT tail.
    pub mono_signal: DescriptorSignal,
    /// Equal-weight summaries of all complete FFT publications in source order.
    pub spectral: DescriptorSpectralSummary,
}

/// Measurements of the mono channel mean, not multichannel peak protection.
///
/// Empty input or any nonfinite input makes all measurements absent (JSON
/// null), while sample count remains available. Nonempty silence has zero
/// peak/RMS/DC/clipping and an absent crest factor. RMS does not replace LUFS.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DescriptorSignal {
    /// Mono samples measured, equal to the interval's actual frame length.
    pub sample_count: u64,
    /// Maximum absolute mono amplitude in linear units; not a true peak.
    pub sample_peak: Option<f64>,
    /// Unweighted, unwindowed root-mean-square mono amplitude in linear units.
    pub rms: Option<f64>,
    /// Linear sample-peak/RMS ratio, absent for silence.
    pub crest_factor: Option<f64>,
    /// Signed arithmetic mono mean in linear units.
    pub dc_offset: Option<f64>,
    /// Number of mono samples with absolute amplitude at least 1.
    pub clipping_count: Option<u64>,
    /// Clipping count divided by sample count, in 0..=1.
    pub clipping_ratio: Option<f64>,
}

impl DescriptorSignal {
    fn from_core(signal: &SignalMeasurements, polluted: bool) -> Self {
        if polluted {
            return Self {
                sample_count: signal.sample_count,
                sample_peak: None,
                rms: None,
                crest_factor: None,
                dc_offset: None,
                clipping_count: None,
                clipping_ratio: None,
            };
        }
        Self {
            sample_count: signal.sample_count,
            sample_peak: signal.sample_peak.filter(|value| value.is_finite()),
            rms: signal.rms.filter(|value| value.is_finite()),
            crest_factor: signal.crest_factor.filter(|value| value.is_finite()),
            dc_offset: signal.dc_offset.filter(|value| value.is_finite()),
            clipping_count: signal.clipping_count,
            clipping_ratio: signal.clipping_ratio.filter(|value| value.is_finite()),
        }
    }
}

/// Complete-window spectral denominators and metric-specific valid means.
///
/// Silence, DC and contaminated windows still count as frames. No warm-up or
/// trailing padding is synthesized; incomplete tails contribute only to signal.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DescriptorSpectralSummary {
    /// All published full FFT windows, including absent measurements.
    pub frame_count: u64,
    /// Published windows containing any nonfinite source or mono input frame.
    pub nonfinite_frame_count: u64,
    /// Absolute exclusive end of the last published window, absent before warm-up.
    pub last_end_frame: Option<u64>,
    /// Mean power-weighted centroid in Hz over finite, present frame values.
    pub centroid_hz: DescriptorMetricSummary,
    /// Mean dimensionless flatness in 0..=1 over finite, present frame values.
    pub flatness: DescriptorMetricSummary,
}

impl DescriptorSpectralSummary {
    fn new() -> Self {
        Self {
            frame_count: 0,
            nonfinite_frame_count: 0,
            last_end_frame: None,
            centroid_hz: DescriptorMetricSummary::new(),
            flatness: DescriptorMetricSummary::new(),
        }
    }
}

/// Ordered equal-weight mean and its explicit valid-window denominator.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DescriptorMetricSummary {
    /// Arithmetic mean, absent (JSON null) when no finite frame value exists.
    pub mean: Option<f64>,
    /// Number of finite, present values used by this metric.
    pub valid_frame_count: u64,
}

impl DescriptorMetricSummary {
    fn new() -> Self {
        Self {
            mean: None,
            valid_frame_count: 0,
        }
    }

    fn push(&mut self, value: Option<f64>) {
        if let Some(value) = value.filter(|value| value.is_finite()) {
            self.valid_frame_count += 1;
            let mean = self.mean.unwrap_or(0.0);
            // Source order and this recurrence are part of the profile identity.
            self.mean = Some(mean + (value - mean) / self.valid_frame_count as f64);
        }
    }
}

fn core_config() -> DescriptorConfig {
    DescriptorConfig {
        fft_size: FFT_SIZE,
        hop_size: HOP_SIZE,
        window: WindowFunction::Hann,
        rolloff_fraction: 0.85,
        flatness_floor: FLATNESS_POWER_FLOOR,
        contrast_band_edges_hz: Vec::new(),
        band_edges_hz: Vec::new(),
        band_rise_floor: 1e-10,
        clipping_threshold: CLIPPING_THRESHOLD,
    }
}

pub(super) struct DescriptorSegmentAccumulator {
    analyzer: DescriptorAnalyzer,
    channels: usize,
    mono: Vec<f64>,
    until_publication: usize,
    nonfinite_input_frame_count: u64,
    last_nonfinite_frame: Option<u64>,
    spectral: DescriptorSpectralSummary,
}

impl DescriptorSegmentAccumulator {
    pub(super) fn new(sample_rate: u32, channels: usize) -> Result<Self, AutomixError> {
        AudioBlockRef::new(&[], channels)
            .map_err(|error| AutomixError::Descriptor(ProcessError::InvalidBlock(error)))?;
        Ok(Self {
            analyzer: DescriptorAnalyzer::new(&core_config(), sample_rate)
                .map_err(AutomixError::Descriptor)?,
            channels,
            mono: vec![0.0; FFT_SIZE],
            until_publication: FFT_SIZE,
            nonfinite_input_frame_count: 0,
            last_nonfinite_frame: None,
            spectral: DescriptorSpectralSummary::new(),
        })
    }

    pub(super) fn process(
        &mut self,
        samples: &[f64],
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<(), AutomixError> {
        check_cancel(cancel)?;
        let block = AudioBlockRef::new(samples, self.channels)
            .map_err(|error| AutomixError::Descriptor(ProcessError::InvalidBlock(error)))?;
        let mut frames = block.samples().chunks_exact(self.channels);
        while frames.len() != 0 {
            let count = frames.len().min(self.until_publication);
            let first_frame = self.analyzer.signal().sample_count;
            for (index, frame) in frames.by_ref().take(count).enumerate() {
                let mut sum = 0.0;
                let mut nonfinite = false;
                for &sample in frame {
                    sum += sample;
                    nonfinite |= !sample.is_finite();
                }
                let mono = sum / self.channels as f64;
                if nonfinite || !mono.is_finite() {
                    self.nonfinite_input_frame_count += 1;
                    self.last_nonfinite_frame = Some(first_frame + index as u64);
                }
                self.mono[index] = mono;
            }
            // The latest-frame core API may overwrite earlier publications.
            // Never cross a publication boundary; flush partial decoder blocks
            // too, so their signal measurements and source clock stay current.
            let updates = self.analyzer.push(&self.mono[..count]);
            self.until_publication -= count;
            debug_assert_eq!(updates, usize::from(self.until_publication == 0));
            if self.until_publication == 0 {
                self.until_publication = HOP_SIZE;
                if let Some(frame) = self.analyzer.spectral() {
                    let polluted = self
                        .last_nonfinite_frame
                        .is_some_and(|bad| bad >= frame.end_sample - FFT_SIZE as u64);
                    self.spectral.frame_count += 1;
                    self.spectral.nonfinite_frame_count += u64::from(polluted);
                    self.spectral.last_end_frame = Some(frame.end_sample);
                    if !polluted {
                        self.spectral.centroid_hz.push(frame.centroid_hz);
                        self.spectral.flatness.push(frame.flatness);
                    }
                }
            }
            check_cancel(cancel)?;
        }
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        role: DescriptorSegmentRole,
        start_frame: u64,
        planned_end_frame: u64,
    ) -> DescriptorSegment {
        let mono_signal = DescriptorSignal::from_core(
            self.analyzer.signal(),
            self.nonfinite_input_frame_count != 0,
        );
        debug_assert!(start_frame <= planned_end_frame);
        debug_assert!(mono_signal.sample_count <= planned_end_frame - start_frame);
        let end_frame = start_frame + mono_signal.sample_count;
        self.spectral.last_end_frame = self
            .spectral
            .last_end_frame
            .map(|relative_end| start_frame + relative_end);
        DescriptorSegment {
            role,
            start_frame,
            planned_end_frame,
            end_frame,
            coverage_status: if end_frame == planned_end_frame {
                DescriptorCoverageStatus::Complete
            } else {
                DescriptorCoverageStatus::ShortEof
            },
            nonfinite_input_frame_count: self.nonfinite_input_frame_count,
            mono_signal,
            spectral: self.spectral,
        }
    }
}

#[cfg(test)]
mod tests;
