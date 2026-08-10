use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

#[cfg(feature = "http")]
use super::{error::network_error_to_decoder_error, NetworkError};
use super::{
    DecodeCancelToken, DecoderError, HttpCredentials, MediaLocation, MediaLocationError,
    MediaLocationKind, OpenedMediaSource, StreamingDecoder,
};

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

fn local(path: impl Into<PathBuf>) -> MediaLocation {
    MediaLocation::local(path)
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
    assert!(!NetworkError::RangeNotSupported { status: 200 }.is_retriable());
    assert!(!NetworkError::InvalidRangeResponse("wrong offset".into()).is_retriable());
    assert!(!NetworkError::DnsFailure("no such host".into()).is_retriable());
    assert!(!NetworkError::TlsError("bad cert".into()).is_retriable());
    assert!(!NetworkError::Other("invalid response".into()).is_retriable());
}

#[test]
fn http_credentials_debug_redacts_both_basic_auth_fields() {
    let credentials = HttpCredentials {
        username: "api-user-secret".to_string(),
        password: "api-password-secret".to_string(),
    };

    let rendered = format!("{credentials:?}");
    assert!(!rendered.contains("api-user-secret"));
    assert!(!rendered.contains("api-password-secret"));
    assert_eq!(rendered.matches("[REDACTED]").count(), 2);
}

#[cfg(feature = "http")]
#[test]
fn reqwest_errors_drop_signed_urls_before_network_error_rendering() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve local port");
    let address = listener.local_addr().expect("reserved local address");
    let server = std::thread::spawn(move || {
        let (socket, _) = listener.accept().expect("accept signed URL request");
        drop(socket);
    });

    let token = "signed-query-secret";
    let request_url = format!(
        "http://basic-user:basic-password@{address}/private/audio.flac?token={token}#fragment"
    );
    let error = reqwest::blocking::Client::new()
        .get(&request_url)
        .send()
        .expect_err("server closes before returning an HTTP response");
    server.join().expect("test server completed");
    assert!(
        error.url().is_some_and(|url| url.as_str().contains(token)),
        "fixture must prove reqwest retained the sensitive URL"
    );

    let rendered = NetworkError::from(error).to_string();
    for secret in [token, "basic-user", "basic-password", &address.to_string()] {
        assert!(
            !rendered.contains(secret),
            "network error leaked `{secret}`: {rendered}"
        );
    }
}

#[test]
fn cancelled_open_returns_before_touching_source() {
    let cancelled = Arc::new(AtomicBool::new(true));
    let token = DecodeCancelToken::from_flag(cancelled);

    let result = StreamingDecoder::open_with_credentials_and_cancel(
        local("Z:/definitely/not/a/real/audio-file.flac"),
        None,
        Some(token),
    );

    assert!(matches!(result, Err(DecoderError::Canceled)));
}

/// The token owns its protocol: a caller cancels through a clone rather than
/// through a separately held `AtomicBool`.
#[test]
fn cancel_token_owns_its_own_flag() {
    let token = DecodeCancelToken::new();
    let remote = token.clone();
    assert!(!token.is_cancelled());

    remote.cancel();

    assert!(token.is_cancelled());
    let result = StreamingDecoder::open_with_credentials_and_cancel(
        local("Z:/definitely/not/a/real/audio-file.flac"),
        None,
        Some(token),
    );
    assert!(matches!(result, Err(DecoderError::Canceled)));
}

#[test]
fn decoder_can_be_built_from_an_already_opened_local_source() {
    let wav = synth_wav(48_000, 2, 32, |frame, channel| {
        (frame as f64 + channel as f64) / 64.0
    });
    let fixture = TempAudio::new("wav", &wav);
    let source = OpenedMediaSource::open_local(fixture.path_str(), None).expect("open source");

    let mut decoder =
        StreamingDecoder::from_opened_source(source, None).expect("construct decoder");

    assert_eq!(decoder.info().sample_rate, 48_000);
    assert_eq!(decoder.info().channels, 2);
    assert_eq!(decode_all_samples(&mut decoder).len(), 64);
}

#[test]
fn borrowed_decode_exposes_decoder_storage_without_caller_staging() {
    let wav = synth_wav(48_000, 2, 64, |frame, channel| {
        (frame as f64 + channel as f64) / 128.0
    });
    let fixture = TempAudio::new("wav", &wav);
    let mut decoder = StreamingDecoder::open(local(fixture.path_str())).expect("open decoder");

    let samples = decoder
        .decode_next_borrowed()
        .expect("decode packet")
        .expect("borrowed output");

    assert_eq!(samples.len(), 128);
    assert!(samples.iter().any(|sample| *sample != 0.0));
}

