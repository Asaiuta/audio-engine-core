# Introduce a Typed Media Location for Decoder Sources

1.0 release gate 4 of 9.

## Goal

Replace path/string guessing at decoder, AutoMix, and loudness-cache boundaries
with one owned typed location. Preserve non-UTF-8 local paths, validate HTTP
URLs once, prevent secret-bearing URLs from reaching logs or cache fields, and
give cache lookup/freshness a location-aware contract before the 1.0 API
freeze.

## What I Already Know

- Gate 2 removed legacy public surface and gate 3 established typed error
  boundaries; this gate may make deliberate breaking API changes because the
  crate has not been published.
- The current decoder has a crate-private borrowed `MediaLocation`, but public
  methods still accept `AsRef<Path>` and perform lossy classification.
- AutoMix accepts owned `String`, while loudness persistence accepts `&str` and
  repeats remote detection and identity normalization.
- Existing HTTP logs already use an origin-only identity, and reqwest errors
  are stripped of request URLs.
- `url` 2.5.8 is already transitive through the optional HTTP stack. A direct
  dependency would let the public type remain available in local-only builds.
- The 2026-07-28 audit explicitly recommended
  `MediaLocation::{Local(PathBuf), Http(Url)}` or distinct public entry points.

## Assumptions (Temporary)

- Use an owned public `MediaLocation` with `Local(PathBuf)` and
  `Http(url::Url)` variants.
- Make `url` a direct non-optional dependency; keep reqwest and actual network
  transport behind the existing `http` feature.
- Replace ambiguous combined-source inputs rather than retaining deprecated
  string/path wrappers.
- Preserve local paths exactly for decoding. Do not require filesystem
  canonicalization merely to construct a location.
- Safe formatting and safe cache identity are distinct from the request URL.

## Open Questions

None. The user selected correctness-first remote freshness: without an ETag,
Last-Modified value, content digest, or caller revision, an HTTP loudness row
is not fresh.

## Requirements (Evolving)

- Export one owned `MediaLocation` from `decoder` and the crate root.
- `MediaLocation::Local` preserves `PathBuf` without UTF-8 conversion.
- `MediaLocation::Http` contains a parsed URL and permits only `http` and
  `https` schemes.
- Invalid or unsupported URL construction returns a typed error; callers do
  not inspect error text.
- `Debug`, library logging, and error rendering never expose URL userinfo,
  path, query, or fragment data. The safe HTTP log identity is origin-only.
- `StreamingDecoder`, `OpenedMediaSource`, and AutoMix accept the typed
  location instead of guessing from `AsRef<Path>` or `String`.
- The `http`-disabled matrix recognizes an HTTP location structurally and
  returns `DecoderError::FeatureUnavailable` without attempting local I/O.
- Decoder routing contains no URL-prefix classification and HTTP request code
  does not reparse an already validated location merely to determine scheme.
- Loudness-cache construction, lookup, freshness checks, deletion, and
  outdated-record results use typed source identity rather than `&str` paths.
- Local and HTTP cache-key namespaces are distinct. Local identity does not
  fold case on case-sensitive platforms; HTTP identity does not lowercase
  case-sensitive URL path/query components.
- Signed URL credentials are not persisted in plaintext cache identifiers or
  source-display fields.
- An HTTP cache row without validator/revision evidence never produces a fresh
  cache hit. Scanner-version equality alone is insufficient remote freshness
  evidence.
- Schema migration is explicit and idempotent. Existing pre-1.0 rows are
  either migrated without ambiguity or invalidated for rescan; they are not
  silently interpreted under a new identity contract.
- Public API snapshots, docs, examples, benches, and affected Trellis specs
  are updated with the breaking surface.

## Acceptance Criteria (Evolving)

- [x] Local paths containing non-UTF-8 data on supported platforms reach
      `File::open` byte-exactly and are never classified as HTTP.
- [x] Mixed-case `HTTP`/`HTTPS` input is accepted by URL construction and
      routed as HTTP.
- [x] Non-HTTP URL schemes are rejected by a typed location-construction
      error.
- [x] With the `http` feature disabled, an HTTP location produces
      `FeatureUnavailable`, not a local-file error.
- [x] URL debug/log tests prove username, password, path token, query secret,
      and fragment secret are absent.
- [x] Local and remote cache identities cannot collide merely because their
      textual payloads match.
- [x] Windows URL identity preserves case-sensitive path/query components;
      local-path case policy is tested separately.
- [x] Cache freshness tests cover present/replaced/unreadable local sources and
      prove validator-less HTTP rows are never fresh.
- [x] Existing-schema migration behavior is tested against a real temporary
      SQLite database.
- [x] `cargo test --all-features` passes.
- [x] `cargo test --no-default-features --features rubato` passes.
- [x] Both supported strict Clippy matrices, formatting, rustdoc, packaging,
      and public API snapshot checks pass.
- [x] Focused component benchmark cases remain work-valid; no performance
      claim is made from compilation alone.

## Definition of Done

- The public API cannot represent an unvalidated HTTP location.
- Decoder/AutoMix/cache call sites use the typed boundary consistently.
- URL secrets stay out of library-controlled logs and persisted identities.
- Supported feature matrices and release checks are green.
- New source-identity and freshness rules are recorded in executable Trellis
  specs.
