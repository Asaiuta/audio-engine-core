use std::fs::File;
#[cfg(feature = "http")]
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;
#[cfg(feature = "http")]
use std::time::Duration;

use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;

#[cfg(feature = "http")]
use super::error::{network_error_to_decoder_error, with_network_retry, NetworkError};
use super::error::{DecodeCancelToken, DecoderError};

#[cfg(feature = "http")]
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(feature = "http")]
const HTTP_RANGE_STREAM_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(feature = "http")]
const HTTP_FULL_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
pub(super) const BYTES_PER_MIB: usize = 1024 * 1024;
pub(super) const F64_SAMPLE_BYTES: usize = std::mem::size_of::<f64>();
#[cfg(feature = "http")]
const NON_RANGE_DOWNLOAD_MEMORY_DIVISOR: usize = 8;
#[cfg(feature = "http")]
pub(super) const RANGE_PREFETCH: usize = 256 * 1024;

/// HTTP Basic authentication credentials for remote audio sources.
///
/// Ignored for local file paths. Only consulted by the HTTP source paths,
/// which require the `http` feature.
#[derive(Debug, Clone, Default)]
pub struct HttpCredentials {
    pub username: String,
    pub password: String,
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
) -> Result<(MediaSourceStream, Hint), DecoderError> {
    let path_str = path.to_string_lossy();
    if cancel_token
        .as_ref()
        .is_some_and(DecodeCancelToken::is_cancelled)
    {
        return Err(DecoderError::Canceled);
    }

    if path_str.starts_with("http://") || path_str.starts_with("https://") {
        #[cfg(feature = "http")]
        {
            open_http_media_source(path_str.as_ref(), credentials, cancel_token)
        }
        #[cfg(not(feature = "http"))]
        {
            let _ = credentials;
            Err(DecoderError::Probe(
                "HTTP sources require the `http` feature of audio-engine-core".to_string(),
            ))
        }
    } else {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        Ok((mss, hint))
    }
}

