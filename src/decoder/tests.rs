use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

#[cfg(feature = "http")]
use super::{source::fetch_range_once, NetworkError};
use super::{DecodeCancelToken, DecoderError, StreamingDecoder};

/// Monotonic counter for unique temp filenames within this test process.
static TMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A self-deleting temp file path for synthetic fixtures.
struct TempAudio {
    path: PathBuf,
}

impl TempAudio {
    fn new(ext: &str, bytes: &[u8]) -> Self {
        let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "aec_decoder_test_{}_{}.{}",
            std::process::id(),
            id,
            ext
        ));
        let mut file = std::fs::File::create(&path).expect("create temp fixture");
        file.write_all(bytes).expect("write temp fixture");
        file.flush().expect("flush temp fixture");
        Self { path }
    }

    fn path_str(&self) -> &str {
        self.path.to_str().expect("utf-8 temp path")
    }
}

impl Drop for TempAudio {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Build a little-endian PCM16 WAV byte stream for `frames` of audio.
///
/// `sample` is called with the (frame, channel) index and returns a value in
/// `[-1.0, 1.0]`. WAV carries no encoder delay/padding, making it the ideal
/// deterministic fixture for decode/seek assertions.
fn synth_wav<F: Fn(u64, usize) -> f64>(
    sample_rate: u32,
    channels: usize,
    frames: u64,
    sample: F,
) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let block_align = channels as u16 * (bits_per_sample / 8);
    let byte_rate = sample_rate * block_align as u32;
    let data_len = (frames as usize) * channels * 2;

    let mut buf = Vec::with_capacity(44 + data_len);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&(channels as u16).to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());
    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_len as u32).to_le_bytes());
    for frame in 0..frames {
        for ch in 0..channels {
            let v = sample(frame, ch).clamp(-1.0, 1.0);
            let q = (v * i16::MAX as f64).round() as i16;
            buf.extend_from_slice(&q.to_le_bytes());
        }
    }
    buf
}

/// Decode the entire stream into interleaved f64 samples.
fn decode_all_samples(decoder: &mut StreamingDecoder) -> Vec<f64> {
    decoder.decode_all().expect("decode_all")
}

#[cfg(feature = "http")]
#[test]
fn network_error_classifies_retriable_errors() {
    assert!(NetworkError::HttpTimeout.is_retriable());
    assert!(NetworkError::ConnectionReset.is_retriable());
    assert!(NetworkError::HttpStatus(408).is_retriable());
    assert!(NetworkError::HttpStatus(429).is_retriable());
    assert!(NetworkError::HttpStatus(500).is_retriable());
    assert!(NetworkError::HttpStatus(503).is_retriable());
    assert!(NetworkError::HttpStatus(504).is_retriable());
}

#[cfg(feature = "http")]
#[test]
fn network_error_classifies_non_retriable_errors() {
    assert!(!NetworkError::HttpStatus(401).is_retriable());
    assert!(!NetworkError::HttpStatus(403).is_retriable());
    assert!(!NetworkError::HttpStatus(404).is_retriable());
    assert!(!NetworkError::DnsFailure("no such host".into()).is_retriable());
    assert!(!NetworkError::TlsError("bad cert".into()).is_retriable());
    assert!(!NetworkError::Other("invalid response".into()).is_retriable());
}

#[test]
fn cancelled_open_returns_before_touching_source() {
    let cancelled = Arc::new(AtomicBool::new(true));
    let token = DecodeCancelToken::new(cancelled);

    let result = StreamingDecoder::open_with_credentials_and_cancel(
        "Z:/definitely/not/a/real/audio-file.flac",
        None,
        Some(token),
    );

    assert!(matches!(result, Err(DecoderError::Canceled)));
}

#[cfg(feature = "http")]
#[test]
fn cancelled_range_fetch_returns_before_network_request() {
    let cancelled = Arc::new(AtomicBool::new(true));
    let token = DecodeCancelToken::new(cancelled);
    let client = reqwest::blocking::Client::builder().build().unwrap();

    let result = fetch_range_once(
        &client,
        "http://127.0.0.1:9/never-requested.flac",
        None,
        0,
        8,
        Some(&token),
    );

    assert!(matches!(result, Err(NetworkError::Other(message)) if message == "Decode cancelled"));
}

// ---------------------------------------------------------------------------
// Typed error paths
// ---------------------------------------------------------------------------

