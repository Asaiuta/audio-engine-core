//! Preserve the reference's repeated floating-point updates with bounded work.
use super::ProcessError;

const BLOCK: u64 = 1024;

#[derive(Clone, Copy, Debug)]
pub(super) struct BlockClock {
    pub(super) index: f64,
    pub(super) base: u64,
    remaining: u64,
}

/// Repeated subtraction of 1024. Each jump stays in one binade, where every
/// update is exact; exponent crossings are individually rounded. Rates are
/// u32, so a sparse gap crosses at most 33 binades, irrespective of its length.
fn rebase(mut value: f64, mut blocks: u64) -> f64 {
    while blocks != 0 {
        if value >= 1024.0 {
            let n = blocks.min((value / 1024.0).floor() as u64);
            value -= n as f64 * 1024.0;
            blocks -= n;
            continue;
        }
        value -= 1024.0;
        blocks -= 1;
        if blocks == 0 {
            break;
        }
        let magnitude = -value;
        let exponent = magnitude.to_bits() >> 52;
        let edge = f64::from_bits((exponent + 1) << 52);
        let n = ((edge - magnitude) / 1024.0).floor() as u64;
        // Leave the crossing and its immediate neighbour to scalar updates.
        let n = blocks.min(n.saturating_sub(1));
        value -= n as f64 * 1024.0;
        blocks -= n;
    }
    value
}

/// Constructor delay removal only moves a negative index toward zero, from
/// -(L-1) to approximately -(L/2-1). Within one binade the rounded increment
/// is constant after a possible ties-to-even parity correction. The integer
/// multiple stays on that binade's exact lattice; crossing updates are scalar.
fn add_repeated(mut value: f64, step: f64, mut count: u64) -> f64 {
    while count != 0 {
        let next = value + step;
        count -= 1;
        if count == 0 {
            return next;
        }
        let delta = next - value;
        value = next;
        let twice = value + step;
        let exponent = (-value).to_bits() >> 52;
        let edge = -f64::from_bits(exponent << 52);
        if value < 0.0 && twice < edge && twice - value == delta {
            let n = ((edge - value) / delta).floor() as u64;
            let n = count.min(n.saturating_sub(2));
            value += n as f64 * delta;
            count -= n;
        }
    }
    value
}

impl BlockClock {
    fn count(index: f64, length: usize, ratio: f64) -> u64 {
        ((BLOCK as f64 - (length + 1) as f64 - index) * ratio).floor() as u64
    }

    pub(super) fn initial(length: usize, ratio: f64) -> Self {
        let start = -(length as f64 - 1.0);
        let skip = (length as f64 * ratio / 2.0) as u64;
        // L <= 256 and the first virtual block has floor(1022 * ratio)
        // outputs, so the entire initial skip fits in that block.
        Self {
            index: add_repeated(start, 1.0 / ratio, skip),
            base: 0,
            remaining: Self::count(start, length, ratio) - skip,
        }
    }

    #[inline]
    pub(super) fn prepare(&mut self, length: usize, ratio: f64) -> Result<(), ProcessError> {
        if self.remaining != 0 {
            return Ok(());
        }
        self.prepare_block(length, ratio)
    }

    // The ordinary output only checks the remaining count. Keep virtual
    // block rebasing and the sparse-ratio search out of that per-output path.
    #[cold]
    #[inline(never)]
    fn prepare_block(&mut self, length: usize, ratio: f64) -> Result<(), ProcessError> {
        let first = self.index - BLOCK as f64;
        let mut blocks = 1;
        if Self::count(first, length, ratio) == 0 {
            // Binary search the first productive virtual block. u32 rates
            // bound this search to 23 iterations, even below 1:1024.
            let mut low = 1;
            let mut high = (1.0 / ratio / BLOCK as f64).ceil() as u64 + 3;
            while low < high {
                let mid = low + (high - low) / 2;
                if Self::count(rebase(self.index, mid), length, ratio) == 0 {
                    low = mid + 1;
                } else {
                    high = mid;
                }
            }
            blocks = low;
        }
        let index = if blocks == 1 {
            first
        } else {
            rebase(self.index, blocks)
        };
        let base = self
            .base
            .checked_add(blocks * BLOCK)
            .ok_or("sinc virtual clock overflow")?;
        let remaining = Self::count(index, length, ratio);
        if remaining == 0 {
            return Err("sinc virtual clock failed to find output".into());
        }
        self.index = index;
        self.base = base;
        self.remaining = remaining;
        Ok(())
    }

    pub(super) fn advance(&mut self, step: f64) {
        self.index += step;
        self.remaining -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_rebasing_matches_literal_subtraction_at_exponent_crossings() {
        for value in [
            -255.0,
            -63.123456789,
            -0.1,
            0.1,
            1023.7,
            16_777_213.314159,
            4_294_967_291.01,
        ] {
            for blocks in [0, 1, 2, 3, 1023, 8193, 262_145, 4_194_307] {
                let mut oracle = value;
                for _ in 0..blocks {
                    oracle -= 1024.0;
                }
                assert_eq!(
                    rebase(value, blocks).to_bits(),
                    oracle.to_bits(),
                    "{value}/{blocks}"
                );
            }
        }
    }

    #[test]
    fn skipped_delay_matches_literal_addition_for_all_quality_lengths() {
        for length in [64, 128, 256] {
            for (from, to) in [
                (48000_u32, 44101_u32),
                (3, 12347),
                (1, 100_003),
                (1001, 1),
                (1, 104729),
                (31, 65521),
                (12347, 53),
            ] {
                let ratio = to as f64 / from as f64;
                let count = (length as f64 * ratio / 2.0) as u64;
                let mut oracle = -(length as f64 - 1.0);
                for _ in 0..count {
                    oracle += 1.0 / ratio;
                }
                assert_eq!(
                    BlockClock::initial(length, ratio).index.to_bits(),
                    oracle.to_bits(),
                    "{length}/{from}/{to}"
                );
            }
        }
    }

    #[test]
    fn extreme_u32_ratios_construct_and_prepare_in_bounded_work() {
        for length in [64, 128, 256] {
            for (from, to) in [(u32::MAX, 1), (1, u32::MAX), (u32::MAX, 3), (3, u32::MAX)] {
                let ratio = to as f64 / from as f64;
                let mut clock = BlockClock::initial(length, ratio);
                for _ in 0..8 {
                    clock.prepare(length, ratio).unwrap();
                    assert!(clock.remaining > 0);
                    assert!(clock.index.is_finite());
                    clock.advance(1.0 / ratio);
                }
            }
        }
    }
}