#[cfg(feature = "http")]
fn open_http_media_source(
    url: &str,
    credentials: Option<&HttpCredentials>,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<(MediaSourceStream, Hint), DecoderError> {
    let owned_creds = credentials.cloned();
    match RangeStream::new(url.to_string(), owned_creds, cancel_token.clone()) {
        Ok(stream) if stream.is_usable_range_stream() => {
            log::info!("HTTP URL supports Range requests, streaming: {}", url);
            let mss = MediaSourceStream::new(Box::new(stream), Default::default());
            Ok((mss, hint_from_url(url)))
        }
        Err(DecoderError::Canceled) => Err(DecoderError::Canceled),
        _ => {
            log::info!(
                "HTTP URL does not support Range, falling back to full download: {}",
                url
            );
            let cursor = download_full_source(url, credentials, cancel_token.as_ref())?;
            let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
            Ok((mss, hint_from_url(url)))
        }
    }
}

#[cfg(feature = "http")]
fn hint_from_url(url: &str) -> Hint {
    let mut hint = Hint::new();
    if let Some(ext) = url
        .split('?')
        .next()
        .and_then(|p| p.rsplit('.').next())
        .filter(|e| e.len() <= 5)
    {
        hint.with_extension(ext);
    }
    hint
}

#[cfg(feature = "http")]
fn download_full_source(
    url: &str,
    credentials: Option<&HttpCredentials>,
    cancel_token: Option<&DecodeCancelToken>,
) -> Result<Cursor<Vec<u8>>, DecoderError> {
    let (_, max_memory_bytes) = configured_decode_memory_limit();
    let max_download_bytes = max_memory_bytes / NON_RANGE_DOWNLOAD_MEMORY_DIVISOR;

    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_FULL_DOWNLOAD_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .build()
        .map_err(|e| {
            DecoderError::Network(NetworkError::Other(format!(
                "Failed to create HTTP client: {}",
                e
            )))
        })?;

    let content_length = with_network_retry("HTTP full-download HEAD", || {
        if cancel_token.is_some_and(DecodeCancelToken::is_cancelled) {
            return Err(NetworkError::Other("Decode cancelled".to_string()));
        }
        let mut head_req = client.head(url);
        if let Some(creds) = credentials {
            head_req = head_req.basic_auth(&creds.username, Some(&creds.password));
        }
        let response = head_req.send().map_err(NetworkError::from)?;
        if let Some(e) = response_network_error(&response) {
            return Err(e);
        }
        Ok(response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok()))
    })
    .map_err(network_error_to_decoder_error)?;

    if let Some(len) = content_length {
        checked_download_capacity(Some(len), max_download_bytes)?;
        log::info!(
            "Downloading {} MB file (server does not support Range)",
            len / BYTES_PER_MIB as u64
        );
    } else {
        log::warn!("Content-Length unknown, downloading without size check (may cause OOM)");
    }

    let response = with_network_retry("HTTP full-download GET", || {
        if cancel_token.is_some_and(DecodeCancelToken::is_cancelled) {
            return Err(NetworkError::Other("Decode cancelled".to_string()));
        }
        let mut req = client.get(url);
        if let Some(creds) = credentials {
            req = req.basic_auth(&creds.username, Some(&creds.password));
        }
        let response = req.send().map_err(NetworkError::from)?;
        if let Some(e) = response_network_error(&response) {
            return Err(e);
        }
        Ok(response)
    })
    .map_err(network_error_to_decoder_error)?;

    let download_capacity = checked_download_capacity(
        content_length.or(response.content_length()),
        max_download_bytes,
    )?;
    let mut stream = response;
    let mut buffer = Vec::with_capacity(download_capacity.unwrap_or(RANGE_PREFETCH));
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        if cancel_token.is_some_and(DecodeCancelToken::is_cancelled) {
            return Err(DecoderError::Canceled);
        }

        let n = stream
            .read(&mut chunk)
            .map_err(|e| DecoderError::Network(NetworkError::from_io(e)))?;
        if n == 0 {
            break;
        }

        buffer.extend_from_slice(&chunk[..n]);
        if buffer.len() > max_download_bytes {
            let actual_mb = bytes_to_mib(buffer.len());
            return Err(DecoderError::Network(NetworkError::Other(format!(
                "Downloaded file exceeds memory limit: {} MB (limit: {} MB)",
                actual_mb,
                bytes_to_mib(max_download_bytes)
            ))));
        }
    }

    log::debug!(
        "Downloaded {} bytes into buffer with initial capacity {}",
        buffer.len(),
        download_capacity.unwrap_or(RANGE_PREFETCH)
    );
    Ok(Cursor::new(buffer))
}

#[cfg(feature = "http")]
fn checked_download_capacity(
    content_length: Option<u64>,
    max_download_bytes: usize,
) -> Result<Option<usize>, DecoderError> {
    let Some(len) = content_length else {
        return Ok(None);
    };

    if len > max_download_bytes as u64 {
        let len_mb = len / BYTES_PER_MIB as u64;
        return Err(DecoderError::Network(NetworkError::Other(format!(
            "File too large for non-Range download: {} MB (limit: {} MB). \
             Server must support Range requests for files this size. \
             Increase DECODE_MAX_MEMORY_MB env var if needed.",
            len_mb,
            bytes_to_mib(max_download_bytes)
        ))));
    }

    Ok(Some(len as usize))
}

#[cfg(feature = "http")]
fn response_network_error(response: &reqwest::blocking::Response) -> Option<NetworkError> {
    let status = response.status();
    (!status.is_success() && status.as_u16() != 206)
        .then_some(NetworkError::HttpStatus(status.as_u16()))
}