#[test]
fn garbage_input_yields_unsupported_format() {
    // Random bytes with no recognizable container header.
    let garbage: Vec<u8> = (0u32..2048)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    let fixture = TempAudio::new("bin", &garbage);

    let result = StreamingDecoder::open(fixture.path_str());
    assert!(
        matches!(result, Err(DecoderError::UnsupportedFormat)),
        "garbage input should map to the typed UnsupportedFormat variant, got {:?}",
        result.err()
    );
}

#[test]
fn empty_input_yields_unsupported_format() {
    let fixture = TempAudio::new("wav", &[]);
    let result = StreamingDecoder::open(fixture.path_str());
    assert!(
        matches!(result, Err(DecoderError::UnsupportedFormat)),
        "empty input should map to UnsupportedFormat, got {:?}",
        result.err()
    );
}

#[test]
fn riff_without_audio_track_is_not_a_panic() {
    // A RIFF/WAVE container whose fmt chunk advertises an unknown codec tag
    // and which has no decodable audio track. Symphonia rejects this at probe
    // or track-selection time; either way it must be a typed error, not a panic
    // and not a generic success.
    let mut buf = Vec::new();
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&36u32.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&0xFFFFu16.to_le_bytes()); // bogus format tag
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&44100u32.to_le_bytes());
    buf.extend_from_slice(&176400u32.to_le_bytes());
    buf.extend_from_slice(&4u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&0u32.to_le_bytes());
    let fixture = TempAudio::new("wav", &buf);

    let result = StreamingDecoder::open(fixture.path_str());
    assert!(
        matches!(
            result,
            Err(DecoderError::NoAudioTrack)
                | Err(DecoderError::UnsupportedFormat)
                | Err(DecoderError::Probe(_))
                | Err(DecoderError::Decoder(_))
        ),
        "no-audio-track container must yield a typed error (not a panic / not Ok), got {:?}",
        result.as_ref().map(|_| "Ok").err()
    );
}

#[test]
fn truncated_wav_has_defined_policy_no_panic() {
    // A valid WAV header that claims a long data chunk but is cut off after a
    // few frames. Policy: never panic, never silently succeed with full data.
    // The decoder either errors out or returns the partial samples it could
    // recover — both are acceptable, a panic is not.
    let full = synth_wav(44100, 2, 4096, |frame, _| {
        (frame as f64 / 4096.0) * 2.0 - 1.0
    });
    // Keep header (44 bytes) + a small slice of audio, but the header still
    // advertises the full data length, so the tail is truncated.
    let mut truncated = full.clone();
    truncated.truncate(44 + 256);
    let fixture = TempAudio::new("wav", &truncated);

    let mut decoder =
        StreamingDecoder::open(fixture.path_str()).expect("open truncated wav header");
    // Drain; must not panic and must terminate.
    let mut total = 0usize;
    loop {
        match decoder.decode_next() {
            Ok(Some(s)) => total += s.len(),
            Ok(None) => break,
            Err(_) => break, // typed error is an acceptable documented outcome
        }
    }
    // Must not have silently produced the full (claimed) sample count.
    assert!(
        total < 4096 * 2,
        "truncated input must not silently yield the full claimed sample count"
    );
}

// ---------------------------------------------------------------------------
// Format -> capability matrix
// ---------------------------------------------------------------------------

#[test]
fn wav_format_capability_matrix() {
    // (sample_rate, channels, frames)
    let cases = [
        (44100u32, 2usize, 44100u64), // 1s stereo CD
        (48000, 1, 24000),            // 0.5s mono
        (22050, 2, 11025),            // 0.5s stereo low-rate
    ];

    for (sample_rate, channels, frames) in cases {
        let bytes = synth_wav(sample_rate, channels, frames, |frame, ch| {
            // Distinct deterministic ramp per channel.
            let base = (frame as f64 / frames as f64) * 2.0 - 1.0;
            base * (1.0 - 0.25 * ch as f64)
        });
        let fixture = TempAudio::new("wav", &bytes);
        let mut decoder = StreamingDecoder::open(fixture.path_str())
            .unwrap_or_else(|e| panic!("open {sample_rate}Hz/{channels}ch wav: {e:?}"));

        // Metadata assertions.
        assert_eq!(decoder.info.sample_rate, sample_rate);
        assert_eq!(decoder.info.channels, channels);
        let total_frames = decoder
            .info
            .total_frames
            .expect("total_frames known for wav");
        assert_eq!(total_frames, frames);
        let dur = decoder.info.duration_secs.expect("duration known for wav");
        assert!(
            (dur - frames as f64 / sample_rate as f64).abs() < 1e-6,
            "duration mismatch: {dur}"
        );

        // Decode assertions: WAV has no delay/padding, so sample count is exact.
        let samples = decode_all_samples(&mut decoder);
        assert_eq!(
            samples.len() as u64,
            frames * channels as u64,
            "decoded sample count mismatch for {sample_rate}Hz/{channels}ch"
        );
    }
}

