# Typed Media Location Boundary Research

## Scope

This note re-verifies release gate 4 against the current tree after gates 1-3.
It covers decoder source routing, AutoMix inputs, HTTP redaction, and the
feature-gated loudness cache.

## Current Evidence

- `src/decoder/source.rs` already has a crate-private borrowed
  `MediaLocation<'a>`, but every public combined-source entry point still
  accepts `AsRef<Path>`. The implementation renders that path with
  `to_string_lossy()` before deciding whether it is local or HTTP.
- `StreamingDecoder::{open, open_with_credentials,
  open_with_credentials_and_cancel}` and
  `OpenedMediaSource::open_path_with_credentials_and_cancel` inherit that
  ambiguous boundary.
- `analyze_automix` and `analyze_automix_with_cancel` accept `String`, then
  pass it through the path-like decoder API.
- `src/processor/loudness_db.rs` independently classifies string paths as
  remote, normalizes local and remote strings through one track-id function,
  and exposes string-based `new`, `get`, `get_fresh`, `needs_scan`, and
  `delete` operations.
- The HTTP implementation already parses with `reqwest::Url` before request
  construction and has an origin-only log identity. This prevents userinfo,
  path, query, and fragment secrets from reaching its own log messages.
- `url` 2.5.8 is already present through `reqwest`, but is not a direct
  dependency. A public location type must also exist in local-only builds, so
  using `reqwest::Url` would incorrectly tie the public type to the optional
  `http` feature.
- `PathBuf` is the only standard owned path representation that preserves
  non-UTF-8 local paths. `std::fs::canonicalize` is unsuitable as an
  unconditional constructor step because it performs I/O and rejects missing
  or temporarily unavailable paths.

## Evaluated Public Shapes

### A. `MediaLocation::{Local(PathBuf), Http(url::Url)}`

Recommended baseline. The enum owns its input, URL parsing establishes the
HTTP invariant once, and local decoding never converts through UTF-8. Making
`url` a direct unconditional dependency keeps the type identical with and
without the optional transport implementation.

Trade-off: `url::Url` becomes part of the public API and dependency contract.
That is preferable to a hand-written URL parser or a public string variant
whose invalid states callers can construct.

### B. Public enum plus a private-field `HttpMediaLocation` wrapper

This avoids exposing `url::Url` directly and can enforce redacted formatting.
It adds another public type and forwarding API without improving routing or
cache correctness. It is only justified if dependency-type exposure is itself
a release concern.

### C. Separate local and HTTP open functions only

This makes routing explicit but duplicates every combined decoder and AutoMix
entry point, and it does not give the loudness cache one shared identity type.
It also leaves callers to carry their own local/remote sum type.

## Identity And Redaction Constraints

- Decoder routing uses the enum variant, never string-prefix inspection.
- HTTP construction accepts only parsed `http` or `https` URLs. Other schemes
  are rejected before transport work.
- Library logs use an origin-only identity. `Debug`/`Display` for the location
  must not accidentally print URL credentials, path tokens, query strings, or
  fragments.
- Cache identity must keep local and HTTP namespaces distinct. Local identity
  must not lowercase case-sensitive platforms; HTTP identity must not
  lowercase path or query components.
- A request URL and a safe persisted cache identity are different concepts.
  Persisting signed URLs verbatim leaks credentials, while dropping every
  query component can merge distinct resources. A stable digest or explicit
  caller-supplied revision is required if the cache is to distinguish full
  URLs without storing them in plaintext.
- A remote URL alone cannot prove that the current response body matches a
  previous analysis. Without an ETag, Last-Modified value, content digest, or
  equivalent caller-owned revision, remote freshness is unknown.

## Recommended Direction

Adopt shape A and move decoder/AutoMix entry points to `MediaLocation` by
value. Centralize safe log identity and cache-key derivation on the type.
Keep local-path preservation separate from cache canonicalization. For remote
freshness, choose an explicit policy before finalizing the cache schema:

1. correctness-first: an HTTP entry without a validator is never fresh;
2. compatibility-first: retain version-only freshness and document that it is
   only reuse policy, not content validation;
3. validator-aware: add an optional typed remote revision and require it for a
   cache hit.

Option 3 gives the strongest long-term contract but expands this gate into
transport metadata propagation. Option 1 is the smallest contract that makes
no false freshness claim.
