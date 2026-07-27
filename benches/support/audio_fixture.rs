use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FIXTURE_SAMPLE_RATE_HZ: u32 = 48_000;
pub const FIXTURE_CHANNELS: usize = 2;
pub const FIXTURE_FRAMES: u64 = 12 * FIXTURE_SAMPLE_RATE_HZ as u64;
const FIXTURE_BITS_PER_SAMPLE: u16 = 16;
const FIXTURE_RELATIVE_PATH: &str =
    "target/audio-benchmark-fixtures/deterministic_pcm16_stereo_48k_12s.wav";
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeterministicPcmFixtureMetadata {
    pub container: String,
    pub codec: String,
    pub sample_format: String,
    pub sample_rate_hz: u32,
    pub channels: usize,
    pub frames: u64,
    pub duration_seconds: u32,
    pub byte_length: usize,
    pub content_fnv1a64: String,
    pub generation: String,
}

pub struct DeterministicPcmFixture {
    pub path: PathBuf,
    pub metadata: DeterministicPcmFixtureMetadata,
}

pub fn ensure_deterministic_pcm_fixture() -> Result<DeterministicPcmFixture, String> {
    let path = PathBuf::from(FIXTURE_RELATIVE_PATH);
    let bytes = deterministic_pcm_wav_bytes();
    let needs_write = match fs::read(&path) {
        Ok(existing) => existing != bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            return Err(format!(
                "failed to inspect deterministic fixture '{}': {error}",
                path.display()
            ));
        }
    };

    if needs_write {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create deterministic fixture directory '{}': {error}",
                    parent.display()
                )
            })?;
        }
        fs::write(&path, &bytes).map_err(|error| {
            format!(
                "failed to write deterministic fixture '{}': {error}",
                path.display()
            )
        })?;
    }

    Ok(DeterministicPcmFixture {
        path,
        metadata: metadata_for_bytes(&bytes),
    })
}

pub fn deterministic_pcm_wav_bytes() -> Vec<u8> {
    let block_align = FIXTURE_CHANNELS as u16 * (FIXTURE_BITS_PER_SAMPLE / 8);
    let byte_rate = FIXTURE_SAMPLE_RATE_HZ * u32::from(block_align);
    let data_len = FIXTURE_FRAMES as usize * FIXTURE_CHANNELS * 2;
    let mut bytes = Vec::with_capacity(44 + data_len);

    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&(FIXTURE_CHANNELS as u16).to_le_bytes());
    bytes.extend_from_slice(&FIXTURE_SAMPLE_RATE_HZ.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&FIXTURE_BITS_PER_SAMPLE.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());

    for frame in 0..FIXTURE_FRAMES {
        for channel in 0..FIXTURE_CHANNELS {
            bytes.extend_from_slice(&fixture_sample(frame, channel).to_le_bytes());
        }
    }
    bytes
}

fn fixture_sample(frame: u64, channel: usize) -> i16 {
    let triangle_a = triangle(frame + channel as u64 * 71, 480, 12_000);
    let triangle_b = triangle(frame * 3 + channel as u64 * 113, 1_200, 5_000);
    let beat_phase = frame % 24_000;
    let beat_gain = if beat_phase < 2_400 { 4_i32 } else { 1_i32 };
    let channel_bias = if channel == 0 { 700_i32 } else { -700_i32 };
    (triangle_a * beat_gain / 4 + triangle_b + channel_bias).clamp(i16::MIN as i32, i16::MAX as i32)
        as i16
}

fn triangle(frame: u64, period: u64, amplitude: i32) -> i32 {
    let phase = frame % period;
    let half = period / 2;
    let ramp = if phase < half { phase } else { period - phase };
    (ramp as i32 * 4 * amplitude / period as i32) - amplitude
}

fn metadata_for_bytes(bytes: &[u8]) -> DeterministicPcmFixtureMetadata {
    DeterministicPcmFixtureMetadata {
        container: "RIFF/WAVE".to_string(),
        codec: "PCM signed integer little-endian".to_string(),
        sample_format: format!("s{FIXTURE_BITS_PER_SAMPLE}"),
        sample_rate_hz: FIXTURE_SAMPLE_RATE_HZ,
        channels: FIXTURE_CHANNELS,
        frames: FIXTURE_FRAMES,
        duration_seconds: (FIXTURE_FRAMES / u64::from(FIXTURE_SAMPLE_RATE_HZ)) as u32,
        byte_length: bytes.len(),
        content_fnv1a64: format!("{:016x}", fnv1a64(bytes)),
        generation: "integer_triangle_120bpm_v1".to_string(),
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

pub fn fixture_path_display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
