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
`RangeNotSupported`, `InvalidRangeResponse`, `Other`) and exposes
`is_retriable()`. The decoder retries retriable network errors with bounded
backoff (`with_network_retry`, max 3 attempts) — this is a non-RT, decode-side
path where sleeping/logging is acceptable. Range capability/protocol failures
are deterministic and therefore non-retriable.

Every `reqwest::Error` conversion calls `without_url()` before inspecting the
source chain or rendering fallback text. reqwest documents that its errors may
retain the full request URL, including signed query parameters. HTTP body
`std::io::Error` fallback diagnostics retain only the structured
`ErrorKind`; they do not reflect dependency messages that may wrap request
context.

## Scenario: HTTP Range Source Trust Boundary

### 1. Scope / Trigger

- Trigger: changing HTTP source opening, Range capability probing, seekable
  buffering, authenticated request construction, or full-download fallback in
  `src/decoder/source/http.rs`.
- This is an untrusted-input and memory-availability boundary. A request header
  alone is not evidence that the response body begins at the requested offset
  or remains within the requested allocation bound.

### 2. Signatures

```rust
fn fetch_range_once(
    client: &reqwest::blocking::Client,
    url: &str,
    credentials: Option<&HttpCredentials>,
    start: u64,
    len: usize,
    expected_total: Option<u64>,
    cancel_token: Option<&DecodeCancelToken>,
) -> Result<RangeFetch, NetworkError>;

NetworkError::RangeNotSupported { status: u16 }
NetworkError::InvalidRangeResponse(String)
```

### 3. Contracts

- Capability probing and ordinary streaming reads call the same strict Range
  fetch boundary. Do not infer support independently from `HEAD` or
  `Accept-Ranges`.
- Every production HTTP client installs the same address policy for Range
  probes, later Range reads, and full-download fallback. DNS resolution must
  reject the request when any returned address is loopback, private,
  link-local, multicast, documentation, carrier-grade NAT, benchmarking, or
  otherwise reserved. The checked resolver output is the output handed to the
  connector; do not add a detached resolve-before-fetch preflight.
- IP-literal URLs are validated before request construction because a connector
  may not invoke DNS for them. Redirects are followed only through a custom
  policy that re-parses and re-resolves every target before the next request;
  the default reqwest redirect policy is not an acceptable trust boundary.
- A usable response is exactly `206 Partial Content` with one numeric
  `Content-Range: bytes start-end/total`. Its interval equals the requested
  interval, `end < total`, and a previously known total must remain unchanged.
- Optional `Content-Length` equals the interval length. The body reader caps
  reads at `len + 1`, rejects an extra byte, and rejects EOF before `len`.
- A final interval is valid because `RangeStream` shortens the request to the
  known remaining bytes before sending it; the response still matches exactly.
- `200`/other successful non-206 responses become `RangeNotSupported`. Invalid
  partial metadata/body becomes `InvalidRangeResponse`. Both are
  non-retriable and may enter the bounded full-download fallback.
- DNS, TLS, authentication/status, timeout, cancellation, and connection errors
  preserve their structured identity. They are not relabelled as lack of Range
  support and do not trigger a duplicate full download.
- Full-download fallback performs one GET, validates advertised length before
  allocation, and enforces the configured limit incrementally when length is
  absent. It does not require a preliminary HEAD.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Response status is successful but not 206 | `RangeNotSupported`, no body buffering |
| Missing, duplicate, non-ASCII, wildcard, or malformed `Content-Range` | `InvalidRangeResponse` |
| Returned start/end differs from request | `InvalidRangeResponse`; never install bytes at the requested offset |
| Returned total changes after the probe | `InvalidRangeResponse` |
| `Content-Length` differs or body is short/oversized | `InvalidRangeResponse` with at most `len + 1` bytes read |
| 401/403/404 or other non-success status | `HttpStatus`; no fallback request |
| Cancellation before/during read | structured cancellation (`Cancelled`/`Canceled`/`Interrupted` at its boundary) |
| Full GET exceeds configured memory limit | typed network failure before extending beyond the limit |
| Hostname resolves to any non-public address | non-retriable policy rejection; no connection |
| Redirect target resolves to a non-public address | policy rejection before the redirected request |