- Changes are committed in coherent task-owned commits; nothing is pushed or
  archived without explicit user direction.

## Technical Approach

### Public boundary

Add a public owned enum:

```rust
pub enum MediaLocation {
    Local(PathBuf),
    Http(url::Url),
}
```

Provide explicit constructors/conversions for local paths and HTTP strings.
Do not provide a generic string parser that guesses whether arbitrary text is
a path or URL. The variant is the routing decision.

The public enum and URL validation exist in all feature matrices. The `http`
feature controls only whether an HTTP variant can be opened.

### Decoder and AutoMix

Move the location by value into staged source opening, so request URL ownership
survives opening without cloning or path conversion. The HTTP module consumes
or borrows `url::Url`; local opening borrows the stored `Path`.

Replace the public `StreamingDecoder` and AutoMix path/string signatures. Keep
`OpenedMediaSource::open_local` only if it remains a genuinely distinct staged
local-only operation; do not retain ambiguous compatibility wrappers.

### Redaction

Implement a safe formatter/log identity on the typed location. HTTP identity
uses origin only. Local logging, where needed, follows the existing logging
policy and must not run on the audio callback.

### Loudness cache

Derive a typed cache identity from `MediaLocation`; do not reuse request URLs
as display strings. Use a schema discriminator for local versus HTTP entries
and an identity representation that cannot merge the two namespaces.

Local freshness uses metadata from the caller's typed local path and keeps the
existing exact scanner-version policy. The remote branch follows the policy
chosen from the open question above.

## Feasible Remote Freshness Policies

### A. Unknown means stale (selected)

An HTTP location without a validator never yields a fresh cache hit. This is
the smallest change that makes `get_fresh` truthful and avoids expanding the
decoder transport DTO. Trade-off: remote sources are re-analyzed unless a
later API adds explicit revision evidence.

### B. Keep version-only reuse

Retain current remote cache hits based only on scanner version, but name and
document the operation as reuse rather than freshness. This preserves remote
cache performance while allowing changed content to receive stale gain.

### C. Add validator-aware identity now

Carry an optional ETag/Last-Modified/content digest through source opening and
require it to match for a remote cache hit. This provides the strongest cache
contract but materially expands this gate into HTTP response metadata,
redirect, and caller-provided revision policy.

## Expansion Sweep

### Future Evolution

- Reserve a location-owned identity boundary that can later accept content
  digests or remote validators without returning to string-prefix routing.
- Keep URL parsing independent from the optional reqwest transport so another
  transport backend does not change the public type.

### Related Scenarios

- Staged `OpenedMediaSource` and direct `StreamingDecoder` opening must have
  identical routing, credential, cancellation, and redaction behavior.
- AutoMix and loudness analysis must use the same location value as playback;
  callers should not rebuild a path string at each layer.

### Failure And Edge Cases

- Non-UTF-8 local paths, mixed-case schemes, unsupported schemes, URL
  userinfo, signed query strings, fragments, relative paths, missing local
  files, and HTTP-disabled builds.
- Existing SQLite rows created under path-string identity rules and future
  rows written by a newer scanner/schema.

## Decision (ADR-lite)

**Context:** A path-like parameter cannot preserve local bytes and also model a
validated HTTP URL. Repeated string classification has already drifted between
decoder and cache code, while raw URLs can contain credentials.

**Decision:** Use one public owned `MediaLocation` backed by `PathBuf` or
`url::Url`, route exclusively by variant, and centralize safe formatting and
cache identity. Treat validator-less HTTP rows as stale; a future explicit
revision API may enable remote cache hits without weakening this contract.

**Consequences:** This is a deliberate breaking public API change before the
first release and adds `url` as a direct dependency. It removes ambiguous
convenience inputs, preserves non-UTF-8 local paths, and creates one place for
future validator/fingerprint evolution. Until that evidence exists, remote
sources are re-analyzed instead of receiving potentially stale gain data.

## Out of Scope

- Supporting non-HTTP URL schemes.
- Replacing reqwest or redesigning the Range-streaming protocol.
- Filesystem watching or background cache invalidation.
- Resolving symlink/volume identity for missing paths through mandatory I/O at
  location construction time.
- Audio callback work; all source opening and cache operations remain off-RT.

## Research References

- [`research/current-boundary-audit.md`](research/current-boundary-audit.md) -
  current call sites, public-shape alternatives, dependency evidence, and
  identity/redaction constraints.
- Archived maintainability audit:
  `.trellis/tasks/archive/2026-08/07-28-codebase-maintainability-audit/research/03b-decoder-and-runtime-modules.md`.

## Technical Notes

- Likely code: `Cargo.toml`, `src/decoder.rs`, `src/decoder/source.rs`,
  `src/decoder/source/http.rs`, `src/decoder/streaming.rs`,
  `src/processor/automix_analysis.rs`, `src/processor/loudness_db.rs`, module
  exports, tests, and affected benches.
- Likely specs: `.trellis/spec/backend/error-handling.md`,
  `database-guidelines.md`, `directory-structure.md`, and
  `analysis-fir-correctness.md`.
- Public API snapshots are required in all-features and rubato-only matrices.
