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