#[test]
fn probed_builder_reports_exact_fixed_decoder_staging_bytes() {
    let wav = synth_wav(48_000, 2, 64, |frame, channel| {
        (frame as f64 + channel as f64) / 128.0
    });
    let fixture = TempAudio::new("wav", &wav);
    let source = OpenedMediaSource::open_local(fixture.path_str(), None).expect("open source");
    let builder = StreamingDecoder::probe_opened_source(source, None).expect("probe opened source");
    let reserved_bytes = builder.staging_buffer_bytes().expect("staging bytes");

    let mut decoder = builder.build().expect("build decoder");

    assert_eq!(decoder.staging_buffer_bytes(), reserved_bytes);
    assert!(decoder.decode_next_borrowed().expect("decode").is_some());
    assert_eq!(decoder.staging_buffer_bytes(), reserved_bytes);
}

#[test]
fn cancelled_opened_source_construction_returns_before_probe() {
    let wav = synth_wav(44_100, 1, 8, |frame, _| frame as f64 / 8.0);
    let fixture = TempAudio::new("wav", &wav);
    let source = OpenedMediaSource::open_local(fixture.path_str(), None).expect("open source");
    let cancelled = Arc::new(AtomicBool::new(true));

    let result =
        StreamingDecoder::from_opened_source(source, Some(DecodeCancelToken::from_flag(cancelled)));

    assert!(matches!(result, Err(DecoderError::Canceled)));
}

#[cfg(feature = "http")]
#[test]
fn network_error_io_kind_classification_is_structured_and_total() {
    use std::io::ErrorKind;

    // Contract table: ErrorKind -> (variant, retriable). Retry semantics must
    // never depend on error message text.
    let retriable_kinds = [
        (ErrorKind::TimedOut, NetworkError::HttpTimeout),
        (ErrorKind::ConnectionReset, NetworkError::ConnectionReset),
        (ErrorKind::ConnectionAborted, NetworkError::ConnectionReset),
        (ErrorKind::BrokenPipe, NetworkError::ConnectionReset),
        (ErrorKind::UnexpectedEof, NetworkError::ConnectionReset),
    ];
    for (kind, expected) in retriable_kinds {
        let classified = NetworkError::from_io(std::io::Error::new(kind, "localized text"));
        assert_eq!(classified, expected, "kind {kind:?}");
        assert!(classified.is_retriable(), "kind {kind:?} must be retriable");
    }

    // Unclassified kinds degrade to the non-retried Other, never to a retry.
    for kind in [
        ErrorKind::NotFound,
        ErrorKind::PermissionDenied,
        ErrorKind::InvalidData,
        ErrorKind::Interrupted,
    ] {
        let classified = NetworkError::from_io(std::io::Error::new(kind, "detail"));
        assert!(
            matches!(classified, NetworkError::Other(_)),
            "kind {kind:?} must fall back to Other, got {classified:?}"
        );
        assert!(!classified.is_retriable());
    }

    let sensitive = NetworkError::from_io(std::io::Error::other(
        "wrapped request failed for https://example.test/audio?token=secret",
    ));
    let rendered = sensitive.to_string();
    assert!(!rendered.contains("example.test"));
    assert!(!rendered.contains("token=secret"));
    assert!(rendered.contains("Other"));
}

#[cfg(feature = "http")]
#[test]
fn network_error_cancelled_is_a_variant_not_a_message() {
    assert!(!NetworkError::Cancelled.is_retriable());

    // A hostile or coincidental message must not turn into a cancellation.
    let spoofed = NetworkError::Other("Decode cancelled".to_string());
    assert!(matches!(
        network_error_to_decoder_error(spoofed),
        DecoderError::Network(NetworkError::Other(_))
    ));
    assert!(matches!(
        network_error_to_decoder_error(NetworkError::Cancelled),
        DecoderError::Canceled
    ));
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

    let result = StreamingDecoder::open(local(fixture.path_str()));
    assert!(
        matches!(result, Err(DecoderError::UnsupportedFormat)),
        "garbage input should map to the typed UnsupportedFormat variant, got {:?}",
        result.err()
    );
}

