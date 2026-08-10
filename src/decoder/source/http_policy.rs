use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;
use reqwest::Url;

use super::{HttpAddressPolicy, HttpOrigin, NetworkError};

const REJECTION_MARKER: &str = "audio-engine-core: remote address rejected";

fn remote_address_error(detail: impl Into<String>) -> NetworkError {
    NetworkError::Other(format!(
        "remote address rejected by policy: {}",
        detail.into()
    ))
}

fn is_disallowed_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
        || address.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0 && !matches!(octets[3], 9 | 10))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        || octets[0] >= 240
}

fn embedded_ipv4(address: Ipv6Addr, prefix_segments: &[u16]) -> Option<Ipv4Addr> {
    let segments = address.segments();
    if !segments.starts_with(prefix_segments) {
        return None;
    }
    let high = segments[6].to_be_bytes();
    let low = segments[7].to_be_bytes();
    Some(Ipv4Addr::new(high[0], high[1], low[0], low[1]))
}

fn is_disallowed_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    let nat64_well_known = embedded_ipv4(address, &[0x0064, 0xff9b, 0, 0, 0, 0]);
    let nat64_local = segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 1;
    let six_to_four = if segments[0] == 0x2002 {
        let high = segments[1].to_be_bytes();
        let low = segments[2].to_be_bytes();
        Some(Ipv4Addr::new(high[0], high[1], low[0], low[1]))
    } else {
        None
    };

    address.is_loopback()
        || address.is_unspecified()
        || address.is_unique_local()
        || address.is_unicast_link_local()
        || address.is_multicast()
        || address.to_ipv4().is_some_and(is_disallowed_ipv4)
        || nat64_well_known.is_some_and(is_disallowed_ipv4)
        || nat64_local
        || six_to_four.is_some_and(is_disallowed_ipv4)
        || (segments[0] == 0x0100 && segments[1..4] == [0, 0, 0])
        || (segments[0] == 0x2001 && segments[1] <= 0x01ff)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
        || segments[0] == 0x5f00
        || (segments[0] & 0xffc0) == 0xfec0
}

fn is_disallowed_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_disallowed_ipv4(address),
        IpAddr::V6(address) => is_disallowed_ipv6(address),
    }
}

fn validate_ip(address: IpAddr) -> Result<(), NetworkError> {
    if is_disallowed_ip(address) {
        Err(remote_address_error(format!("{address}")))
    } else {
        Ok(())
    }
}

fn resolve_host(host: &str, port: u16) -> Result<Vec<SocketAddr>, NetworkError> {
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| NetworkError::DnsFailure(format!("lookup failed ({:?})", error.kind())))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(NetworkError::DnsFailure(
            "lookup returned no addresses".to_string(),
        ));
    }
    Ok(addresses)
}

fn resolve_and_validate_host(host: &str, port: u16) -> Result<Vec<SocketAddr>, NetworkError> {
    let addresses = resolve_host(host, port)?;
    for address in &addresses {
        validate_ip(address.ip())?;
    }
    Ok(addresses)
}

fn canonical_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

fn parse_supported_http_url(raw_url: &str) -> Result<Url, NetworkError> {
    let url = Url::parse(raw_url)
        .map_err(|_| NetworkError::Other("invalid HTTP source URL".to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(NetworkError::Other(
            "HTTP source URL uses an unsupported scheme".to_string(),
        ));
    }
    Ok(url)
}

fn origin_from_url(url: &Url) -> Result<HttpOrigin, NetworkError> {
    let host = url
        .host_str()
        .ok_or_else(|| remote_address_error("URL has no host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| remote_address_error("URL has no supported port"))?;
    Ok(HttpOrigin {
        scheme: url.scheme().to_string(),
        host: canonical_host(host),
        port,
    })
}

pub(super) fn trusted_origin_policy(raw_url: &str) -> Result<HttpAddressPolicy, NetworkError> {
    let url = parse_supported_http_url(raw_url)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NetworkError::Other(
            "trusted HTTP origin must not contain userinfo".to_string(),
        ));
    }
    Ok(HttpAddressPolicy {
        trusted_origin: Some(origin_from_url(&url)?),
    })
}

fn parse_http_url(raw_url: &str, address_policy: &HttpAddressPolicy) -> Result<Url, NetworkError> {
    let url = parse_supported_http_url(raw_url)?;
    let origin = origin_from_url(&url)?;
    if let Some(trusted_origin) = &address_policy.trusted_origin {
        if origin != *trusted_origin {
            return Err(remote_address_error(
                "source URL is outside the trusted HTTP origin",
            ));
        }
    } else if let Ok(address) = origin.host.parse::<IpAddr>() {
        validate_ip(address)?;
    }
    Ok(url)
}

fn validate_redirect_target(
    url: &Url,
    address_policy: &HttpAddressPolicy,
) -> Result<(), NetworkError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(remote_address_error("redirect uses a non-HTTP scheme"));
    }
    let origin = origin_from_url(url)?;
    if let Some(trusted_origin) = &address_policy.trusted_origin {
        if origin == *trusted_origin {
            return Ok(());
        }
        if origin.host == trusted_origin.host {
            return Err(remote_address_error(
                "redirect changes the trusted HTTP origin",
            ));
        }
    }
    resolve_and_validate_host(&origin.host, origin.port).map(|_| ())
}

