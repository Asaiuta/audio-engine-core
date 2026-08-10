use std::fmt;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};
#[cfg(not(test))]
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{HeaderMap, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use reqwest::redirect;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use url::Url;

#[cfg(not(test))]
use std::sync::Arc;

use super::{
    bytes_to_mib, configured_decode_memory_limit, DecodeCancelToken, DecoderError, HttpCredentials,
    HttpMediaLocation, BYTES_PER_MIB,
};
use crate::decoder::error::{network_error_to_decoder_error, with_network_retry, NetworkError};

const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_RANGE_STREAM_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_FULL_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const NON_RANGE_DOWNLOAD_MEMORY_DIVISOR: usize = 8;
const RANGE_PREFETCH: usize = 256 * 1024;
const READ_CHUNK_SIZE: usize = 64 * 1024;

pub(super) fn open_http_media_source(
    location: &HttpMediaLocation,
    credentials: Option<&HttpCredentials>,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<(MediaSourceStream<'static>, Hint), DecoderError> {
    let url = location.url();
    let log_identity = location.log_identity();
    match RangeStream::new(url.clone(), credentials.cloned(), cancel_token.clone()) {
        Ok(stream) => {
            log::info!(
                "HTTP origin supports Range requests, streaming: {}",
                log_identity
            );
            let hint = hint_from_url_and_content_type(url, stream.content_type.as_deref());
            let stream = MediaSourceStream::new(Box::new(stream), Default::default());
            Ok((stream, hint))
        }
        Err(NetworkError::RangeNotSupported { .. } | NetworkError::InvalidRangeResponse(_)) => {
            log::info!(
                "HTTP URL does not support valid Range responses, falling back to full download: {}",
                log_identity
            );
            let (cursor, content_type) =
                download_full_source(url, credentials, cancel_token.as_ref())?;
            let hint = hint_from_url_and_content_type(url, content_type.as_deref());
            let stream = MediaSourceStream::new(Box::new(cursor), Default::default());
            Ok((stream, hint))
        }
        Err(error) => Err(network_error_to_decoder_error(error)),
    }
}

fn remote_address_error(detail: impl Into<String>) -> NetworkError {
    NetworkError::Other(format!(
        "remote address rejected by policy: {}",
        detail.into()
    ))
}

fn is_disallowed_ipv4(address: std::net::Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
        || address.is_multicast()
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        || octets[0] >= 240
}

fn is_disallowed_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_disallowed_ipv4(address),
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_multicast()
                || address.segments()[0] == 0x2001 && address.segments()[1] == 0x0db8
                || address.to_ipv4().is_some_and(is_disallowed_ipv4)
        }
    }
}

fn validate_ip(address: IpAddr) -> Result<(), NetworkError> {
    if is_disallowed_ip(address) {
        Err(remote_address_error(format!("{address}")))
    } else {
        Ok(())
    }
}

fn resolve_and_validate_host(host: &str, port: u16) -> Result<Vec<SocketAddr>, NetworkError> {
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| NetworkError::DnsFailure(format!("lookup failed ({:?})", error.kind())))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(NetworkError::DnsFailure(
            "lookup returned no addresses".to_string(),
        ));
    }
    for address in &addresses {
        validate_ip(address.ip())?;
    }
    Ok(addresses)
}

fn validate_literal_host(url: &reqwest::Url) -> Result<(), NetworkError> {
    let host = url
        .host_str()
        .ok_or_else(|| remote_address_error("URL has no host"))?;
    if let Ok(address) = host.parse::<IpAddr>() {
        validate_ip(address)?;
    }
    Ok(())
}

fn validate_redirect_target(url: &reqwest::Url) -> Result<(), NetworkError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(remote_address_error("redirect uses a non-HTTP scheme"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| remote_address_error("redirect has no host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| remote_address_error("redirect has no supported port"))?;
    resolve_and_validate_host(host, port).map(|_| ())
}

#[derive(Debug)]
struct RedirectAddressRejected;

impl fmt::Display for RedirectAddressRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("audio-engine-core: remote address rejected by policy")
    }
}

impl std::error::Error for RedirectAddressRejected {}

