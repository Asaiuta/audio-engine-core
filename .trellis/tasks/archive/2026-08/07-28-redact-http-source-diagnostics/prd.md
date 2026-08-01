# Redact HTTP credentials and URL diagnostics

## Goal

Prevent HTTP Basic credentials, signed URL query parameters, URL userinfo, and
fragments from entering ordinary debug output, library logs, retry diagnostics,
or returned network-error strings. Establish one internal diagnostic-redaction
boundary without changing request behavior or the public construction shape of
`HttpCredentials`.

## What I already know

- The 2026-07-28 maintainability audit ranked printable credentials and raw URL
  logs as a P1 secret-exposure defect.
- Revalidation confirms `HttpCredentials` still derives `Debug` over public
  plaintext fields and `source/http.rs` logs the raw URL on both streaming and
  fallback paths.
- The original finding is incomplete: reqwest documents that `Error` may carry
  the complete request URL and explicitly provides `Error::without_url()` for
  sensitive URLs. `NetworkError::from(reqwest::Error)` currently stringifies
  that error on fallback classification, and retry logging prints it.
- The preceding Range-boundary task already gives HTTP protocol code one
  private owner in `source/http.rs`; this task builds on that structure.

## Requirements

- `Debug` formatting an `HttpCredentials` value redacts both username and
  password, including when embedded in another derived debug structure.
- HTTP lifecycle logs identify only the parsed URL origin
  (`scheme://host[:port]`), never userinfo, path, query, or fragment.
- Invalid URL diagnostics use a fixed placeholder rather than echoing raw input.
- Every `reqwest::Error` is stripped with `without_url()` before classification,
  source-chain inspection, message fallback, storage, display, or retry logging.
- Raw URLs remain available only to request construction and media hint parsing;
  no public HTTP API or credential-field visibility changes in this task.
- No new dependency is added; use reqwest's existing URL/error APIs.

## Acceptance Criteria

- [x] Credential debug output contains neither supplied username nor password.
- [x] URL log identity for a URL with userinfo/path/query/fragment equals only
      its origin and contains none of the secrets.
- [x] A real local reqwest failure for a signed URL produces a `NetworkError`
      display string containing neither the URL nor its token.
- [x] Source search finds no HTTP log that formats the raw URL.
- [x] Focused tests plus both supported Clippy/test matrices pass.
- [x] The final review records which broader credential/URL API refactors were
      adopted or rejected and why.

## Definition of Done

- Regression tests cover all three diagnostic channels: `Debug`, log identity,
  and reqwest error conversion.
- Logging and error-handling specs capture the executable redaction contract.
- Existing unrelated dirty work remains untouched.
- No commit, push, or archive occurs without the user's explicit direction.

## Technical Approach

Replace derived credential `Debug` with a manual redacted implementation. Add
one private `http_url_log_identity` helper based on reqwest's parsed URL origin
and use it at every HTTP lifecycle log site. Strip reqwest errors immediately
at the start of `From<reqwest::Error> for NetworkError` so every downstream
classification branch inherits the same safe input.

## Decision (ADR-lite)

**Context**: Query strings are not the only credential carrier; Basic tokens
can appear in either userinfo field, and reqwest errors can reintroduce URLs
even after explicit logs are sanitized.

**Decision**: Redact both credential fields, log origin only, and remove URLs
entirely from reqwest errors before any inspection or rendering.

**Consequences**: Diagnostics retain host/port correlation and typed error
classification but deliberately lose request path/query detail. Callers that
need request correlation must attach their own non-secret identifier.

## Out of Scope

- Making `HttpCredentials` fields private or introducing a secrecy dependency.
- Replacing string/path source entry points with a public typed `MediaLocation`.
- Cache URL identity, URL scheme recognition, redirect policy, or TLS policy.

## Technical Notes

- `src/decoder/source.rs`: public credential type.
- `src/decoder/source/http.rs`: non-RT HTTP lifecycle logs.
- `src/decoder/error.rs`: reqwest conversion and retry logging.
- reqwest 0.12.28 documents full-URL error risk and `without_url()` in
  `src/error.rs:14-16,86-90` of the local dependency source.