#[derive(Debug)]
struct RedirectAddressRejected;

impl fmt::Display for RedirectAddressRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REJECTION_MARKER)
    }
}

impl std::error::Error for RedirectAddressRejected {}

fn redirect_policy(address_policy: HttpAddressPolicy) -> redirect::Policy {
    let limited = redirect::Policy::limited(10);
    redirect::Policy::custom(move |attempt| {
        if validate_redirect_target(attempt.url(), &address_policy).is_ok() {
            limited.redirect(attempt)
        } else {
            attempt.error(RedirectAddressRejected)
        }
    })
}

struct PolicyResolver {
    address_policy: HttpAddressPolicy,
}

impl Resolve for PolicyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let allows_private = self
            .address_policy
            .trusted_origin
            .as_ref()
            .is_some_and(|origin| origin.host == canonical_host(&host));
        Box::pin(std::future::ready(
            if allows_private {
                resolve_host(&host, 0)
            } else {
                resolve_and_validate_host(&host, 0)
            }
            .map(|addresses| Box::new(addresses.into_iter()) as Addrs)
            .map_err(|error| {
                let message = if is_address_rejected(&error) {
                    format!("{REJECTION_MARKER}: {error}")
                } else {
                    error.to_string()
                };
                Box::new(std::io::Error::other(message)) as Box<dyn std::error::Error + Send + Sync>
            }),
        ))
    }
}

pub(super) fn build_client(
    timeout: Duration,
    connect_timeout: Duration,
    address_policy: &HttpAddressPolicy,
) -> Result<Client, NetworkError> {
    Client::builder()
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .no_proxy()
        .redirect(redirect_policy(address_policy.clone()))
        .dns_resolver(Arc::new(PolicyResolver {
            address_policy: address_policy.clone(),
        }))
        .build()
        .map_err(|error| NetworkError::Other(format!("Failed to create HTTP client: {error}")))
}

pub(super) fn get(
    client: &Client,
    url: &str,
    address_policy: &HttpAddressPolicy,
) -> Result<RequestBuilder, NetworkError> {
    Ok(client.get(parse_http_url(url, address_policy)?))
}

pub(super) fn head(
    client: &Client,
    url: &str,
    address_policy: &HttpAddressPolicy,
) -> Result<RequestBuilder, NetworkError> {
    Ok(client.head(parse_http_url(url, address_policy)?))
}

