//! Shared deterministic synthetic signals for benchmark workloads.
//!
//! Only recipes that are exactly shared between probes live here. Each
//! benchmark's synthetic signal is part of its measured conditions, so a
//! probe-specific recipe must stay in that probe: consolidating
//! similar-but-different generators would silently change workloads and
//! invalidate recorded baselines.

/// Deterministic stereo-first musical test buffer shared by the resampler
/// matrix and streaming probes.
///
/// Two swept carriers (330/550 Hz with slow vibrato) under a 1.1 Hz envelope;
/// channels beyond the first two are scaled copies of the left channel.
pub fn resampler_test_buffer(frames: usize, channels: usize, sample_rate: u32) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    let sample_rate = sample_rate as f64;
    let mut left_phase = 0.0_f64;
    let mut right_phase = 0.0_f64;

    for frame in 0..frames {
        let t = frame as f64 / sample_rate;
        left_phase += std::f64::consts::TAU * (330.0 + 17.0 * (t * 2.5).sin()) / sample_rate;
        right_phase += std::f64::consts::TAU * (550.0 + 23.0 * (t * 1.7).cos()) / sample_rate;
        let envelope = 0.7 + 0.15 * (std::f64::consts::TAU * 1.1 * t).sin();
        let left = (left_phase.sin() * 0.6 + (left_phase * 2.0).sin() * 0.05) * envelope;
        let right = (right_phase.sin() * 0.55 - (right_phase * 3.0).cos() * 0.04) * envelope;

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
