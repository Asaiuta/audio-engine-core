//! Offline and read-only audio analysis APIs.
//!
//! Analysis consumes decoded audio and reports measurements or placement
//! evidence; it does not transform samples on the realtime callback path. The
//! implementation modules are being moved here incrementally. Interleaved block
//! geometry is validated by the shared [`crate::audio_block`] contract; the
//! processor lifecycle/error protocol is not duplicated in this module.

mod descriptors;
mod hpss;
mod measurement;
mod mel;
mod signal;
mod spectrum;

pub use descriptors::{
    BandMeasurements, DescriptorAnalyzer, DescriptorConfig, SpectralDescriptors,
};
pub use hpss::{HpssAnalyzer, HpssConfig, HpssFrame};
pub(crate) use measurement::{true_peak_fir, true_peak_reconstruction_l1_bound, TRUE_PEAK_DELAY};
pub use measurement::{LoudnessMeter, TruePeakDetector};
pub use mel::{
    MelAnalyzer, MelConfig, MelFrame, MelLogCompression, MelNormalization, MelSpectrumScale,
    MfccAnalyzer, MfccConfig, MfccFrame,
};
pub use signal::SignalMeasurements;
pub use spectrum::{SpectrumAnalyzer, SpectrumConfig, WindowFunction};
