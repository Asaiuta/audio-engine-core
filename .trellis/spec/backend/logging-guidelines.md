# Logging Guidelines

> Logging conventions for the **non-realtime** paths of this crate. The hot
> audio path does not log at all — see `realtime-safety.md`.

---

## Library, Not Application

This crate uses the `log` crate facade (`log = "0.4"`) and emits records
through `log::warn!` / `log::info!` / etc. It does **not** initialize a logger
or pick a backend — choosing and installing a logger (`env_logger`,
`tracing-log`, etc.) is the consuming application's job. Never add a logger
implementation or global init here.

## Where Logging Is Allowed

Logging is allowed only on non-RT, setup/decode/diagnostic paths. Current call
sites are all off the audio callback:

- `decoder/streaming.rs`, `decoder/source.rs`, `decoder/source/http.rs`,
  `decoder/error.rs` — decode and network-retry diagnostics.
- `decoder/metadata.rs` — metadata extraction.
- `processor/resampler/mod.rs`, `processor/loudness/normalizer.rs`,
  `processor/loudness/meter.rs`, `processor/loudness_db.rs` — setup/control-path
  diagnostics, not the per-sample inner loops.

`diagnostics.rs` is a setup/diagnostic module and would be allowed to log, but
currently does not.

## Where Logging Is Forbidden

**No `log::*` macro may appear on the hot path** (`pipeline.rs`,
`processor/output_chain.rs`, `dsp_chain.rs`, `adapters.rs`, and the per-sample
processor loops). These files contain zero `log::` calls and must stay that way;
a log call inside a callback can allocate, format, and lock, violating realtime
safety. If you need visibility into a processor's behavior, surface it through a
value the control thread reads (e.g. an atomic telemetry snapshot like
`AtomicDynamicLoudnessTelemetry`), not a log line.

> `pipeline.rs` is on this list, not the allowed list. It was once only a
> `RingBuffer` streaming primitive, for which logging would have been fine; it
> now also owns `PlaybackPipeline::process`, the realtime callback entry point.
> The `PlaybackBuilder`/`PlaybackController` half of the same file is
> control-thread code, but keeping the whole file logging-free avoids a rule
> that depends on which half a future edit lands in.

`runtime::audio_thread_init` is also a hot-path entry even though it lives in a
top-level runtime helper rather than a processor module. Every target-specific
implementation and private helper reachable from it must remain logging-free.
Unsupported architectures use an empty initializer plus the software
`flush_subnormal_sample` fallback; they must not warn from the callback.

## Log Levels

- `warn!` — a recoverable problem the caller should know about (e.g. a network
  attempt failed and will be retried; a fallback path was taken).
- `info!` — significant lifecycle events on setup/decode paths.
- `debug!` / `trace!` — detailed diagnostics, off by default in release.
- `error!` — reserve for genuine failures; in a library prefer returning a
  typed error (see `error-handling.md`) over logging at `error!` and continuing.

## What Not To Log

- Nothing on the realtime path, ever.
- No tight-loop / per-sample logging on any path (floods and skews timing).
- No secrets; for HTTP sources, do not log full credentials/tokens.

## Scenario: HTTP Diagnostic Redaction

### 1. Scope / Trigger

- Trigger: changing `HttpCredentials`, HTTP source lifecycle logs, reqwest
  error conversion, or HTTP response-body error handling.
- Applies to logs, `Debug`, returned `Display` strings, retry warnings, panic
  attachments, and nested derived debug output. A value is not safe merely
  because it is emitted off the realtime path.

### 2. Signatures

```rust
impl std::fmt::Debug for HttpCredentials;
HttpMediaLocation::log_identity(&self) -> String;
MediaLocation::log_identity(&self) -> String;
impl From<reqwest::Error> for NetworkError;
```

### 3. Contracts

- `HttpCredentials` debug output always shows two `[REDACTED]` markers and
  never reveals either field. Basic-auth tokens can occupy the username as well
  as the password.
- HTTP lifecycle logs use `HttpMediaLocation::log_identity`, which returns only
  `scheme://host[:port]` from an already-validated URL. Userinfo, path, query,
  and fragment are absent. Invalid input is rejected before a location exists.
- Full URLs remain behind `HttpMediaLocation::url` for request construction and
  media hint parsing. Never pass that value directly to `log::*`.
- `reqwest::Error::without_url()` runs before timeout/status/source-chain/
  message classification. All branches operate on the stripped error.
- Response-body `io::Error` messages and malformed response-header text are not
  reflected into returned diagnostics. Preserve stable kinds/numeric geometry,
  not opaque dependency/server strings.

### 4. Validation & Error Matrix

| Input/diagnostic path | Required result |
| --- | --- |
| `format!("{credentials:?}")` | type/field names plus redaction markers; no values |
| URL with userinfo/path/query/fragment | lifecycle identity contains origin only |
| Invalid raw URL | typed construction error before formatting or transport |
| reqwest send error with signed URL | converted/displayed error contains no URL/token |
| HTTP body error with opaque message | classify stable `ErrorKind` or emit kind-only text |
| Malformed `Content-Range` containing attacker text | named protocol failure without reflected header value |

### 5. Good / Base / Bad Cases

- Good: `log::info!("HTTP origin ... {}", location.log_identity())` and `let
  error = error.without_url()` at the conversion entry.
- Base: retain origin for operational correlation while requiring callers to
  attach their own non-secret request ID for finer tracing.
- Bad: redact only `password`, strip only query strings, log path components,
  or sanitize an error after `to_string()` has already copied the URL.

### 6. Tests Required

- Credential debug test supplies distinct secrets in both fields and asserts
  neither appears.
- Typed URL identity test includes userinfo, password, port, private path,
  signed query, and fragment; assert exact origin output. Invalid inputs are
  covered by `MediaLocationError` variant tests.
- A loopback server accepts then closes a signed-URL request. Assert the raw
  reqwest error retained the URL before conversion and `NetworkError` display
  contains no token, userinfo, or address afterward.
- Inject opaque secret text into an unclassified body `io::Error` and malformed
  `Content-Range`; assert neither rendered result reflects it.
- Search every `log::*` call in `src/decoder` during review; raw URL variables
  must not be formatting arguments.

### 7. Wrong vs Correct

#### Wrong

```rust
#[derive(Debug)]
pub struct HttpCredentials { pub username: String, pub password: String }
log::info!("opening {}", location.url());
let text = reqwest_error.to_string();
```

#### Correct

```rust
impl Debug for HttpCredentials { /* both fields -> [REDACTED] */ }
log::info!("opening {}", location.log_identity());
let reqwest_error = reqwest_error.without_url();
let network_error = NetworkError::from(reqwest_error);
```
