use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use thiserror::Error;
use url::Url;

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

/// Errors returned while constructing a typed HTTP media location.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MediaLocationError {
    /// The input is not a syntactically valid URL.
    #[error("invalid media URL")]
    InvalidUrl(#[source] url::ParseError),
    /// The URL uses a scheme that this crate does not decode.
    #[error("unsupported media URL scheme: {scheme}")]
    UnsupportedScheme { scheme: String },
    /// An HTTP URL without a host cannot be fetched safely.
    #[error("HTTP media URL has no host")]
    MissingHost,
}

/// A validated HTTP(S) media URL.
///
/// The URL is kept private so callers cannot construct an invalid scheme or a
/// host-less HTTP location through a public enum variant. Use [`Self::parse`]
/// or [`Self::from_url`] to construct one.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct HttpMediaLocation {
    url: Url,
}

impl HttpMediaLocation {
    /// Parse and validate an HTTP(S) media URL.
    pub fn parse(input: impl AsRef<str>) -> Result<Self, MediaLocationError> {
        let url = Url::parse(input.as_ref()).map_err(MediaLocationError::InvalidUrl)?;
        Self::from_url(url)
    }

    /// Validate an already parsed URL.
    pub fn from_url(url: Url) -> Result<Self, MediaLocationError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(MediaLocationError::UnsupportedScheme {
                scheme: url.scheme().to_string(),
            });
        }
        if url.host_str().is_none() {
            return Err(MediaLocationError::MissingHost);
        }
        Ok(Self { url })
    }

    /// Return the validated request URL.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Return an origin-only identity suitable for logs and diagnostics.
    pub fn log_identity(&self) -> String {
        self.url.origin().ascii_serialization()
    }
}

impl fmt::Debug for HttpMediaLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("HttpMediaLocation")
            .field(&self.log_identity())
            .finish()
    }
}

impl fmt::Display for HttpMediaLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.log_identity())
    }
}

/// An owned local or validated HTTP(S) media source.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MediaLocation {
    /// A local filesystem path. The owned `PathBuf` preserves non-UTF-8 paths.
    Local(PathBuf),
    /// A validated HTTP(S) URL.
    Http(HttpMediaLocation),
}

impl MediaLocation {
    /// Construct a local media location without filesystem I/O.
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::Local(path.into())
    }

    /// Parse and construct an HTTP(S) media location.
    pub fn http(input: impl AsRef<str>) -> Result<Self, MediaLocationError> {
        HttpMediaLocation::parse(input).map(Self::Http)
    }

    /// Return the location kind without exposing its representation.
    pub fn kind(&self) -> MediaLocationKind {
        match self {
            Self::Local(_) => MediaLocationKind::Local,
            Self::Http(_) => MediaLocationKind::Http,
        }
    }

    /// Borrow the local path when this is a local location.
    pub fn as_local_path(&self) -> Option<&Path> {
        match self {
            Self::Local(path) => Some(path),
            Self::Http(_) => None,
        }
    }

    /// Borrow the validated HTTP URL when this is an HTTP location.
    pub fn as_http(&self) -> Option<&HttpMediaLocation> {
        match self {
            Self::Local(_) => None,
            Self::Http(url) => Some(url),
        }
    }

    /// Return a safe identity for logs and diagnostics.
    pub fn log_identity(&self) -> String {
        match self {
            Self::Local(path) => path.to_string_lossy().into_owned(),
            Self::Http(url) => url.log_identity(),
        }
    }
}

impl fmt::Debug for MediaLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(path) => formatter.debug_tuple("Local").field(path).finish(),
            Self::Http(url) => formatter.debug_tuple("Http").field(url).finish(),
        }
    }
}

impl fmt::Display for MediaLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.log_identity())
    }
}

/// The stable source namespace used by cache and diagnostics layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum MediaLocationKind {
    Local,
    Http,
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
    pub fn open_with_credentials_and_cancel(
        location: MediaLocation,
        credentials: Option<&HttpCredentials>,
        cancel_token: Option<DecodeCancelToken>,
    ) -> Result<Self, DecoderError> {
        let (stream, hint) = open_media_source(&location, credentials, cancel_token)?;
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
    location: &MediaLocation,
    credentials: Option<&HttpCredentials>,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<(MediaSourceStream<'static>, Hint), DecoderError> {
    if cancel_token
        .as_ref()
        .is_some_and(DecodeCancelToken::is_cancelled)
    {
        return Err(DecoderError::Canceled);
    }

    match location {
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
