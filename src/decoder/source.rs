use std::fmt;
use std::fs::File;
use std::path::Path;

use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;

use super::error::{DecodeCancelToken, DecoderError};

#[cfg(feature = "http")]
mod http;

pub(super) const BYTES_PER_MIB: usize = 1024 * 1024;

/// HTTP Basic authentication credentials for remote audio sources.
///
/// Ignored for local file paths. Only consulted by the HTTP source paths,
/// which require the `http` feature.
#[derive(Clone, Default)]
pub struct HttpCredentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for HttpCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpCredentials")
            .field("username", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// An opened decode source that has not yet been probed or bound to a codec.
///
/// Keeping the Symphonia transport fields private lets callers separate source
/// opening from decoder construction without depending on Symphonia's API.
pub struct OpenedMediaSource {
    pub(super) stream: MediaSourceStream<'static>,
    pub(super) hint: Hint,
}

/// Where a decode source lives, resolved once from the caller's path-like input.
///
/// Callers still pass a `Path`, because the public open APIs accept one, but the
/// remote/local decision is made in exactly one place instead of being
/// re-derived from string prefixes at each call site. That also keeps the URL
/// text in one branch, which is where redaction and cache identity belong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MediaLocation<'a> {
    /// A local filesystem path. Kept as a `Path` so a non-UTF-8 path opens
    /// byte-exactly rather than through a lossy string.
    Local(&'a Path),
    /// An `http`/`https` URL, in its original spelling.
    Http(&'a str),
}

/// Schemes routed to the HTTP source. RFC 3986 defines schemes as
/// case-insensitive, so `HTTPS://host/track.flac` is a URL, not a relative
/// directory named `HTTPS:`.
const HTTP_SCHEMES: [&str; 2] = ["http://", "https://"];

impl<'a> MediaLocation<'a> {
    /// Classify a caller-supplied path.
    ///
    /// `path_text` is the caller's path rendered as UTF-8. A path that is not
    /// valid UTF-8 cannot be a URL, so a lossy rendering can only ever be
    /// classified as local, and the original `Path` is what gets opened.
    pub(super) fn classify(path: &'a Path, path_text: &'a str) -> Self {
        if HTTP_SCHEMES.iter().any(|scheme| {
            path_text.len() >= scheme.len()
                && path_text[..scheme.len()].eq_ignore_ascii_case(scheme)
        }) {
            Self::Http(path_text)
        } else {
            Self::Local(path)
        }
    }
}

/// Open a local file and derive its extension hint.
///
/// Single owner of the local branch, so the staged `open_local` entry point and
/// the general `open_media_source` router cannot drift apart.
fn open_local_media_source(
    path: &Path,
) -> Result<(MediaSourceStream<'static>, Hint), DecoderError> {
    let file = File::open(path)?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }
    Ok((stream, hint))
}

impl OpenedMediaSource {
    /// Open a local or HTTP media source without probing or constructing a decoder.
    ///
    /// This lets a player reserve decoder memory after source opening but before
    /// decoder construction while preserving Range transport, credentials, and
    /// cancellation state in the returned source.
    pub fn open_path_with_credentials_and_cancel<P: AsRef<Path>>(
        path: P,
        credentials: Option<&HttpCredentials>,
        cancel_token: Option<DecodeCancelToken>,
    ) -> Result<Self, DecoderError> {
        let (stream, hint) = open_media_source(path.as_ref(), credentials, cancel_token)?;
        Ok(Self { stream, hint })
    }

    /// Open a local file once and retain its extension hint for later probing.
    pub fn open_local<P: AsRef<Path>>(
        path: P,
        cancel_token: Option<DecodeCancelToken>,
    ) -> Result<Self, DecoderError> {
        if cancel_token
            .as_ref()
            .is_some_and(DecodeCancelToken::is_cancelled)
        {
            return Err(DecoderError::Canceled);
        }

        let (stream, hint) = open_local_media_source(path.as_ref())?;
        Ok(Self { stream, hint })
    }
}

pub(super) fn configured_decode_memory_limit() -> (usize, usize) {
    let budget = crate::diagnostics::decode_memory_budget();
    (budget.limit_mb, budget.limit_bytes)
}

pub(super) fn bytes_to_mib(bytes: usize) -> usize {
    bytes / BYTES_PER_MIB
}

pub(super) fn open_media_source(
    path: &Path,
    credentials: Option<&HttpCredentials>,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<(MediaSourceStream<'static>, Hint), DecoderError> {
    let path_text = path.to_string_lossy();
    if cancel_token
        .as_ref()
        .is_some_and(DecodeCancelToken::is_cancelled)
    {
        return Err(DecoderError::Canceled);
    }

    match MediaLocation::classify(path, path_text.as_ref()) {
        MediaLocation::Http(url) => {
            #[cfg(feature = "http")]
            {
                http::open_http_media_source(url, credentials, cancel_token)
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = (url, credentials);
                Err(DecoderError::FeatureUnavailable {
                    source_kind: "HTTP(S)",
                    feature: "http",
                })
            }
        }
        MediaLocation::Local(path) => open_local_media_source(path),
    }
}
