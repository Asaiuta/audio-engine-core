//! Linear gain ramp for smooth track-to-track transitions.

/// Linear gain ramp for smooth transitions between tracks.
/// Caches the current gain and per-sample delta so accessors stay cheap.
///
/// Use cases:
/// - Track-to-track gain changes
/// - Mute/unmute transitions
/// - Bypass switching
pub struct GainRamp {
    /// Starting gain value (linear)
    from: f64,
    /// Target gain value (linear)
    to: f64,
    /// Current gain value (linear)
    current: f64,
    /// Per-sample gain delta
    step: f64,
    /// Total samples in the ramp
    total_samples: usize,
    /// Remaining samples in the ramp
    remaining: usize,
}

impl GainRamp {
    /// Create a new gain ramp
    ///
    /// # Arguments
    /// * `from` - Starting gain (linear)
    /// * `to` - Target gain (linear)
    /// * `sample_rate` - Sample rate in Hz
    /// * `ramp_ms` - Ramp duration in milliseconds
    pub fn new(from: f64, to: f64, sample_rate: u32, ramp_ms: u32) -> Self {
        let total_samples = (sample_rate as u64 * ramp_ms as u64 / 1000) as usize;
        let total_samples = total_samples.max(1);

        Self {
            from,
            to,
            current: from,
            step: (to - from) / total_samples as f64,
            total_samples,
            remaining: total_samples,
        }
    }

    /// Create a ramp from 0 to target (fade in)
    pub fn fade_in(target: f64, sample_rate: u32, ramp_ms: u32) -> Self {
        Self::new(0.0, target, sample_rate, ramp_ms)
    }

    /// Create a ramp from current to 0 (fade out)
    pub fn fade_out(from: f64, sample_rate: u32, ramp_ms: u32) -> Self {
        Self::new(from, 0.0, sample_rate, ramp_ms)
    }

    /// Get the next gain value (call once per sample)
    /// Uses a cached per-sample delta and snaps to the target at ramp end.
    #[inline(always)]
    pub fn next_gain(&mut self) -> f64 {
        if self.remaining > 0 {
            let gain = self.current;
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.to;
            } else {
                self.current += self.step;
            }
            gain
        } else {
            self.to
        }
    }

    /// Apply gain ramp to a buffer (more efficient than per-sample calls)
    pub fn apply(&mut self, samples: &mut [f64]) {
        if samples.is_empty() {
            return;
        }

        let ramp_samples = samples.len().min(self.remaining);

        if ramp_samples > 0 {
            let mut gain = self.current;
            for sample in &mut samples[..ramp_samples] {
                *sample *= gain;
                gain += self.step;
            }

            self.remaining -= ramp_samples;
            if self.remaining == 0 {
                self.current = self.to;
            } else {
                self.current = gain;
            }
        }

        if ramp_samples < samples.len() && self.to != 1.0 {
            for sample in &mut samples[ramp_samples..] {
                *sample *= self.to;
            }
        }
    }

    /// Check if ramp is complete
    pub fn is_done(&self) -> bool {
        self.remaining == 0
    }

    /// Get remaining samples
    pub fn remaining_samples(&self) -> usize {
        self.remaining
    }

    /// Get current gain
    pub fn current(&self) -> f64 {
        self.current
    }

    /// Get target gain
    pub fn target(&self) -> f64 {
        self.to
    }

    /// Set a new target, starting from current position
    pub fn retarget(&mut self, new_target: f64, sample_rate: u32, ramp_ms: u32) {
        let current = self.current();
        self.from = current;
        self.to = new_target;
        let total_samples = (sample_rate as u64 * ramp_ms as u64 / 1000) as usize;
        self.total_samples = total_samples.max(1);
        self.remaining = self.total_samples;
        self.current = current;
        self.step = (self.to - self.from) / self.total_samples as f64;
    }

    /// Jump immediately to target (no ramp)
    pub fn jump(&mut self, target: f64) {
        self.from = target;
        self.to = target;
        self.current = target;
        self.step = 0.0;
        self.total_samples = 1;
        self.remaining = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::GainRamp;

    #[test]
    fn apply_matches_next_gain_loop_across_ramp_and_tail() {
        let mut apply_ramp = GainRamp::new(0.05, 0.95, 48_000, 10);
        let mut loop_ramp = GainRamp::new(0.05, 0.95, 48_000, 10);
        let mut apply_samples = synthetic_samples(2_048);
        let mut loop_samples = apply_samples.clone();

        for chunk in apply_samples.chunks_mut(127) {
            apply_ramp.apply(chunk);
        }
        for sample in &mut loop_samples {
            *sample *= loop_ramp.next_gain();
        }

        assert_eq!(
            apply_ramp.remaining_samples(),
            loop_ramp.remaining_samples()
        );
        assert_eq!(
            apply_ramp.current().to_bits(),
            loop_ramp.current().to_bits()
        );
        assert_samples_match(&apply_samples, &loop_samples);
    }

    #[test]
    fn apply_uses_target_gain_after_ramp_is_done() {
        let mut ramp = GainRamp::new(0.0, 0.5, 1_000, 1);
        let mut samples = vec![2.0; 5];

        ramp.apply(&mut samples);

        assert_eq!(samples, vec![0.0, 1.0, 1.0, 1.0, 1.0]);
        assert!(ramp.is_done());
        assert_eq!(ramp.current(), 0.5);
    }

    #[test]
    fn apply_does_not_change_empty_buffer_state() {
        let mut ramp = GainRamp::new(0.0, 1.0, 48_000, 100);
        let current = ramp.current();
        let remaining = ramp.remaining_samples();

        ramp.apply(&mut []);

        assert_eq!(ramp.current(), current);
        assert_eq!(ramp.remaining_samples(), remaining);
    }

    fn synthetic_samples(len: usize) -> Vec<f64> {
        (0..len)
            .map(|idx| {
                let t = idx as f64 / 48_000.0;
                (std::f64::consts::TAU * 997.0 * t).sin() * 0.35
            })
            .collect()
    }

    fn assert_samples_match(left: &[f64], right: &[f64]) {
        assert_eq!(left.len(), right.len());
        for (idx, (left, right)) in left.iter().zip(right).enumerate() {
            assert_eq!(
                left.to_bits(),
                right.to_bits(),
                "sample mismatch at {idx}: {left} != {right}"
            );
        }
    }
}