fn redirect_policy() -> redirect::Policy {
    redirect::Policy::custom(|attempt| {
        if validate_redirect_target(attempt.url()).is_ok() {
            attempt.follow()
        } else {
            attempt.error(RedirectAddressRejected)
        }
    })
}

#[cfg(not(test))]
struct RejectPrivateResolver;

#[cfg(not(test))]
impl Resolve for RejectPrivateResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(std::future::ready(
            resolve_and_validate_host(&host, 0)
                .map(|addresses| Box::new(addresses.into_iter()) as Addrs)
                .map_err(|error| {
                    Box::new(std::io::Error::other(error.to_string()))
                        as Box<dyn std::error::Error + Send + Sync>
                }),
        ))
    }
}

fn build_client(timeout: Duration) -> Result<Client, NetworkError> {
    let builder = Client::builder()
        .timeout(timeout)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .redirect(redirect_policy());
    #[cfg(not(test))]
    let builder = builder.dns_resolver(Arc::new(RejectPrivateResolver));
    builder
        .build()
        .map_err(|error| NetworkError::Other(format!("Failed to create HTTP client: {error}")))
}

fn authenticated_get(
    client: &Client,
    url: &Url,
    credentials: Option<&HttpCredentials>,
) -> Result<RequestBuilder, NetworkError> {
    validate_literal_host(url)?;
    let request = client.get(url.clone());
    if let Some(credentials) = credentials {
        Ok(request.basic_auth(&credentials.username, Some(&credentials.password)))
    } else {
        Ok(request)
    }
}

fn response_status_error(response: &Response) -> Option<NetworkError> {
    let status = response.status();
    (!status.is_success()).then_some(NetworkError::HttpStatus(status.as_u16()))
}