#[test]
fn empty_input_yields_unsupported_format() {
    let fixture = TempAudio::new("wav", &[]);
    let result = StreamingDecoder::open(local(fixture.path_str()));
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

    let result = StreamingDecoder::open(local(fixture.path_str()));
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
        StreamingDecoder::open(local(fixture.path_str())).expect("open truncated wav header");
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
        let mut decoder = StreamingDecoder::open(local(fixture.path_str()))
            .unwrap_or_else(|e| panic!("open {sample_rate}Hz/{channels}ch wav: {e:?}"));

        // Metadata assertions.
        assert_eq!(decoder.info().sample_rate, sample_rate);
        assert_eq!(decoder.info().channels, channels);
        let total_frames = decoder
            .info()
            .total_frames
            .expect("total_frames known for wav");
        assert_eq!(total_frames, frames);
        let dur = decoder
            .info()
            .duration_secs
            .expect("duration known for wav");
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
    let mut decoder = StreamingDecoder::open(local(fixture.path_str())).expect("open wav");

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
    let mut baseline = StreamingDecoder::open(local(fixture.path_str())).expect("open wav");
    baseline.seek(1.0).expect("seek baseline");
    let baseline_first = baseline
        .decode_next()
        .expect("baseline decode")
        .expect("baseline samples");

    // Now inject a large synthetic encoder_delay before seeking.
    let mut decoder = StreamingDecoder::open(local(fixture.path_str())).expect("open wav");
    decoder.set_gapless_counters_for_test(5_000, 0); // far larger than one packet
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
    let mut decoder = StreamingDecoder::open(local(fixture.path_str())).expect("open wav");
    decoder.set_gapless_counters_for_test(delay, 0);
    let samples = decode_all_samples(&mut decoder);

    // With a start-of-stream delay of `delay` frames (mono => `delay` samples),
    // total output should be frames - delay.
    assert_eq!(
        samples.len() as u64,
        frames - delay as u64,
        "start-of-stream encoder_delay trimming regressed"
    );
}

#[test]
fn end_of_stream_trims_encoder_padding_once() {
    let sample_rate = 48_000u32;
    let frames = 5_000u64;
    let padding = 1_000u32;
    let bytes = synth_wav(sample_rate, 1, frames, |frame, _| {
        (frame as f64 / frames as f64) * 2.0 - 1.0
    });
    let fixture = TempAudio::new("wav", &bytes);
    let mut decoder = StreamingDecoder::open(local(fixture.path_str())).expect("open wav");
    decoder.set_gapless_counters_for_test(0, padding);

    let decoded = decode_all_samples(&mut decoder);
    assert_eq!(decoded.len() as u64, frames - padding as u64);
}

#[test]
fn media_location_http_constructor_validates_once() {
    for url in [
        "http://example.invalid/a.flac",
        "https://example.invalid/a.flac",
        "HTTP://example.invalid/a.flac",
        "HTTPS://example.invalid/a.flac",
        "HtTpS://example.invalid/a.flac",
    ] {
        let location = MediaLocation::http(url).expect("valid HTTP URL");
        assert_eq!(location.kind(), MediaLocationKind::Http);
        assert!(location.as_local_path().is_none());
        assert!(matches!(
            location.as_http().expect("HTTP variant").url().scheme(),
            "http" | "https"
        ));
    }

    assert!(matches!(
        MediaLocation::http("ftp://example.invalid/a.flac"),
        Err(MediaLocationError::UnsupportedScheme { .. })
    ));
    assert!(matches!(
        MediaLocation::http("not a URL"),
        Err(MediaLocationError::InvalidUrl(_))
    ));
}

#[test]
fn media_location_debug_redacts_http_secrets() {
    let location = MediaLocation::http(
        "https://basic-user:basic-password@example.test:8443/private/token.flac?signature=query-secret#fragment-secret",
    )
    .expect("valid HTTP URL");
    let rendered = format!("{location:?}");
    assert!(rendered.contains("https://example.test:8443"));
    for secret in [
        "basic-user",
        "basic-password",
        "private",
        "token.flac",
        "query-secret",
        "fragment-secret",
    ] {
        assert!(!rendered.contains(secret));
    }
}

#[cfg(not(feature = "http"))]
#[test]
fn http_url_without_the_http_feature_reports_the_missing_feature() {
    let location = MediaLocation::http("https://example.invalid/a.flac").expect("valid URL");
    let result = StreamingDecoder::open(location);

    assert!(
        matches!(
            result,
            Err(DecoderError::FeatureUnavailable {
                source_kind: "HTTP(S)",
                feature: "http",
            })
        ),
        "a disabled build feature must not be reported as a probe failure"
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_local_path_reaches_file_open_without_url_conversion() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let path = PathBuf::from(OsString::from_vec(b"missing-\xFF-audio.flac".to_vec()));
    let location = MediaLocation::local(path.clone());
    assert_eq!(location.as_local_path(), Some(path.as_path()));
    assert!(matches!(
        StreamingDecoder::open(location),
        Err(DecoderError::FileOpen(_))
    ));
}

#[cfg(windows)]
#[test]
fn non_utf8_local_path_reaches_file_open_without_url_conversion() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let path = PathBuf::from(OsString::from_wide(&[
        b'm' as u16,
        b'i' as u16,
        b's' as u16,
        b's' as u16,
        0xD800,
        b'.' as u16,
        b'f' as u16,
        b'l' as u16,
        b'a' as u16,
        b'c' as u16,
    ]));
    let location = MediaLocation::local(path.clone());
    assert_eq!(location.as_local_path(), Some(path.as_path()));
    assert!(matches!(
        StreamingDecoder::open(location),
        Err(DecoderError::FileOpen(_))
    ));
}
