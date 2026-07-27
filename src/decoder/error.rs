use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(feature = "http")]
use std::time::Duration;

use thiserror::Error;

#[cfg(feature = "http")]
const NETWORK_MAX_ATTEMPTS: usize = 3;
#[cfg(feature = "http")]
const NETWORK_BACKOFF_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];

/// Classification of network failures encountered while decoding from an
/// HTTP(S) source.
///
/// Only available with the `http` feature.
///
/// Marked `#[non_exhaustive]`: transport classification may gain variants as
/// the structured error mapping improves, without a breaking release. Match
/// with a `_` arm and treat unknown variants as non-retriable.
#[cfg(feature = "http")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetworkError {
    /// The request exceeded its configured timeout.
    HttpTimeout,
    /// The connection was reset by the peer mid-transfer.
    ConnectionReset,
    /// The server returned a non-success status code.
    HttpStatus(u16),
    /// DNS resolution failed for the request host.
    DnsFailure(String),
    /// The TLS handshake or certificate validation failed.
    TlsError(String),
    /// The operation was cancelled via a [`DecodeCancelToken`] before or
    /// during the network request. Never retried.
    Cancelled,
    /// Any other transport error, carrying its description.
    Other(String),
}

#[cfg(feature = "http")]
impl NetworkError {
    /// Returns `true` when retrying the operation may succeed (transient
    /// timeouts, connection resets, and 408/429/5xx statuses).
    pub fn is_retriable(&self) -> bool {
        match self {
            NetworkError::HttpTimeout | NetworkError::ConnectionReset => true,
            NetworkError::HttpStatus(status) => matches!(status, 408 | 429 | 500..=504),
            NetworkError::DnsFailure(_)
            | NetworkError::TlsError(_)
            | NetworkError::Cancelled
            | NetworkError::Other(_) => false,
        }
    }

    pub(super) fn from_io(e: std::io::Error) -> Self {
        match classify_io_error_kind(e.kind()) {
            Some(classified) => classified,
            None => NetworkError::Other(e.to_string()),
        }
    }
}

/// Structured transport classification from a stable [`std::io::ErrorKind`].
///
/// This is the preferred classification path: it does not depend on any error
/// message text, so dependency upgrades or localized OS messages cannot
/// silently change retry semantics.
#[cfg(feature = "http")]
fn classify_io_error_kind(kind: std::io::ErrorKind) -> Option<NetworkError> {
    match kind {
        std::io::ErrorKind::TimedOut => Some(NetworkError::HttpTimeout),
        std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::UnexpectedEof => Some(NetworkError::ConnectionReset),
        _ => None,
    }
}

#[cfg(feature = "http")]
pub(super) fn network_error_to_decoder_error(error: NetworkError) -> DecoderError {
    if error == NetworkError::Cancelled {
        DecoderError::Canceled
    } else {
        DecoderError::Network(error)
    }
}

#[cfg(feature = "http")]
impl From<reqwest::Error> for NetworkError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return NetworkError::HttpTimeout;
        }
        if let Some(status) = e.status() {
            return NetworkError::HttpStatus(status.as_u16());
        }

        // Walk the error source chain looking for a structured io::Error
        // before falling back to message heuristics. reqwest wraps hyper,
        // which wraps io::Error for transport-level failures.
        let mut source = std::error::Error::source(&e);
        while let Some(inner) = source {
            if let Some(io) = inner.downcast_ref::<std::io::Error>() {
                if let Some(classified) = classify_io_error_kind(io.kind()) {
                    return classified;
                }
            }
            source = inner.source();
        }

        classify_reqwest_error_by_message(&e)
    }
}

/// Last-resort classification from the rendered error text.
///
/// Only reached when no io::Error in the source chain carried a stable
/// [`std::io::ErrorKind`]. This depends on reqwest/hyper message wording and
/// may miss on dependency upgrades; misses degrade to the non-retried
/// [`NetworkError::Other`], never to a spurious retry.
#[cfg(feature = "http")]
fn classify_reqwest_error_by_message(e: &reqwest::Error) -> NetworkError {
    let text = e.to_string();
    let lower = text.to_ascii_lowercase();
    if lower.contains("connection reset") {
        NetworkError::ConnectionReset
    } else if e.is_connect() && (lower.contains("dns") || lower.contains("resolve")) {
        NetworkError::DnsFailure(text)
    } else if lower.contains("tls") || lower.contains("certificate") {
        NetworkError::TlsError(text)
    } else {
        NetworkError::Other(text)
    }
}

#[cfg(feature = "http")]
impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkError::HttpTimeout => write!(f, "HTTP timeout"),
            NetworkError::ConnectionReset => write!(f, "connection reset"),
            NetworkError::HttpStatus(status) => write!(f, "HTTP status {}", status),
            NetworkError::DnsFailure(e) => write!(f, "DNS failure: {}", e),
            NetworkError::TlsError(e) => write!(f, "TLS error: {}", e),
            NetworkError::Cancelled => write!(f, "Decode cancelled"),
            NetworkError::Other(e) => write!(f, "{}", e),
        }
    }
}

/// Cooperative cancellation handle shared with a running decode operation.
#[derive(Clone, Debug)]
pub struct DecodeCancelToken {
    cancelled: Arc<AtomicBool>,
}

impl DecodeCancelToken {
    /// Wrap a shared cancellation flag for use with the decoder open/decode
    /// entry points.
    pub fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self { cancelled }
    }

    /// Returns `true` once the underlying flag has been set, signalling that
    /// the decode should stop as soon as possible.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Errors returned by the streaming decoder.
#[derive(Error, Debug)]
pub enum DecoderError {
    /// A local file could not be opened.
    #[error("Failed to open file: {0}")]
    FileOpen(#[from] std::io::Error),
    /// An HTTP(S) source failed. Only constructible with the `http` feature.
    #[cfg(feature = "http")]
    #[error("Network error: {0}")]
    Network(NetworkError),
    /// The container format is not supported.
    #[error("Unsupported format")]
    UnsupportedFormat,
    /// No decodable audio track was found in the container.
    #[error("No audio track found")]
    NoAudioTrack,
    /// The codec failed to decode a packet.
    #[error("Decoder error: {0}")]
    Decoder(String),
    /// Format probing failed.
    #[error("Probe error: {0}")]
    Probe(String),
    /// The decode was cancelled via a [`DecodeCancelToken`].
    #[error("Decode cancelled")]
    Canceled,
}

#[cfg(feature = "http")]
// `attempt` indexes NETWORK_BACKOFF_DELAYS, a different array than the loop range.
#[allow(clippy::needless_range_loop)]
pub(super) fn with_network_retry<T, F>(operation_name: &str, mut op: F) -> Result<T, NetworkError>
where
    F: FnMut() -> Result<T, NetworkError>,
{
    for attempt in 0..NETWORK_MAX_ATTEMPTS {
        match op() {
            Ok(value) => return Ok(value),
            Err(e) if e.is_retriable() && attempt < NETWORK_BACKOFF_DELAYS.len() => {
                let delay = NETWORK_BACKOFF_DELAYS[attempt];
                log::warn!(
                    "{} attempt {} failed ({}), retrying in {:?}",
                    operation_name,
                    attempt + 1,
                    e,
                    delay
                );
                std::thread::sleep(delay);
            }
            Err(e) => return Err(e),
        }
    }

    unreachable!("network retry loop returns on success or final error")
}
