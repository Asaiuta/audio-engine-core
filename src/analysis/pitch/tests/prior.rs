use super::*;

// O19: independent closed forms (uniform, arcsine, integer binomial tail),
// not a second call to the implementation's incomplete-beta recurrence.
#[test]
fn o19_beta_cdf_masses_match_closed_forms() {
    for (a, b) in [(1.0, 1.0), (0.5, 0.5), (2.0, 18.0), (3.0, 4.0), (8.0, 12.0)] {
        let prior = threshold_prior(a, b).unwrap();
        let mut cdf = 0.0;
        for (i, mass) in prior.iter().enumerate() {
            assert!(*mass >= 0.0);
            cdf += mass;
            let x = (i + 1) as f64 / 100.0;
            let expected = if a == 1.0 {
                x
            } else if a == 0.5 {
                2.0 / PI * x.sqrt().asin()
            } else {
                let n = (a + b - 1.0) as i32;
                ((a as i32)..=n)
                    .map(|k| {
                        let binomial =
                            (1..=k).fold(1.0, |c, j| c * f64::from(n + 1 - j) / f64::from(j));
                        binomial * x.powi(k) * (1.0 - x).powi(n - k)
                    })
                    .sum()
            };
            assert!(
                (cdf - expected).abs() < 2e-13,
                "Beta({a},{b}) x={x}: {cdf} != {expected}"
            );
        }
        assert!((cdf - 1.0).abs() < 1e-15);
    }
    for (a, b) in [(0.01, 0.1), (12.5, 3.25), (1000.0, 1000.0)] {
        let p = threshold_prior(a, b).unwrap();
        let reflected = threshold_prior(b, a).unwrap();
        for (left, right) in p.iter().zip(reflected.iter().rev()) {
            assert!((left - right).abs() < 2e-12, "{a}/{b}: {left}/{right}");
        }
    }
}

fn curve(dips: &[(usize, f64)]) -> Vec<f64> {
    let mut values = vec![1.5; 20];
    for &(lag, depth) in dips {
        values[lag] = depth;
    }
    values
}

fn candidates(values: &[f64], prior: &[f64; 100], fallback: f64) -> (f64, Vec<PitchCandidate>) {
    let mut output = Vec::with_capacity(101);
    let total = candidate_probabilities(values, values, 2, prior, fallback, 8_000.0, &mut output);
    (total, output)
}

// O19: strict s_i > v, first trough preference, equal-depth ties, the
// discounted global fallback and probability conservation on hand curves.
#[test]
fn o19_single_and_multiple_dips_follow_paper_rule() {
    let prior = threshold_prior(2.0, 18.0).unwrap();
    let survival = |v: f64| {
        prior
            .iter()
            .enumerate()
            .filter(|(i, _)| (i + 1) as f64 / 100.0 > v)
            .map(|(_, p)| p)
            .sum::<f64>()
    };
    let mut previous = 1.0;
    for depth in [0.0, 0.009, 0.05, 0.0856, 0.1, 0.2, 0.5, 0.82, 1.0, 1.2] {
        let (total, output) = candidates(&curve(&[(4, depth)]), &prior, 0.01);
        let expected = survival(depth) + 0.01 * (1.0 - survival(depth));
        assert!((total - expected).abs() <= 1e-15);
        assert!(total <= previous + 1e-15);
        previous = total;
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].period_samples, 4.0);
        assert_eq!(output[0].f0_hz, 2_000.0);
        assert_eq!(output[0].aperiodicity, depth);
    }
    let (total, output) = candidates(&curve(&[(4, 0.2), (8, 0.1), (12, 0.1)]), &prior, 0.01);
    assert_eq!(output.len(), 2); // the equal-depth later dip never wins
    assert!((output[0].probability - survival(0.2)).abs() < 1e-15);
    assert!(
        (output[1].probability - (survival(0.1) - survival(0.2) + 0.01 * (1.0 - survival(0.1))))
            .abs()
            < 1e-15
    );
    assert!((total - output.iter().map(|c| c.probability).sum::<f64>()).abs() < 1e-15);
    let (total, output) = candidates(&curve(&[(4, 1.2)]), &prior, 0.0);
    assert_eq!(total, 0.0);
    assert!(output.is_empty());
    let (total, output) = candidates(&[1.0; 20], &prior, 0.01);
    assert_eq!(total, 0.0);
    assert!(output.is_empty());
}

// O19: exercise the complete 100-threshold capacity bound and independently
// enumerate every threshold's YIN choice; changing tau count cannot allocate.
#[test]
fn o19_threshold_enumeration_matches_linear_scan() {
    let prior = threshold_prior(1.0, 1.0).unwrap();
    let mut values = vec![1.5; 406];
    for i in 0..101 {
        values[2 + 4 * i] = 1.005 - i as f64 * 0.01;
    }
    let mut expected = vec![0.0; values.len()];
    for (i, mass) in prior.iter().enumerate() {
        let threshold = (i + 1) as f64 / 100.0;
        let first = (0..101)
            .map(|j| 2 + 4 * j)
            .find(|&lag| values[lag] < threshold);
        match first {
            Some(lag) => expected[lag] += mass,
            None => expected[402] += 0.01 * mass,
        }
    }
    let mut output = Vec::with_capacity(101);
    assert_no_alloc::assert_no_alloc(|| {
        candidate_probabilities(&values, &values, 2, &prior, 0.01, 8_000.0, &mut output);
    });
    assert!(output.len() <= 101);
    for candidate in &output {
        assert!(
            (candidate.probability - expected[candidate.period_samples as usize]).abs() < 1e-15
        );
    }
}

// Kernel boundary/O1/O2: build d from a chosen CMNDF curve by solving
// y_tau = tau*d_tau/(prefix+d_tau); pin plateau, fallback and no-dip cases.
#[test]
fn yin_kernel_accepts_hand_built_differences() {
    fn difference_from(normalized: &[f64]) -> Vec<f64> {
        let mut d = vec![0.0; normalized.len()];
        d[1] = 1.0;
        let mut sum = 1.0;
        for tau in 2..d.len() {
            d[tau] = normalized[tau] * sum / (tau as f64 - normalized[tau]);
            sum += d[tau];
        }
        d
    }
    let desired = [1.0, 1.0, 0.8, 0.6, 0.2, 0.6, 0.8, 0.6, 0.05, 0.6, 0.8, 1.0];
    let d = difference_from(&desired);
    let mut normalized = [0.0; 12];
    let early = yin_kernel(&d, &mut normalized, 2, 10, 0.3).unwrap();
    assert!((early.period - 4.0).abs() < 1.0);
    let late = yin_kernel(&d, &mut normalized, 2, 10, 0.1).unwrap();
    assert!((late.period - 8.0).abs() < 1.0);
    let fallback = yin_kernel(&d, &mut normalized, 2, 10, 0.001).unwrap();
    assert_eq!(fallback, late);
    assert!(yin_kernel(&[0.0; 12], &mut normalized, 2, 10, 0.1).is_none());
    let monotone = difference_from(&[1.0; 12]);
    assert!(yin_kernel(&monotone, &mut normalized, 2, 10, 0.1).is_none());
}
