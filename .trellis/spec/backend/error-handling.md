# Error Handling

> How this crate models and propagates errors. Source of truth:
> `src/decoder/error.rs` and the `Result` signatures across `src/`.

---

## Error Model

This is a library, so errors are returned as typed `Result<T, E>` values, never
swallowed and never surfaced as HTTP responses (the consuming app owns that).
Error enums are defined with `thiserror::Error` and carry a `#[error("...")]`
display string per variant.

## The Decoder Error Type

`DecoderError` (`src/decoder/error.rs`) is the primary error enum:

| Variant | Meaning |
| --- | --- |
| `FileOpen(std::io::Error)` | `#[from]` local file open failure |
| `Network(NetworkError)` | HTTP(S) source failure — **`#[cfg(feature = "http")]` only** |
| `UnsupportedFormat` | container format not supported |
| `NoAudioTrack` | no decodable audio track in the container |
| `Decoder(String)` | codec failed to decode a packet |
| `Probe(String)` | format probing failed |
| `Canceled` | decode stopped via a `DecodeCancelToken` |

Conventions to preserve:

- `#[from]` is used for the one lossless conversion (`std::io::Error` ->
  `FileOpen`). Other conversions are explicit (e.g.
  `network_error_to_decoder_error` maps a cancelled network op to
  `DecoderError::Canceled` rather than `Network`).
- Feature-gated variants (`Network`) and their helper types must stay behind
  `#[cfg(feature = "http")]` so the crate builds with `--no-default-features`.
- **Known gap (do not treat as the intended contract):** `UnsupportedFormat`
  is defined but never constructed today; probe/decode failures currently
  return the generic `Probe(String)` / `Decoder(String)`. Routing genuinely
  unsupported input to the typed variant is owned by
  `06-12-audio-engine-decoder-format-capability`.

## Network Errors

`NetworkError` (http feature) classifies transport failures
(`HttpTimeout`, `ConnectionReset`, `HttpStatus(u16)`, `DnsFailure`, `TlsError`,
`Other`) and exposes `is_retriable()`. The decoder retries retriable network
errors with bounded backoff (`with_network_retry`, max 3 attempts) — this is a
non-RT, decode-side path where sleeping/logging is acceptable.

## HTTP Source Address Policy

### 1. Scope / Trigger

This policy applies to every request-supplied HTTP(S) decoder source. Range
initialization/reads and full-download fallback share the implementation in
`decoder/source/http_policy.rs`.

### 2. Signatures

- `HttpAddressPolicy::public_only()` is the default for request-supplied URLs.
- `HttpAddressPolicy::trusted_origin(url)` permits private addresses only for
  the exact configured scheme/host/port; it is for persisted, user-selected
  sources such as a LAN WebDAV origin, never for request credentials or flags.
- `build_client(timeout, connect_timeout, address_policy)` is the only decoder
  HTTP client constructor.
- `get(client, url, address_policy)` and `head(client, url, address_policy)`
  validate URL syntax, scheme, origin and IP literals before constructing a
  request.
- `is_address_rejected(error)` identifies the canonical address-policy error
  after reqwest has erased the internal resolver or redirect error type.

### 3. Contracts

- DNS resolution rejects the request when any returned address is loopback,
  private, link-local, multicast, documentation, carrier-grade NAT,
  benchmarking, IPv4-mapped private, or otherwise reserved.
- The checked resolver output is handed directly to the connector. Policy
  clients call `no_proxy()` so ambient proxy settings cannot move target DNS
  resolution outside that checked connector.
- Every redirect target is parsed and re-resolved before the next request;
  public HTTP(S) redirects remain supported and retain reqwest's 10-hop limit.
- A trusted origin may redirect within its exact scheme/host/port. A redirect
  to another origin returns to the public-address policy; changing only the
  trusted host's scheme or port is rejected instead of inheriting trust.
- Address-policy rejections are non-retriable and never enter full-download
  fallback.

### 4. Validation & Error Matrix

- Disallowed IP literal -> `NetworkError::Other("remote address rejected by policy: ...")`.
- Hostname resolving to any disallowed IP -> canonical address-policy error.
- Redirect to a disallowed or non-HTTP(S) target -> canonical address-policy
  error and no second request.
- Configured private origin -> allowed only when the request and same-origin
  redirects match the policy's exact scheme/host/port.
- Ordinary DNS/transport/status failures -> their existing `NetworkError`
  classification; do not relabel them as policy rejection.

### 5. Good / Base / Bad Cases

- Good: a public CDN redirects to another public HTTP(S) CDN and both resolver
  results are checked.
- Base: a direct public IP or public hostname uses the shared policy client.
- Bad: `localhost`, RFC 1918, link-local, documentation or encoded private IPv4
  destinations are rejected before a connection.

### 6. Tests Required

- Cover reserved IPv4/IPv6 ranges and direct IP-literal rejection.
- Resolve `localhost` through both the policy helper and a real policy client;
  assert the latter retains `is_address_rejected == true`.
- Use a real loopback response to prove a redirect to `127.0.0.1` sends no
  second request.
- Cover a configured private origin, same-origin redirects, and rejection when
  the trusted host changes scheme or port.

### 7. Wrong vs Correct

Wrong: resolve once in a detached preflight, then let a default reqwest client
resolve again, follow redirects or use an ambient proxy.

Correct: construct every decoder request with `build_client`, pass the same
explicit `HttpAddressPolicy` through Range, seek and full-download paths,
validate literals through `get` / `head`, and let the checked resolver supply
the connector's actual addresses for every hop.

## Propagation

- Propagate with `?` and let the typed enum flow to the caller. Do not
  stringify an error early if a typed variant exists.
- `Result` aliases per module are fine; the variant set is the contract.

## No Panics On The Hot Path

The DSP/callback path must not panic: no `unwrap()`, `expect()`, or `panic!` in
`dsp_chain.rs`, `adapters.rs`, or the per-sample processor loops (they currently
contain zero). A panic across an audio callback boundary is
aborting/undefined. Validate inputs and return a typed error (or clamp/skip per
a documented policy) instead. `unreachable!`/`expect` is only acceptable on a
genuinely impossible, non-RT control path (e.g. the `with_network_retry` loop
post-condition) with a comment explaining why.

See `realtime-safety.md` for the full hot-path prohibition list.