fn download_full_source(
    url: &Url,
    credentials: Option<&HttpCredentials>,
    cancel_token: Option<&DecodeCancelToken>,
) -> Result<(Cursor<Vec<u8>>, Option<String>), DecoderError> {
    let (_, max_memory_bytes) = configured_decode_memory_limit();
    let max_download_bytes = max_memory_bytes / NON_RANGE_DOWNLOAD_MEMORY_DIVISOR;
    let client =
        build_client(HTTP_FULL_DOWNLOAD_TIMEOUT).map_err(network_error_to_decoder_error)?;

    let response = with_network_retry("HTTP full-download GET", || {
        check_cancelled(cancel_token)?;
        let response = authenticated_get(&client, url, credentials)?
            .send()
            .map_err(NetworkError::from)?;
        if let Some(error) = response_status_error(&response) {
            return Err(error);
        }
        Ok(response)
    })
    .map_err(network_error_to_decoder_error)?;

    let content_length = response.content_length();
    let download_capacity = checked_download_capacity(content_length, max_download_bytes)?;
    if let Some(length) = content_length {
        log::info!(
            "Downloading {} MB file (server does not support Range)",
            length / BYTES_PER_MIB as u64
        );
    } else {
        log::warn!("Content-Length unknown; enforcing the download memory limit while reading");
    }

    let content_type = header_string(response.headers(), CONTENT_TYPE);
    let mut stream = response;
    let mut buffer = Vec::with_capacity(download_capacity.unwrap_or(RANGE_PREFETCH));
    let mut chunk = [0_u8; READ_CHUNK_SIZE];
    loop {
        check_cancelled(cancel_token).map_err(network_error_to_decoder_error)?;
        let read = stream
            .read(&mut chunk)
            .map_err(NetworkError::from_io)
            .map_err(network_error_to_decoder_error)?;
        if read == 0 {
            break;
        }
        if buffer.len().saturating_add(read) > max_download_bytes {
            return Err(DecoderError::Network(NetworkError::Other(format!(
                "Downloaded file exceeds memory limit: more than {} MB",
                bytes_to_mib(max_download_bytes)
            ))));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    log::debug!(
        "Downloaded {} bytes into buffer with initial capacity {}",
        buffer.len(),
        download_capacity.unwrap_or(RANGE_PREFETCH)
    );
    Ok((Cursor::new(buffer), content_type))
}

fn checked_download_capacity(
    content_length: Option<u64>,
    max_download_bytes: usize,
) -> Result<Option<usize>, DecoderError> {
    let Some(length) = content_length else {
        return Ok(None);
    };
    if length > max_download_bytes as u64 {
        return Err(DecoderError::Network(NetworkError::Other(format!(
            "File too large for non-Range download: {} MB (limit: {} MB). Server must support Range requests for files this size. Increase DECODE_MAX_MEMORY_MB env var if needed.",
            length / BYTES_PER_MIB as u64,
            bytes_to_mib(max_download_bytes)
        ))));
    }
    Ok(Some(length as usize))
}

#[derive(Debug, PartialEq, Eq)]
struct ContentRange {
    start: u64,
    end: u64,
    total: u64,
}

impl ContentRange {
    fn parse(value: &str) -> Result<Self, NetworkError> {
        let value = value.trim();
        let remainder = value.strip_prefix("bytes ").ok_or_else(|| {
            invalid_range("Content-Range uses an unsupported unit or syntax".to_string())
        })?;
        let (interval, total) = remainder
            .split_once('/')
            .ok_or_else(|| invalid_range("Content-Range is missing `/total`".to_string()))?;
        if total.contains('/') {
            return Err(invalid_range(
                "Content-Range contains multiple totals".to_string(),
            ));
        }
        let (start, end) = interval
            .split_once('-')
            .ok_or_else(|| invalid_range("Content-Range is missing `start-end`".to_string()))?;
        let start = parse_range_number("start", start)?;
        let end = parse_range_number("end", end)?;
        let total = parse_range_number("total", total)?;
        if start > end {
            return Err(invalid_range(format!(
                "Content-Range start {start} exceeds end {end}"
            )));
        }
        if end >= total {
            return Err(invalid_range(format!(
                "Content-Range end {end} is outside total length {total}"
            )));
        }
        Ok(Self { start, end, total })
    }

    fn validate(
        &self,
        requested_start: u64,
        requested_end: u64,
        expected_total: Option<u64>,
    ) -> Result<(), NetworkError> {
        if self.start != requested_start || self.end != requested_end {
            return Err(invalid_range(format!(
                "returned interval {}-{} does not match requested {requested_start}-{requested_end}",
                self.start, self.end
            )));
        }
        if let Some(expected_total) = expected_total {
            if self.total != expected_total {
                return Err(invalid_range(format!(
                    "returned total {} does not match expected {expected_total}",
                    self.total
                )));
            }
        }
        Ok(())
    }
}

fn parse_range_number(field: &str, value: &str) -> Result<u64, NetworkError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_range(format!(
            "Content-Range {field} is not an unsigned integer"
        )));
    }
    value.parse().map_err(|_| {
        invalid_range(format!(
            "Content-Range {field} exceeds the supported integer range"
        ))
    })
}

fn invalid_range(message: String) -> NetworkError {
    NetworkError::InvalidRangeResponse(message)
}

struct RangeFetch {
    body: Vec<u8>,
    total: u64,
    content_type: Option<String>,
}

fn fetch_range_once(
    client: &Client,
    url: &Url,
    credentials: Option<&HttpCredentials>,
    start: u64,
    len: usize,
    expected_total: Option<u64>,
    cancel_token: Option<&DecodeCancelToken>,
) -> Result<RangeFetch, NetworkError> {
    if len == 0 {
        return Err(invalid_range("zero-length Range request".to_string()));
    }
    check_cancelled(cancel_token)?;
    let requested_end = start
        .checked_add(len as u64 - 1)
        .ok_or_else(|| invalid_range("requested Range end overflow".to_string()))?;

    let response = authenticated_get(client, url, credentials)?
        .header(RANGE, format!("bytes={start}-{requested_end}"))
        .send()
        .map_err(NetworkError::from)?;
    let status = response.status();
    if status.as_u16() != 206 {
        return if status.is_success() {
            Err(NetworkError::RangeNotSupported {
                status: status.as_u16(),
            })
        } else {
            Err(NetworkError::HttpStatus(status.as_u16()))
        };
    }

    let content_range = required_header(response.headers(), CONTENT_RANGE, "Content-Range")?;
    let content_range = ContentRange::parse(content_range)?;
    content_range.validate(start, requested_end, expected_total)?;
    validate_content_length(response.headers(), len)?;
    let content_type = header_string(response.headers(), CONTENT_TYPE);
    let body = read_bounded_body(response, len, cancel_token)?;
    Ok(RangeFetch {
        body,
        total: content_range.total,
        content_type,
    })
}

