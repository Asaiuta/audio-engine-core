use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[cfg(feature = "http")]
use super::{source::fetch_range_once, NetworkError};
use super::{DecodeCancelToken, DecoderError, StreamingDecoder};

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
