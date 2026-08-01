# Harden HTTP range source trust boundary

## Goal

Prevent an HTTP server from bypassing the decoder's bounded Range-streaming
contract or supplying bytes for the wrong file offset. Before implementing the
fix, reassess the surrounding HTTP-source design for duplicated ownership,
parsing, request construction, error policy, and module boundaries. Apply
bounded refactoring where it materially reduces drift or makes the trust
boundary easier to verify; do not restrict the work to the smallest patch.

## What I already know

- The 2026-07-28 maintainability audit ranked this as the highest-risk finding.
- `fetch_range_once` currently accepts every successful 2xx response, does not
  validate `Content-Range`, and buffers the complete response with
  `Response::bytes()`.
- `RangeStream` installs the returned bytes at the requested offset, so a
  `200 OK` full body or mismatched `206` can become both an availability and
  decode-correctness defect.
- Existing HTTP tests cover cancellation and error classification only; they
  do not exercise a local server or response geometry.
- `src/decoder/source.rs` and `src/decoder/error.rs` are clean in the current
  dirty worktree and may be changed without overlapping the existing playback
  facade work.

## Requirements

- A Range body is accepted only from an HTTP `206 Partial Content` response.
- `Content-Range` must use `bytes start-end/total` with numeric, internally
  consistent values matching the requested start/end and the already-known
  total length when one exists.
- The body must contain exactly the declared/requested interval. Missing,
  malformed, short, oversized, or mismatched responses are rejected.
- Body reading is bounded before allocation/growth by the requested interval;
  a server cannot force buffering of its complete response.
- Valid final intervals remain supported by shortening the request to the
  known remaining bytes before it is sent.
- Range-contract failures are structurally represented as non-retriable
  `NetworkError`s rather than inferred from message text.
- No new dependency or callback-path behavior is introduced.
- Range request construction, authentication attachment, response parsing, and
  capability probing have one clear owner each; remove duplicate protocol
  interpretations found during the re-review.
- Refactor/split the HTTP implementation when doing so improves ownership and
  test localization. File length alone is not a reason to move code.

## Acceptance Criteria

- [x] A local HTTP fixture proves `200 OK` for a Range request is rejected.
- [x] Local fixtures reject missing/malformed `Content-Range`, wrong start,
      wrong end, inconsistent total, oversized bodies, and short bodies.
- [x] Local fixtures accept an exact ordinary interval and an exact final
      partial interval, and assert the emitted `Range` header.
- [x] Focused decoder HTTP tests pass with the `http` feature.
- [x] Both supported feature matrices pass formatting, Clippy, and tests.
- [x] Existing unrelated dirty files remain untouched.
- [x] The final review records which broader refactors were adopted or rejected
      and why, including maintenance impact rather than line-count arguments.

## Definition of Done

- Tests added for the actual socket-level boundary.
- Strict typed validation is applied to both the initialization probe and
  subsequent Range fetches.
- Formatting, lint, and supported test matrices are green.
- No commit, push, or archive occurs without the user's explicit direction.

## Technical Approach

First map the HTTP source responsibilities and call graph. Consolidate strict
numeric `Content-Range` parsing, request/auth construction, response metadata,
and bounded body reading behind one internal Range-fetch boundary. Reuse that
boundary for capability probing and ordinary fetches so initialization cannot
interpret the protocol differently. Split implementation/tests into a focused
submodule if the source review confirms that local-file and HTTP ownership are
currently blurred.

## Decision (ADR-lite)

**Context**: A permissive short-response policy would make it ambiguous whether
the server honored the requested interval, especially after seeks.

**Decision**: Require the exact requested interval and refactor around a single
validated Range-response abstraction. At EOF, `RangeStream` already knows the
total length and reduces the requested length to the remaining bytes, so a
valid final response is still exact.

**Consequences**: Broken servers fail or fall back instead of producing
plausible corrupt audio. Servers that omit a valid `Content-Range` are not
treated as seekable even if they advertise `Accept-Ranges`.

## Out of Scope

- Credential and signed-URL redaction remains a separately ranked finding, but
  this task may centralize request/auth and sanitized source-identity plumbing
  when that is necessary to remove duplication. It must not claim redaction is
  fixed unless its own adversarial tests are added and pass.
- Changing the HEAD/full-download fallback classification policy.
- Typed media locations, decoder metadata mutability, channel layouts, or
  checked decode-all arithmetic.

## Technical Notes

- Primary source: `src/decoder/source.rs`.
- Typed network error: `src/decoder/error.rs`.
- Audit evidence: `../07-28-codebase-maintainability-audit/research/03b-decoder-and-runtime-modules.md`.
- Relevant specs: backend error handling, quality, logging, and directory
  structure.