// ---------------------------------------------------------------------------
// Seek tolerance + gapless double-trim regression
// ---------------------------------------------------------------------------

#[test]
fn seek_lands_within_documented_coarse_tolerance() {
    let sample_rate = 44100u32;
    let frames = sample_rate as u64 * 2; // 2 seconds
    let bytes = synth_wav(sample_rate, 1, frames, |frame, _| {
        // Ramp encoding the frame index so realized position is recoverable.
        (frame as f64 / frames as f64) * 2.0 - 1.0
    });
    let fixture = TempAudio::new("wav", &bytes);
    let mut decoder = StreamingDecoder::open(fixture.path_str()).expect("open wav");

    let target_secs = 1.0;
    let target_frame = (target_secs * sample_rate as f64) as u64;
    decoder.seek(target_secs).expect("seek");

    let realized = decoder.current_frame();
    let tol = StreamingDecoder::SEEK_COARSE_TOLERANCE_FRAMES;
    assert!(
        realized.abs_diff(target_frame) <= tol,
        "post-seek position {realized} not within {tol} frames of target {target_frame}"
    );

    // First decoded sample should reflect a position near the seek target, not
    // near zero (which would indicate a broken seek).
    let chunk = decoder
        .decode_next()
        .expect("decode after seek")
        .expect("samples after seek");
    let first = chunk[0];
    let expected_near_target = (realized as f64 / frames as f64) * 2.0 - 1.0;
    assert!(
        (first - expected_near_target).abs() < 0.05,
        "first post-seek sample {first} does not match seek target ramp {expected_near_target}"
    );
}

#[test]
fn seek_does_not_retrim_encoder_delay() {
    // Regression for the audited double-trim bug. WAV carries no real encoder
    // delay, so we inject a synthetic non-zero `encoder_delay` into the open
    // decoder, then verify that AFTER a seek the leading samples are NOT
    // trimmed (i.e. the seek does not re-arm start-of-stream delay trimming).
    let sample_rate = 44100u32;
    let frames = sample_rate as u64 * 2;
    let bytes = synth_wav(sample_rate, 1, frames, |frame, _| {
        (frame as f64 / frames as f64) * 2.0 - 1.0
    });
    let fixture = TempAudio::new("wav", &bytes);

    // Baseline: how many samples does a plain seek + decode yield at the target?
    let mut baseline = StreamingDecoder::open(fixture.path_str()).expect("open wav");
    baseline.seek(1.0).expect("seek baseline");
    let baseline_first = baseline
        .decode_next()
        .expect("baseline decode")
        .expect("baseline samples");

    // Now inject a large synthetic encoder_delay before seeking.
    let mut decoder = StreamingDecoder::open(fixture.path_str()).expect("open wav");
    decoder.info.encoder_delay = 5_000; // far larger than one packet
    decoder.seek(1.0).expect("seek with injected delay");
    let after = decoder
        .decode_next()
        .expect("decode after seek")
        .expect("samples after seek");

    // If the bug were present, the injected delay would trim ~5000 leading
    // samples from the post-seek stream, so the first surviving sample would
    // jump ahead. With the fix, post-seek output is identical to baseline
    // because at_stream_start is not re-armed by seek().
    assert_eq!(
        after.first(),
        baseline_first.first(),
        "post-seek decode was re-trimmed for encoder_delay (double-trim bug)"
    );
    assert_eq!(
        after.len(),
        baseline_first.len(),
        "post-seek chunk length changed under injected encoder_delay -> delay was wrongly re-applied"
    );
}

#[test]
fn start_of_stream_still_trims_encoder_delay() {
    // Ensure the fix does not break legitimate start-of-stream delay trimming.
    let sample_rate = 44100u32;
    let frames = 8_000u64;
    let bytes = synth_wav(sample_rate, 1, frames, |frame, _| {
        (frame as f64 / frames as f64) * 2.0 - 1.0
    });
    let fixture = TempAudio::new("wav", &bytes);

    let delay = 1_000u32;
    let mut decoder = StreamingDecoder::open(fixture.path_str()).expect("open wav");
    decoder.info.encoder_delay = delay;
    let samples = decode_all_samples(&mut decoder);

    // With a start-of-stream delay of `delay` frames (mono => `delay` samples),
    // total output should be frames - delay.
    assert_eq!(
        samples.len() as u64,
        frames - delay as u64,
        "start-of-stream encoder_delay trimming regressed"
    );
}