fn required_header<'a>(
    headers: &'a HeaderMap,
    name: reqwest::header::HeaderName,
    display_name: &str,
) -> Result<&'a str, NetworkError> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .ok_or_else(|| invalid_range(format!("missing {display_name} header")))?;
    if values.next().is_some() {
        return Err(invalid_range(format!(
            "multiple {display_name} headers are not allowed"
        )));
    }
    value
        .to_str()
        .map_err(|_| invalid_range(format!("{display_name} is not valid ASCII")))
}

fn validate_content_length(headers: &HeaderMap, expected_len: usize) -> Result<(), NetworkError> {
    let mut values = headers.get_all(CONTENT_LENGTH).iter();
    let Some(value) = values.next() else {
        return Ok(());
    };
    if values.next().is_some() {
        return Err(invalid_range(
            "multiple Content-Length headers are not allowed".to_string(),
        ));
    }
    let value = value
        .to_str()
        .map_err(|_| invalid_range("Content-Length is not valid ASCII".to_string()))?;
    let length = parse_range_number("Content-Length", value)?;
    if length != expected_len as u64 {
        return Err(invalid_range(format!(
            "Content-Length {length} does not match Range length {expected_len}"
        )));
    }
    Ok(())
}

fn read_bounded_body(
    response: Response,
    expected_len: usize,
    cancel_token: Option<&DecodeCancelToken>,
) -> Result<Vec<u8>, NetworkError> {
    let read_limit = (expected_len as u64)
        .checked_add(1)
        .ok_or_else(|| invalid_range("Range body limit overflow".to_string()))?;
    let mut stream = response.take(read_limit);
    let mut body = Vec::with_capacity(expected_len);
    let mut chunk = [0_u8; READ_CHUNK_SIZE];
    loop {
        check_cancelled(cancel_token)?;
        let read = stream.read(&mut chunk).map_err(|error| {
            invalid_range(format!(
                "failed while reading Range body ({:?})",
                error.kind()
            ))
        })?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
        if body.len() > expected_len {
            return Err(invalid_range(format!(
                "Range body exceeds declared interval length {expected_len}"
            )));
        }
    }
    if body.len() != expected_len {
        return Err(invalid_range(format!(
            "Range body length {} does not match declared interval length {expected_len}",
            body.len()
        )));
    }
    check_cancelled(cancel_token)?;
    Ok(body)
}

fn check_cancelled(cancel_token: Option<&DecodeCancelToken>) -> Result<(), NetworkError> {
    if cancel_token.is_some_and(DecodeCancelToken::is_cancelled) {
        Err(NetworkError::Cancelled)
    } else {
        Ok(())
    }
}