pub(super) fn is_address_rejected(error: &NetworkError) -> bool {
    matches!(
        error,
        NetworkError::Other(message)
            if message == "remote address rejected by policy"
                || message.starts_with("remote address rejected by policy:")
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    #[test]
    fn private_and_reserved_ip_ranges_are_rejected() {
        for raw in [
            "0.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:2::1",
            "2001:db8::1",
            "2002:7f00:1::1",
            "3fff::1",
            "3fff:fff::1",
            "5f00::1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
        ] {
            let address = raw.parse().expect("test IP address");
            assert!(
                is_disallowed_ip(address),
                "accepted disallowed address {raw}"
            );
        }
        for raw in [
            "1.1.1.1",
            "192.0.0.9",
            "192.0.0.10",
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
            "3fff:1000::1",
        ] {
            let address = raw.parse().expect("public test IP address");
            assert!(!is_disallowed_ip(address), "rejected public address {raw}");
        }
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
    fn client_dns_rejection_retains_address_policy_classification() {
        let address_policy = HttpAddressPolicy::public_only();
        let client = build_client(
            Duration::from_secs(2),
            Duration::from_secs(2),
            &address_policy,
        )
        .expect("test client");
        let error = get(&client, "http://localhost:9/private.flac", &address_policy)
            .expect("validated request")
            .send()
            .map_err(NetworkError::from)
            .expect_err("localhost DNS result must be rejected");
        assert!(
            is_address_rejected(&error),
            "DNS rejection lost its address-policy classification: {error:?}"
        );
    }

    #[test]
    fn direct_loopback_literal_is_rejected_before_request_construction() {
        let client = Client::builder().build().expect("test client");
        let result = get(
            &client,
            "http://127.0.0.1/private.flac",
            &HttpAddressPolicy::public_only(),
        );
        assert!(
            matches!(result, Err(NetworkError::Other(ref message)) if message.contains("remote address rejected by policy")),
            "loopback literal was not rejected"
        );
    }

    #[test]
    fn trusted_origin_allows_its_private_address_only() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let handle = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept trusted request");
            let mut request = [0_u8; 1024];
            let read = socket.read(&mut request).expect("read trusted request");
            assert!(request[..read].starts_with(b"GET "));
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write trusted response");
        });

        let url = format!("http://127.0.0.1:{}/audio.flac", address.port());
        let address_policy = HttpAddressPolicy::trusted_origin(&url).expect("trusted origin");
        let client = build_client(
            Duration::from_secs(2),
            Duration::from_secs(2),
            &address_policy,
        )
        .expect("test client");
        let response = get(&client, &url, &address_policy)
            .expect("trusted request")
            .send()
            .expect("trusted private origin request");
        assert!(response.status().is_success());
        handle.join().expect("trusted server completed");

        let other_port = if address.port() == u16::MAX {
            address.port() - 1
        } else {
            address.port() + 1
        };
        let other_origin = format!("http://127.0.0.1:{other_port}/private.flac");
        let error = get(&client, &other_origin, &address_policy)
            .expect_err("different private origin must be rejected");
        assert!(is_address_rejected(&error));
    }

    #[test]
    fn trusted_private_origin_keeps_same_origin_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            for response in [
                "HTTP/1.1 302 Found\r\nLocation: /redirected.flac\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            ] {
                let (mut socket, _) = listener.accept().expect("accept trusted request");
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = socket.read(&mut chunk).expect("read trusted request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                request_tx.send(request).expect("send captured request");
                socket
                    .write_all(response.as_bytes())
                    .expect("write trusted response");
            }
        });

        let url = format!("http://127.0.0.1:{}/audio.flac", address.port());
        let address_policy = HttpAddressPolicy::trusted_origin(&url).expect("trusted origin");
        let client = build_client(
            Duration::from_secs(2),
            Duration::from_secs(2),
            &address_policy,
        )
        .expect("test client");
        let response = get(&client, &url, &address_policy)
            .expect("trusted request")
            .basic_auth("alice", Some("secret"))
            .send()
            .expect("same-origin redirect");
        assert!(response.status().is_success());
        let first = request_rx.recv().expect("initial request");
        let second = request_rx.recv().expect("redirected request");
        assert!(first.starts_with(b"GET /audio.flac "));
        assert!(second.starts_with(b"GET /redirected.flac "));
        let first_text = String::from_utf8_lossy(&first).to_ascii_lowercase();
        let second_text = String::from_utf8_lossy(&second).to_ascii_lowercase();
        assert!(first_text.contains("authorization: basic "));
        assert!(second_text.contains("authorization: basic "));
        handle.join().expect("trusted redirect server completed");
    }

    #[test]
    fn trusted_origin_does_not_follow_trust_across_scheme_or_port() {
        let address_policy = HttpAddressPolicy::trusted_origin("http://127.0.0.1:8080/audio")
            .expect("trusted origin");

        for target in [
            "https://127.0.0.1:8080/redirected.flac",
            "http://127.0.0.1:8081/redirected.flac",
        ] {
            let url = Url::parse(target).expect("redirect target");
            let error = validate_redirect_target(&url, &address_policy)
                .expect_err("changed origin must not inherit private-address trust");
            assert!(is_address_rejected(&error), "unexpected error: {error:?}");
        }
    }

    #[test]
    fn redirect_to_loopback_is_rejected_before_second_hop() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
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
            request_tx.send(request).expect("send captured request");
            write!(
                socket,
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/private.flac\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write redirect response");
        });

        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .connect_timeout(Duration::from_secs(2))
            .redirect(redirect_policy(HttpAddressPolicy::public_only()))
            .build()
            .expect("test client");
        let url = format!("http://localhost:{}/audio.flac", address.port());
        let result = get(&client, &url, &HttpAddressPolicy::public_only())
            .expect("validated initial request")
            .basic_auth("alice", Some("secret"))
            .send()
            .map_err(NetworkError::from);
        assert!(
            matches!(result, Err(NetworkError::Other(ref message)) if message == "remote address rejected by policy"),
            "redirect was not rejected"
        );
        let request = request_rx.recv().expect("captured initial request");
        assert!(request.starts_with(b"GET "));
        let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();
        assert!(request_text.contains("authorization: basic "));
        assert!(
            request_rx.try_recv().is_err(),
            "client attempted a request after the rejected redirect"
        );
        handle.join().expect("test server completed");
    }
}
