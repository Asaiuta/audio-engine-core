//! Streaming and offline read-only audio analysis APIs.
//!
//! Analyzers consume decoded blocks and publish measurements from bounded
//! windows or accumulated statistics. They do not transform playback samples.
//! Streaming input does not by itself guarantee callback-safe execution; use
//! each analyzer's documented cost and realtime contract. Interleaved geometry
//! is validated by the shared [`crate::audio_block`] contract.

mod descriptors;
mod hpss;
mod measurement;
mod mel;
mod pitch;
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
pub use pitch::{PitchAnalyzer, PitchCandidate, PitchConfig, PitchFrame};
pub use signal::SignalMeasurements;
pub use spectrum::{SpectrumAnalyzer, SpectrumConfig, WindowFunction};