#[cfg(feature = "http")]
pub(super) fn fetch_range_once(
    client: &reqwest::blocking::Client,
    url: &str,
    credentials: Option<&HttpCredentials>,
    start: u64,
    len: usize,
    cancel_token: Option<&DecodeCancelToken>,
) -> Result<Vec<u8>, NetworkError> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if cancel_token.is_some_and(DecodeCancelToken::is_cancelled) {
        return Err(NetworkError::Other("Decode cancelled".to_string()));
    }

    let end = start
        .checked_add(len as u64 - 1)
        .ok_or_else(|| NetworkError::Other("Range end overflow".into()))?;

    let mut req = client
        .get(url)
        .header("Range", format!("bytes={}-{}", start, end));
    if let Some(creds) = credentials {
        req = req.basic_auth(&creds.username, Some(&creds.password));
    }

    let response = req.send().map_err(NetworkError::from)?;
    if let Some(e) = response_network_error(&response) {
        return Err(e);
    }

    let bytes = response.bytes().map_err(NetworkError::from)?;
    if cancel_token.is_some_and(DecodeCancelToken::is_cancelled) {
        return Err(NetworkError::Other("Decode cancelled".to_string()));
    }

    Ok(bytes.to_vec())
}

#[cfg(feature = "http")]
struct RangeStream {
    url: String,
    credentials: Option<HttpCredentials>,
    client: reqwest::blocking::Client,
    buf: Vec<u8>,
    buf_start: u64,
    pos: u64,
    content_length: Option<u64>,
    supports_range: bool,
    cancel_token: Option<DecodeCancelToken>,
}

#[cfg(feature = "http")]
impl RangeStream {
    fn new(
        url: String,
        credentials: Option<HttpCredentials>,
        cancel_token: Option<DecodeCancelToken>,
    ) -> Result<Self, DecoderError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(HTTP_RANGE_STREAM_TIMEOUT)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .build()
            .map_err(|e| {
                DecoderError::Network(NetworkError::Other(format!(
                    "Failed to create HTTP client: {}",
                    e
                )))
            })?;

        let (content_length, supports_range) =
            with_network_retry("HTTP stream initialization", || {
                if cancel_token
                    .as_ref()
                    .is_some_and(DecodeCancelToken::is_cancelled)
                {
                    return Err(NetworkError::Other("Decode cancelled".to_string()));
                }

                let mut head_req = client.head(&url);
                if let Some(ref creds) = credentials {
                    head_req = head_req.basic_auth(&creds.username, Some(&creds.password));
                }

                let head_response = match head_req.send() {
                    Ok(response) => {
                        if let Some(e) = response_network_error(&response) {
                            return Err(e);
                        }
                        Some(response)
                    }
                    Err(e) => return Err(NetworkError::from(e)),
                };

                let mut content_length = head_response.as_ref().and_then(|r| {
                    r.headers()
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse().ok())
                });
                let mut supports_range = head_response
                    .as_ref()
                    .map(|r| {
                        r.headers()
                            .get("accept-ranges")
                            .and_then(|v| v.to_str().ok())
                            .map(|s| s == "bytes")
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);

                if !supports_range || content_length.is_none() {
                    if cancel_token
                        .as_ref()
                        .is_some_and(DecodeCancelToken::is_cancelled)
                    {
                        return Err(NetworkError::Other("Decode cancelled".to_string()));
                    }
                    let mut range_req = client.get(&url).header("Range", "bytes=0-0");
                    if let Some(ref creds) = credentials {
                        range_req = range_req.basic_auth(&creds.username, Some(&creds.password));
                    }
                    let range_response = range_req.send().map_err(NetworkError::from)?;
                    if let Some(e) = response_network_error(&range_response) {
                        return Err(e);
                    }
                    let range_status = range_response.status();
                    let range_content_total = range_response
                        .headers()
                        .get("content-range")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.split('/').next_back().and_then(|s| s.parse().ok()));
                    let range_content_length = range_response
                        .headers()
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse().ok());

                    if range_status.as_u16() == 206 || range_content_total.is_some() {
                        supports_range = true;
                        content_length = range_content_total.or(content_length);
                    } else if content_length.is_none() {
                        content_length = range_content_length;
                    }
                }