### 5. Good / Base / Bad Cases

- Good: probe `bytes=0-0`, validate its exact 206 response, then request an
  exact prefetch using the now-known total.
- Base: a server ignores Range with `200`; discard that response and perform
  one bounded full GET.
- Bad: trust `Accept-Ranges`, accept any successful 2xx, parse only the total
  suffix, or call `Response::bytes()` before enforcing the interval bound.

### 6. Tests Required

- Use a loopback TCP HTTP fixture, not parser-only mocks, to assert emitted
  Range headers plus status/header/body behavior.
- Accept an exact ordinary interval and an exact final interval.
- Reject 200/full-body, missing/malformed/duplicate headers, wrong start/end/
  total, mismatched content length, short body, and streamed oversized body.
- Assert ignored Range produces exactly a capability GET plus one full GET,
  with no HEAD and no Range header on fallback.
- Assert 404 produces one request and remains `HttpStatus(404)`.
- Resolve `localhost` through the policy and assert rejection. Exercise a real
  loopback HTTP response that redirects to `127.0.0.1`, assert the redirect is
  rejected, and assert the fixture observes no second request.
- Compile/test both all-features and Rubato-only so the optional HTTP dependency
  remains outside the no-HTTP graph.

### 7. Wrong vs Correct

#### Wrong

```rust
let response = client.get(url).header("Range", range).send()?;
let bytes = response.bytes()?; // buffers untrusted size and ignores offset
```

#### Correct

```rust
let fetch = fetch_range_once(
    client,
    url,
    credentials,
    requested.start,
    requested.len,
    known_total,
    cancel_token,
)?;
install_at_offset(fetch.body, requested.start);
```

## Propagation

- Propagate with `?` and let the typed enum flow to the caller. Do not
  stringify an error early if a typed variant exists.
- `Result` aliases per module are fine; the variant set is the contract.

## DSP Process Errors

Streaming processor, callback-chain, and offline-render construction propagate
`ProcessError` without a `String` compatibility boundary. In particular, all
three Convolver consumer entry points use the same conflict variant:

```rust
ProcessError::ConsumerAlreadyActive { processor: "Convolver" }

ConvolverProcessor::new(control) -> Result<ConvolverProcessor, ProcessError>
OutputChainBuilder::build_callback_chain(&self) -> Result<DspChain, ProcessError>
OutputChainBuilder::build_render_chain(&self) -> Result<OutputRenderChain, ProcessError>
FFTConvolver::new(ir_data, channels) -> Result<FFTConvolver, ProcessError>
FFTConvolver::process_into(&mut self, input, output) -> Result<(), ProcessError>
FFTConvolver::process_inplace(&mut self, buffer) -> Result<(), ProcessError>
```

The consumer lease is private; callers cannot forge or mismatch it. A build
that fails after acquiring the lease must release it through normal drop so a
later construction can succeed. String conversion is allowed only at an
external reporting boundary such as a custom benchmark whose enclosing return
type is already `Result<_, String>`.
Malformed interleaved IR geometry (zero channels, empty data, or an incomplete
frame) returns `ProcessError::InvalidBlock`/`InvalidGeometry` from the fallible
constructor. No public constructor retains an `expect`/panic compatibility
path, including code used during callback setup.

## No Panics On The Hot Path

The DSP/callback path must not panic: no `unwrap()`, `expect()`, or `panic!` in
`dsp_chain.rs`, `adapters.rs`, or the per-sample processor loops (they currently
contain zero). A panic across an audio callback boundary is
aborting/undefined. Validate inputs and return a typed error (or clamp/skip per
a documented policy) instead. `unreachable!`/`expect` is only acceptable on a
genuinely impossible, non-RT control path (e.g. the `with_network_retry` loop
post-condition) with a comment explaining why.

See `realtime-safety.md` for the full hot-path prohibition list.
