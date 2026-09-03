//! Stateless, low-level DSP mathematics shared by analysis and processors.
//!
//! This module intentionally contains only pure helpers with no callback state,
//! allocation, or parameter publication. Stateful realtime processors remain
//! under [`crate::processor`].

/// Convert a decibel value to a linear amplitude multiplier.
#[inline(always)]
pub fn db_to_linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

/// Convert a linear amplitude multiplier to decibels.
///
/// Non-positive values map to negative infinity, matching the historical
/// processor helper and preserving silence semantics.
#[inline(always)]
pub fn linear_to_db(linear: f64) -> f64 {
    if linear > 0.0 {
        20.0 * linear.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Modified Bessel function of the first kind, order zero.
///
/// Windowed FIR and spectrum designs call this bounded series during setup.
/// Keeping the pure scalar helper here lets analysis and processors share the
/// implementation without making either layer depend on the other.
pub(crate) fn modified_bessel_i0(value: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half_squared = (value * value) * 0.25;
    for order in 1..=64 {
        term *= half_squared / (order as f64 * order as f64);
        sum += term;
        if term.abs() <= sum.abs() * 1.0e-16 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::{db_to_linear, linear_to_db, modified_bessel_i0};

    #[test]
    fn conversions_match_the_audio_gain_contract() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-12);
        assert!((db_to_linear(-6.0) - 0.501).abs() < 0.01);
        assert!((linear_to_db(1.0) - 0.0).abs() < 1e-12);
        assert!((linear_to_db(0.5) + 6.0206).abs() < 0.01);
        assert!(linear_to_db(0.0).is_infinite());
        assert!(linear_to_db(-1.0).is_infinite());
    }

    #[test]
    fn modified_bessel_matches_known_values() {
        assert_eq!(modified_bessel_i0(0.0), 1.0);
        assert!((modified_bessel_i0(1.0) - 1.266_065_877_752_008_2).abs() < 1.0e-14);
        assert!((modified_bessel_i0(14.0) - 129_418.562_700_648_56).abs() < 1.0e-8);
    }
}
