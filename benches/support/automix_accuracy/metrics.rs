//! Offline scoring protocol. AMLt follows mir_eval 0.8.2; see the independent
//! generated goldens next to this module. Inputs are validated by the runner.

use serde::{Deserialize, Serialize};

pub const TEMPO_TOLERANCE: f64 = 0.04;
pub const BEAT_COLLAR_SEC: f64 = 0.070;
pub const CONTINUITY_TOLERANCE: f64 = 0.175;
pub const TRIM_SEC: f64 = 5.0;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyMode {
    Major,
    Minor,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub pitch_class: u8,
    pub mode: KeyMode,
}

pub fn tempo_accuracy(reference: f64, prediction: Option<f64>, octave: bool) -> f64 {
    let Some(prediction) = prediction.filter(|bpm| bpm.is_finite() && *bpm > 0.0) else {
        return 0.0;
    };
    let multiples: &[f64] = if octave {
        &[1.0 / 3.0, 0.5, 1.0, 2.0, 3.0]
    } else {
        &[1.0]
    };
    // A tiny numerical allowance represents the inclusive mathematical boundary,
    // not an extra measurement tolerance (e.g. 104 / 100 - 1 in binary64).
    f64::from(multiples.iter().any(|factor| {
        (prediction / (reference * factor) - 1.0).abs() <= TEMPO_TOLERANCE + 8.0 * f64::EPSILON
    }))
}

pub fn trim_beats(beats: &[f64], start: f64, end: f64) -> Vec<f64> {
    beats
        .iter()
        .copied()
        .filter(|beat| *beat >= start.max(TRIM_SEC) && *beat < end)
        .collect()
}

pub fn predicted_beats(bpm: Option<f64>, phase: Option<f64>, end: f64) -> Vec<f64> {
    let (Some(bpm), Some(phase)) = (bpm, phase) else {
        return Vec::new();
    };
    if !bpm.is_finite() || bpm <= 0.0 || !phase.is_finite() || phase < 0.0 {
        return Vec::new();
    }
    let period = 60.0 / bpm;
    // The public estimator is bounded to 55..200 BPM and 300 s. Keep malformed
    // benchmark predictions from causing an unbounded allocation too.
    if end / period > 100_000.0 {
        return Vec::new();
    }
    (0..((end - phase).max(0.0) / period).ceil() as usize)
        .map(|index| phase + index as f64 * period)
        .filter(|beat| *beat < end)
        .collect()
}

pub fn beat_f_measure(reference: &[f64], prediction: &[f64]) -> f64 {
    if reference.is_empty() || prediction.is_empty() {
        return 0.0;
    }
    // Earliest feasible pair is a maximum-cardinality matching for sorted
    // points with a fixed collar. Each index is consumed at most once.
    let (mut r, mut p, mut matches) = (0, 0, 0);
    while r < reference.len() && p < prediction.len() {
        let difference = prediction[p] - reference[r];
        // Compare the inclusive interval itself: subtracting two timestamps
        // can round a mathematical 70 ms difference just above 0.07.
        if prediction[p] >= reference[r] - BEAT_COLLAR_SEC
            && prediction[p] <= reference[r] + BEAT_COLLAR_SEC
        {
            matches += 1;
            r += 1;
            p += 1;
        } else if difference < 0.0 {
            p += 1;
        } else {
            r += 1;
        }
    }
    2.0 * matches as f64 / (reference.len() + prediction.len()) as f64
}

pub fn beat_amlt(reference: &[f64], prediction: &[f64]) -> f64 {
    if reference.len() <= 1 || prediction.len() <= 1 {
        return 0.0;
    }
    let offbeat: Vec<_> = reference
        .windows(2)
        .map(|pair| (pair[0] + pair[1]) / 2.0)
        .collect();
    let doubled: Vec<_> = reference
        .iter()
        .enumerate()
        .flat_map(|(index, beat)| std::iter::once(*beat).chain(offbeat.get(index).copied()))
        .collect();
    let even: Vec<_> = reference.iter().step_by(2).copied().collect();
    let odd: Vec<_> = reference.iter().skip(1).step_by(2).copied().collect();
    [reference, &offbeat, &doubled, &even, &odd]
        .into_iter()
        .map(|variant| total_continuity(variant, prediction))
        .fold(0.0, f64::max)
}

fn total_continuity(reference: &[f64], prediction: &[f64]) -> f64 {
    if reference.len() < 2 {
        return 0.0;
    }
    let mut used = vec![false; reference.len()];
    let mut successes = 0;
    for (index, beat) in prediction.iter().enumerate() {
        let right = reference.partition_point(|value| value < beat);
        let nearest = if right == 0 {
            0
        } else if right == reference.len() || beat - reference[right - 1] <= reference[right] - beat
        {
            right - 1
        } else {
            right
        };
        if used[nearest] {
            continue;
        }
        let forward = index == 0 || nearest == 0;
        let r = if forward && nearest + 1 < reference.len() {
            nearest
        } else {
            nearest.saturating_sub(1)
        };
        let p = if forward && index + 1 < prediction.len() {
            index
        } else {
            index.saturating_sub(1)
        };
        let reference_interval = reference[r + 1] - reference[r];
        let predicted_interval = prediction[p + 1] - prediction[p];
        let phase = (beat - reference[nearest]).abs() / reference_interval;
        let period = (1.0 - predicted_interval / reference_interval).abs();
        if phase < CONTINUITY_TOLERANCE && period < CONTINUITY_TOLERANCE {
            used[nearest] = true;
            successes += 1;
        }
    }
    successes as f64 / reference.len().max(prediction.len()) as f64
}

pub fn key_scores(reference: Key, prediction: Option<Key>) -> (f64, f64) {
    let Some(prediction) = prediction else {
        return (0.0, 0.0);
    };
    if prediction == reference {
        (1.0, 1.0)
    } else if prediction.mode == reference.mode
        && prediction.pitch_class == (reference.pitch_class + 7) % 12
    {
        (0.0, 0.5)
    } else if prediction.mode != reference.mode
        && prediction.pitch_class
            == (reference.pitch_class
                + if reference.mode == KeyMode::Major {
                    9
                } else {
                    3
                })
                % 12
    {
        (0.0, 0.3)
    } else if prediction.mode != reference.mode && prediction.pitch_class == reference.pitch_class {
        (0.0, 0.2)
    } else {
        (0.0, 0.0)
    }
}