                Ok((content_length, supports_range))
            })
            .map_err(network_error_to_decoder_error)?;

        let initial_fetch_len = content_length
            .map(|len| RANGE_PREFETCH.min(len as usize))
            .unwrap_or(RANGE_PREFETCH);
        let initial_buf = if supports_range && initial_fetch_len > 0 {
            with_network_retry("HTTP stream initial range GET", || {
                fetch_range_once(
                    &client,
                    &url,
                    credentials.as_ref(),
                    0,
                    initial_fetch_len,
                    cancel_token.as_ref(),
                )
            })
            .map_err(network_error_to_decoder_error)?
        } else {
            Vec::new()
        };

        Ok(Self {
            url,
            credentials,
            client,
            buf: initial_buf,
            buf_start: 0,
            pos: 0,
            content_length,
            supports_range,
            cancel_token,
        })
    }

    fn is_usable_range_stream(&self) -> bool {
        self.supports_range && self.content_length.is_some()
    }

    fn fetch_range(&mut self, start: u64, len: usize) -> Result<Vec<u8>, DecoderError> {
        fetch_range_once(
            &self.client,
            &self.url,
            self.credentials.as_ref(),
            start,
            len,
            self.cancel_token.as_ref(),
        )
        .map_err(network_error_to_decoder_error)
    }

    fn ensure_buffered(&mut self, need: usize) -> std::io::Result<()> {
        let buf_end = self.buf_start + self.buf.len() as u64;
        if self.pos >= self.buf_start && self.pos + need as u64 <= buf_end {
            return Ok(());
        }

        let fetch_len = need.max(RANGE_PREFETCH);
        let fetch_len = if let Some(cl) = self.content_length {
            fetch_len.min((cl.saturating_sub(self.pos)) as usize)
        } else {
            fetch_len
        };
        if fetch_len == 0 {
            return Ok(());
        }
        let data = self
            .fetch_range(self.pos, fetch_len)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.buf_start = self.pos;
        self.buf = data;
        Ok(())
    }
}

#[cfg(feature = "http")]
impl Read for RangeStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self
            .cancel_token
            .as_ref()
            .is_some_and(DecodeCancelToken::is_cancelled)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Decode cancelled",
            ));
        }
        self.ensure_buffered(buf.len())?;
        let offset = (self.pos - self.buf_start) as usize;
        let available = self.buf.len().saturating_sub(offset);
        if available == 0 {
            return Ok(0);
        }
        let n = available.min(buf.len());
        buf[..n].copy_from_slice(&self.buf[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

#[cfg(feature = "http")]
impl Seek for RangeStream {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => {
                if let Some(len) = self.content_length {
                    len as i64 + d
                } else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "content-length unknown",
                    ));
                }
            }
        };
        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "negative seek",
            ));
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

#[cfg(feature = "http")]
impl symphonia::core::io::MediaSource for RangeStream {
    fn is_seekable(&self) -> bool {
        self.is_usable_range_stream()
    }

    fn byte_len(&self) -> Option<u64> {
        self.content_length
    }
}

#[cfg(all(test, feature = "http"))]
mod tests {
    use super::*;

    #[test]
    fn range_stream_requires_range_support_and_content_length() {
        let stream = RangeStream {
            url: "https://example.test/song.flac".to_string(),
            credentials: None,
            client: reqwest::blocking::Client::builder()
                .build()
                .expect("client fixture"),
            buf: Vec::new(),
            buf_start: 0,
            pos: 0,
            content_length: Some(1024),
            supports_range: true,
            cancel_token: None,
        };
        assert!(stream.is_usable_range_stream());

        let no_range = RangeStream {
            supports_range: false,
            ..stream
        };
        assert!(!no_range.is_usable_range_stream());

        let no_len = RangeStream {
            supports_range: true,
            content_length: None,
            ..no_range
        };
        assert!(!no_len.is_usable_range_stream());
    }
}
