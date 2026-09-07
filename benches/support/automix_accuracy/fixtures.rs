//! Deterministic audio-to-result fixtures, with annotations from the synthesis
//! clock rather than the detector's observation grid. No external media.

use serde::{Deserialize, Serialize};

use super::corpus::Annotation;

pub const FIXTURE_REVISION: &str = "percussive_pcm16_v1";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    Straight,
    Subdivision,
    Swung,
    OffGrid,
    Ramp,
    Silence,
    Noise,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Fixture {
    pub id: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_sec: f64,
    pub bpm: f64,
    pub phase_sec: f64,
    pub pattern: Pattern,
}

impl Fixture {
    pub fn constant(sample_rate: u32, bpm: f64) -> Self {
        Self {
            id: format!("straight_{bpm}_{sample_rate}"),
            sample_rate,
            channels: 2,
            duration_sec: 60.0,
            bpm,
            phase_sec: 0.217,
            pattern: Pattern::Straight,
        }
    }

    pub fn precision_gate(&self) -> bool {
        matches!(
            self.pattern,
            Pattern::Straight | Pattern::Subdivision | Pattern::Swung
        ) && self.duration_sec >= 5.0
    }

    pub fn render(&self) -> (Vec<u8>, Annotation) {
        let frames = (self.duration_sec * f64::from(self.sample_rate)).round() as usize;
        let mut mono = vec![0.0_f64; frames];
        let mut beats = Vec::new();
        let mut beat_time = self.phase_sec;
        let mut index = 0_usize;
        let mut seed = 0x5a17_d3b9_u32;
        if self.pattern == Pattern::Noise {
            for sample in &mut mono {
                *sample = noise(&mut seed) * 0.25;
            }
        } else if self.pattern != Pattern::Silence {
            while beat_time < self.duration_sec {
                let bpm = if self.pattern == Pattern::Ramp {
                    self.bpm + 24.0 * beat_time / self.duration_sec
                } else {
                    self.bpm
                };
                let period = 60.0 / bpm;
                let jitter = if self.pattern == Pattern::OffGrid {
                    0.065 * (index as f64 * 1.713).sin()
                } else {
                    0.0
                };
                let onset = beat_time + jitter;
                beats.push(onset);
                add_click(&mut mono, self.sample_rate, onset, 0.8, &mut seed);
                if matches!(self.pattern, Pattern::Subdivision | Pattern::Swung) {
                    let fraction = if self.pattern == Pattern::Swung {
                        2.0 / 3.0
                    } else {
                        0.5
                    };
                    // A distinctly weaker subdivision establishes the annotated
                    // metrical level in the 70/140 and 90/180 ambiguity pairs.
                    add_click(
                        &mut mono,
                        self.sample_rate,
                        beat_time + period * fraction,
                        0.12,
                        &mut seed,
                    );
                }
                index += 1;
                beat_time = self.phase_sec + index as f64 * period;
                if self.pattern == Pattern::Ramp {
                    beat_time = onset + period;
                }
            }
        }
        let annotation = Annotation {
            schema_version: 1,
            tempo_bpm: Some(self.bpm),
            beats_sec: Some(beats),
            key: None,
        };
        (
            pcm16_wav(self.sample_rate, self.channels, &mono),
            annotation,
        )
    }
}

fn noise(seed: &mut u32) -> f64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    f64::from(*seed) / f64::from(u32::MAX) * 2.0 - 1.0
}

fn add_click(mono: &mut [f64], sample_rate: u32, onset: f64, amplitude: f64, seed: &mut u32) {
    let start = (onset * f64::from(sample_rate)).round() as usize;
    let length = (0.006 * f64::from(sample_rate)).round() as usize;
    if start >= mono.len() {
        return;
    }
    for (offset, sample) in mono.iter_mut().skip(start).take(length).enumerate() {
        let time = offset as f64 / f64::from(sample_rate);
        let transient = 0.65 * noise(seed) + 0.35 * (std::f64::consts::TAU * 900.0 * time).cos();
        *sample += amplitude * (-time / 0.0015).exp() * transient;
    }
}

pub fn pcm16_wav(sample_rate: u32, channels: u16, mono: &[f64]) -> Vec<u8> {
    let data_len = mono.len() * channels as usize * 2;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in mono {
        for channel in 0..channels {
            let gain = if channel % 2 == 0 { 1.0 } else { 0.85 };
            let quantized = (sample * gain * f64::from(i16::MAX))
                .round()
                .clamp(-32768.0, 32767.0) as i16;
            bytes.extend_from_slice(&quantized.to_le_bytes());
        }
    }
    bytes
}

pub fn suite() -> Vec<Fixture> {
    let mut fixtures = Vec::new();
    for rate in [44_100, 48_000, 96_000] {
        for bpm in [60.0, 70.0, 90.0, 120.0, 127.3, 140.0, 174.6, 180.0, 200.0] {
            fixtures.push(Fixture::constant(rate, bpm));
        }
    }
    for bpm in [70.0, 90.0, 140.0, 180.0] {
        let mut fixture = Fixture::constant(48_000, bpm);
        fixture.id = format!("accented_{bpm}_48000");
        fixture.pattern = Pattern::Subdivision;
        fixtures.push(fixture);
    }
    for pattern in [
        Pattern::Swung,
        Pattern::OffGrid,
        Pattern::Ramp,
        Pattern::Silence,
        Pattern::Noise,
    ] {
        let mut fixture = Fixture::constant(48_000, 127.3);
        fixture.id = format!("{pattern:?}_127.3_48000").to_lowercase();
        fixture.pattern = pattern;
        fixture.channels = 1;
        fixtures.push(fixture);
    }
    let mut short = Fixture::constant(44_100, 127.3);
    short.id = "short_0.4s_44100".into();
    short.duration_sec = 0.4;
    fixtures.push(short);
    fixtures.sort_by(|a, b| a.id.cmp(&b.id));
    fixtures
}
