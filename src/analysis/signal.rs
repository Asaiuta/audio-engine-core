//! Cumulative, unwindowed signal measurements; independent of loudness weighting.

/// Measurements over every mono sample supplied since construction or reset.
///
/// Amplitudes use the input's linear units (conventionally full scale = 1).
/// Empty input yields `None` for all measurements. A non-finite input makes
/// all measurements `None` until reset; `sample_count` still includes it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SignalMeasurements {
    /// Number of supplied samples, saturating at `u64::MAX`.
    pub sample_count: u64,
    /// Maximum absolute sample amplitude; zero for non-empty silence.
    pub sample_peak: Option<f64>,
    /// Root mean square amplitude: `sqrt(sum(x*x) / sample_count)`.
    pub rms: Option<f64>,
    /// Linear ratio `sample_peak / rms`; undefined for silence.
    pub crest_factor: Option<f64>,
    /// Arithmetic mean sample amplitude (DC), without windowing or filtering.
    pub dc_offset: Option<f64>,
    /// Count of samples with `abs(x) >= DescriptorConfig::clipping_threshold`.
    pub clipping_count: Option<u64>,
    /// `clipping_count / sample_count`, a dimensionless fraction in `[0, 1]`.
    pub clipping_ratio: Option<f64>,
    /// Strict opposite-sign adjacent pairs divided by `sample_count - 1`.
    ///
    /// Dimensionless crossings per sample interval; multiply by sample rate
    /// for crossings/second. Exact zeros (including negative zero) do not
    /// cross either neighbour. The first sample has no predecessor. Undefined
    /// with fewer than two samples; zero for constant signals and silence.
    pub zero_crossing_rate: Option<f64>,
}

#[derive(Default)]
pub(super) struct SignalAccumulator {
    count: u64,
    peak: f64,
    scaled_sum: f64,
    scaled_squares: f64,
    clipped: u64,
    crossings: u64,
    previous: f64,
    invalid: bool,
}

impl SignalAccumulator {
    pub(super) fn push(&mut self, sample: f64, clipping_threshold: f64) {
        if self.count == u64::MAX {
            self.invalid = true;
            return;
        }
        self.count += 1;
        if !sample.is_finite() {
            self.invalid = true;
        }
        if self.invalid {
            return;
        }
        let magnitude = sample.abs();
        // Scale BEFORE squaring, preserving finite measurements for inputs
        // near f64::MAX and tiny inputs whose raw square would underflow.
        if magnitude > self.peak {
            let ratio = self.peak / magnitude;
            self.scaled_sum *= ratio;
            self.scaled_squares *= ratio * ratio;
            self.peak = magnitude;
        }
        if self.peak > 0.0 {
            let normalized = sample / self.peak;
            self.scaled_sum += normalized;
            self.scaled_squares += normalized * normalized;
        }
        self.clipped += u64::from(magnitude >= clipping_threshold);
        self.crossings += u64::from(
            (sample > 0.0 && self.previous < 0.0) || (sample < 0.0 && self.previous > 0.0),
        );
        self.previous = sample;
    }

    pub(super) fn measurements(&self) -> SignalMeasurements {
        let mut result = SignalMeasurements {
            sample_count: self.count,
            ..SignalMeasurements::default()
        };
        if self.count == 0 || self.invalid {
            return result;
        }
        let count = self.count as f64;
        let scaled_rms = (self.scaled_squares / count).clamp(0.0, 1.0).sqrt();
        result.sample_peak = Some(self.peak);
        result.rms = Some(self.peak * scaled_rms);
        result.crest_factor = (scaled_rms > 0.0).then(|| 1.0 / scaled_rms);
        result.dc_offset = Some(self.peak * (self.scaled_sum / count).clamp(-1.0, 1.0));
        result.clipping_count = Some(self.clipped);
        result.clipping_ratio = Some(self.clipped as f64 / count);
        result.zero_crossing_rate =
            (self.count > 1).then(|| self.crossings as f64 / (self.count - 1) as f64);
        result
    }
}