fn header_string(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn probe_mime_essence(content_type: &str) -> Option<&str> {
    let essence = content_type.split(';').next().unwrap_or("").trim();
    if essence.is_empty()
        || essence.eq_ignore_ascii_case("application/octet-stream")
        || essence.eq_ignore_ascii_case("binary/octet-stream")
        || essence.eq_ignore_ascii_case("text/plain")
    {
        None
    } else {
        Some(essence)
    }
}

fn hint_from_url_and_content_type(url: &Url, content_type: Option<&str>) -> Hint {
    let mut hint = hint_from_url(url);
    if let Some(mime) = content_type.and_then(probe_mime_essence) {
        hint.mime_type(mime);
    }
    hint
}

fn hint_from_url(url: &Url) -> Hint {
    let mut hint = Hint::new();
    if let Some(extension) = url
        .path_segments()
        .and_then(Iterator::last)
        .and_then(|name| name.rsplit_once('.').map(|(_, extension)| extension))
        .filter(|extension| extension.len() <= 5)
    {
        hint.with_extension(extension);
    }
    hint
}

struct RangeStream {
    url: Url,
    credentials: Option<HttpCredentials>,
    client: Client,
    buffer: Vec<u8>,
    buffer_start: u64,
    position: u64,
    content_length: u64,
    content_type: Option<String>,
    cancel_token: Option<DecodeCancelToken>,
}

impl RangeStream {
    fn new(
        url: Url,
        credentials: Option<HttpCredentials>,
        cancel_token: Option<DecodeCancelToken>,
    ) -> Result<Self, NetworkError> {
        let client = build_client(HTTP_RANGE_STREAM_TIMEOUT)?;
        let probe = with_network_retry("HTTP Range capability probe", || {
            fetch_range_once(
                &client,
                &url,
                credentials.as_ref(),
                0,
                1,
                None,
                cancel_token.as_ref(),
            )
        })?;
        let RangeFetch {
            body: probe_body,
            total: content_length,
            content_type: probe_content_type,
        } = probe;
        let initial_len = RANGE_PREFETCH.min(usize::try_from(content_length).unwrap_or(usize::MAX));
        let (buffer, content_type) = if initial_len == 1 {
            (probe_body, probe_content_type)
        } else {
            let initial = with_network_retry("HTTP stream initial Range GET", || {
                fetch_range_once(
                    &client,
                    &url,
                    credentials.as_ref(),
                    0,
                    initial_len,
                    Some(content_length),
                    cancel_token.as_ref(),
                )
            })?;
            (initial.body, initial.content_type.or(probe_content_type))
        };

        Ok(Self {
            url,
            credentials,
            client,
            buffer,
            buffer_start: 0,
            position: 0,
            content_length,
            content_type,
            cancel_token,
        })
    }

    fn ensure_buffered(&mut self, needed: usize) -> std::io::Result<()> {
        let buffer_end = self.buffer_start + self.buffer.len() as u64;
        let requested_end = self.position.saturating_add(needed as u64);
        if self.position >= self.buffer_start && requested_end <= buffer_end {
            return Ok(());
        }

        let remaining = self.content_length.saturating_sub(self.position);
        let fetch_len = needed
            .max(RANGE_PREFETCH)
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        if fetch_len == 0 {
            return Ok(());
        }
        let fetch = with_network_retry("HTTP stream Range GET", || {
            fetch_range_once(
                &self.client,
                &self.url,
                self.credentials.as_ref(),
                self.position,
                fetch_len,
                Some(self.content_length),
                self.cancel_token.as_ref(),
            )
        })
        .map_err(network_error_to_io_error)?;
        self.buffer_start = self.position;
        self.buffer = fetch.body;
        Ok(())
    }
}

fn network_error_to_io_error(error: NetworkError) -> std::io::Error {
    if error == NetworkError::Cancelled {
        std::io::Error::new(std::io::ErrorKind::Interrupted, "Decode cancelled")
    } else {
        std::io::Error::other(error.to_string())
    }
}

impl Read for RangeStream {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        check_cancelled(self.cancel_token.as_ref()).map_err(network_error_to_io_error)?;
        if self.position >= self.content_length {
            return Ok(0);
        }
        self.ensure_buffered(output.len())?;
        let offset = (self.position - self.buffer_start) as usize;
        let available = self.buffer.len().saturating_sub(offset);
        if available == 0 {
            return Ok(0);
        }
        let read = available.min(output.len());
        output[..read].copy_from_slice(&self.buffer[offset..offset + read]);
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for RangeStream {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let new_position = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
            SeekFrom::End(delta) => i128::from(self.content_length) + i128::from(delta),
        };
        if !(0..=i128::from(u64::MAX)).contains(&new_position) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek position is outside the supported byte range",
            ));
        }
        self.position = new_position as u64;
        Ok(self.position)
    }
}

impl symphonia::core::io::MediaSource for RangeStream {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.content_length)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::sync::{mpsc, Arc};
    use std::thread;

    use super::*;

    struct TestResponse {
        status: &'static str,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    }

    impl TestResponse {
        fn partial(content_range: &str, body: &[u8]) -> Self {
            Self {
                status: "206 Partial Content",
                headers: vec![
                    ("Content-Range", content_range.to_string()),
                    ("Content-Length", body.len().to_string()),
                ],
                body: body.to_vec(),
            }
        }
    }

    fn serve_sequence(
        responses: Vec<TestResponse>,
    ) -> (Url, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            for response in responses {
                let (mut socket, _) = listener.accept().expect("accept test request");
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = socket.read(&mut chunk).expect("read test request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                request_tx
                    .send(String::from_utf8(request).expect("ASCII test request"))
                    .expect("send captured request");

                write!(socket, "HTTP/1.1 {}\r\n", response.status).expect("write status");
                for (name, value) in response.headers {
                    write!(socket, "{name}: {value}\r\n").expect("write header");
                }
                write!(socket, "Connection: close\r\n\r\n").expect("finish headers");
                socket
                    .write_all(&response.body)
                    .expect("write response body");
            }
        });
        (
            Url::parse(&format!("http://localhost:{}/audio.flac", address.port()))
                .expect("test URL"),
            request_rx,
            handle,
        )
    }

    fn serve_once(response: TestResponse) -> (Url, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        serve_sequence(vec![response])
    }

    fn fetch_from(
        response: TestResponse,
        start: u64,
        len: usize,
        total: Option<u64>,
    ) -> Result<RangeFetch, NetworkError> {
        let (url, request_rx, handle) = serve_once(response);
        let client = build_client(Duration::from_secs(2)).expect("test client");
        let result = fetch_range_once(&client, &url, None, start, len, total, None);
        let request = request_rx.recv().expect("captured request");
        assert!(
            request.lines().any(|line| line
                .eq_ignore_ascii_case(&format!("range: bytes={start}-{}", start + len as u64 - 1))),
            "request did not contain the expected Range header:\n{request}"
        );
        handle.join().expect("test server completed");
        result
    }

    #[test]
    fn private_and_reserved_ip_ranges_are_rejected() {
        for raw in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "192.0.0.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
        ] {
            let address = raw.parse().expect("test IP address");
            assert!(
                is_disallowed_ip(address),
                "accepted disallowed address {raw}"
            );
        }
        assert!(!is_disallowed_ip("1.1.1.1".parse().expect("public IPv4")));
        assert!(!is_disallowed_ip(
            "2606:4700:4700::1111".parse().expect("public IPv6")
        ));
    }

    #[test]
    fn hostname_resolving_to_localhost_is_rejected() {
        let result = resolve_and_validate_host("localhost", 80);
        assert!(
            matches!(result, Err(NetworkError::Other(ref message)) if message.contains("remote address rejected by policy")),
            "localhost was not rejected: {result:?}"
        );
    }

    #[test]
    fn redirect_to_loopback_is_rejected_before_second_hop() {
        let redirect = TestResponse {
            status: "302 Found",
            headers: vec![(
                "Location",
                "http://127.0.0.1:9/private/audio.flac".to_string(),
            )],
            body: Vec::new(),
        };
        let (url, request_rx, handle) = serve_once(redirect);
        let result = download_full_source(&url, None, None);
        assert!(
            matches!(
                result,
                Err(DecoderError::Network(NetworkError::Other(ref message)))
                    if message.contains("remote address rejected by policy")
            ),
            "redirect was not rejected: {result:?}"
        );
        let request = request_rx
            .recv()
            .expect("captured redirect response request");
        assert!(request.starts_with("GET "));
        assert!(
            request_rx.try_recv().is_err(),
            "client attempted a request after the rejected redirect"
        );
        handle.join().expect("test server completed");
    }

    #[test]
    fn content_range_parser_requires_numeric_consistent_geometry() {
        assert_eq!(
            ContentRange::parse("bytes 4-7/10").unwrap(),
            ContentRange {
                start: 4,
                end: 7,
                total: 10
            }
        );
        for invalid in [
            "items 4-7/10",
            "bytes */10",
            "bytes 4-7/*",
            "bytes 7-4/10",
            "bytes 4-10/10",
            "bytes 4-7/10/11",
        ] {
            assert!(
                matches!(
                    ContentRange::parse(invalid),
                    Err(NetworkError::InvalidRangeResponse(_))
                ),
                "accepted invalid Content-Range: {invalid}"
            );
        }

        let reflected_secret = ContentRange::parse("bytes token=query-secret").unwrap_err();
        assert!(!reflected_secret.to_string().contains("query-secret"));
    }

    #[test]
    fn exact_range_and_final_interval_are_accepted() {
        let ordinary = fetch_from(TestResponse::partial("bytes 2-5/8", b"cdef"), 2, 4, Some(8))
            .expect("valid ordinary range");
        assert_eq!(ordinary.body, b"cdef");
        assert_eq!(ordinary.total, 8);

        let final_byte = fetch_from(TestResponse::partial("bytes 7-7/8", b"h"), 7, 1, Some(8))
            .expect("valid final range");
        assert_eq!(final_byte.body, b"h");
    }

    #[test]
    fn successful_non_partial_response_is_range_not_supported() {
        let response = TestResponse {
            status: "200 OK",
            headers: vec![("Content-Length", "8".to_string())],
            body: b"abcdefgh".to_vec(),
        };
        assert!(matches!(
            fetch_from(response, 2, 4, None),
            Err(NetworkError::RangeNotSupported { status: 200 })
        ));
    }

    #[test]
    fn missing_or_mismatched_content_range_is_rejected() {
        let missing = TestResponse {
            status: "206 Partial Content",
            headers: vec![("Content-Length", "4".to_string())],
            body: b"cdef".to_vec(),
        };
        assert!(matches!(
            fetch_from(missing, 2, 4, Some(8)),
            Err(NetworkError::InvalidRangeResponse(_))
        ));

        assert!(matches!(
            fetch_from(
                TestResponse::partial("bytes malformed", b"cdef"),
                2,
                4,
                Some(8)
            ),
            Err(NetworkError::InvalidRangeResponse(_))
        ));

        let duplicate = TestResponse {
            status: "206 Partial Content",
            headers: vec![
                ("Content-Range", "bytes 2-5/8".to_string()),
                ("Content-Range", "bytes 1-4/8".to_string()),
                ("Content-Length", "4".to_string()),
            ],
            body: b"cdef".to_vec(),
        };
        assert!(matches!(
            fetch_from(duplicate, 2, 4, Some(8)),
            Err(NetworkError::InvalidRangeResponse(_))
        ));

        for content_range in ["bytes 1-4/8", "bytes 2-4/8", "bytes 2-5/9"] {
            assert!(matches!(
                fetch_from(TestResponse::partial(content_range, b"cdef"), 2, 4, Some(8)),
                Err(NetworkError::InvalidRangeResponse(_))
            ));
        }
    }

    #[test]
    fn content_length_and_body_must_match_the_interval() {
        let mismatched_length = TestResponse {
            status: "206 Partial Content",
            headers: vec![
                ("Content-Range", "bytes 2-5/8".to_string()),
                ("Content-Length", "5".to_string()),
            ],
            body: b"cdefg".to_vec(),
        };
        assert!(matches!(
            fetch_from(mismatched_length, 2, 4, Some(8)),
            Err(NetworkError::InvalidRangeResponse(_))
        ));

        let oversized_without_length = TestResponse {
            status: "206 Partial Content",
            headers: vec![("Content-Range", "bytes 2-5/8".to_string())],
            body: b"cdefg".to_vec(),
        };
        assert!(matches!(
            fetch_from(oversized_without_length, 2, 4, Some(8)),
            Err(NetworkError::InvalidRangeResponse(_))
        ));

        let short_without_length = TestResponse {
            status: "206 Partial Content",
            headers: vec![("Content-Range", "bytes 2-5/8".to_string())],
            body: b"cde".to_vec(),
        };
        assert!(matches!(
            fetch_from(short_without_length, 2, 4, Some(8)),
            Err(NetworkError::InvalidRangeResponse(_))
        ));
    }

    #[test]
    fn cancelled_range_fetch_returns_before_network_request() {
        let cancelled = Arc::new(AtomicBool::new(true));
        let token = DecodeCancelToken::from_flag(cancelled);
        let client = build_client(Duration::from_secs(2)).expect("test client");
        let url = Url::parse("http://127.0.0.1:9/never-requested.flac").expect("test URL");
        let result = fetch_range_once(&client, &url, None, 0, 8, None, Some(&token));
        assert!(matches!(result, Err(NetworkError::Cancelled)));
    }

    #[test]
    fn full_download_uses_one_get_without_a_head_dependency() {
        let response = TestResponse {
            status: "200 OK",
            headers: vec![
                ("Content-Length", "4".to_string()),
                ("Content-Type", "audio/flac".to_string()),
            ],
            body: b"data".to_vec(),
        };
        let (url, request_rx, handle) = serve_once(response);
        let (cursor, content_type) =
            download_full_source(&url, None, None).expect("bounded full download");
        assert_eq!(cursor.into_inner(), b"data");
        assert_eq!(content_type.as_deref(), Some("audio/flac"));
        let request = request_rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET "),
            "unexpected request:\n{request}"
        );
        handle.join().expect("test server completed");
    }

    #[test]
    fn ignored_range_falls_back_to_one_bounded_full_get() {
        let ignored_range = TestResponse {
            status: "200 OK",
            headers: vec![("Content-Length", "4".to_string())],
            body: b"data".to_vec(),
        };
        let full_download = TestResponse {
            status: "200 OK",
            headers: vec![("Content-Length", "4".to_string())],
            body: b"data".to_vec(),
        };
        let (url, request_rx, handle) = serve_sequence(vec![ignored_range, full_download]);
        let location = HttpMediaLocation::from_url(url).expect("valid test URL");
        let result = open_http_media_source(&location, None, None);
        assert!(result.is_ok(), "bounded fallback should open successfully");

        let capability_request = request_rx.recv().expect("captured capability request");
        assert!(
            capability_request
                .lines()
                .any(|line| line.eq_ignore_ascii_case("range: bytes=0-0")),
            "missing capability Range header:\n{capability_request}"
        );
        let fallback_request = request_rx.recv().expect("captured fallback request");
        assert!(fallback_request.starts_with("GET "));
        assert!(
            !fallback_request
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("range:")),
            "full-download fallback retained a Range header:\n{fallback_request}"
        );
        handle.join().expect("test server completed");
    }

    #[test]
    fn non_range_network_errors_are_not_relabelled_or_retried_as_fallback() {
        let response = TestResponse {
            status: "404 Not Found",
            headers: vec![("Content-Length", "0".to_string())],
            body: Vec::new(),
        };
        let (url, request_rx, handle) = serve_once(response);
        let location = HttpMediaLocation::from_url(url).expect("valid test URL");
        let result = open_http_media_source(&location, None, None);
        assert!(matches!(
            result,
            Err(DecoderError::Network(NetworkError::HttpStatus(404)))
        ));
        let request = request_rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET "),
            "unexpected request:\n{request}"
        );
        assert!(
            request
                .lines()
                .any(|line| line.eq_ignore_ascii_case("range: bytes=0-0")),
            "missing capability Range header:\n{request}"
        );
        handle.join().expect("test server completed");
    }

    #[test]
    fn content_type_probe_essence_filters_generic_and_parameterized_types() {
        assert_eq!(probe_mime_essence("audio/flac"), Some("audio/flac"));
        assert_eq!(
            probe_mime_essence("audio/mpeg; charset=binary"),
            Some("audio/mpeg")
        );
        assert_eq!(probe_mime_essence(" audio/ogg "), Some("audio/ogg"));
        assert_eq!(probe_mime_essence("application/octet-stream"), None);
        assert_eq!(probe_mime_essence("Application/Octet-Stream"), None);
        assert_eq!(probe_mime_essence("binary/octet-stream"), None);
        assert_eq!(probe_mime_essence("text/plain; charset=utf-8"), None);
        assert_eq!(probe_mime_essence(""), None);
        assert_eq!(probe_mime_essence(";"), None);
    }

    #[test]
    fn http_log_identity_keeps_only_origin() {
        let raw = "https://basic-user:basic-password@example.test:8443/private/token.flac?signature=query-secret#fragment-secret";
        let location = HttpMediaLocation::parse(raw).expect("valid test URL");
        let identity = location.log_identity();
        assert_eq!(identity, "https://example.test:8443");
        for secret in [
            "basic-user",
            "basic-password",
            "private",
            "token.flac",
            "query-secret",
            "fragment-secret",
        ] {
            assert!(!identity.contains(secret));
        }
    }
}
