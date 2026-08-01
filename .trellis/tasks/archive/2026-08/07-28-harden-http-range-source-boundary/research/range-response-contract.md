# HTTP Range response contract revalidation

## Snapshot

- Revalidated on 2026-07-28 against branch `main`, HEAD `0c62feb`.
- `src/decoder/source.rs`, `src/decoder/error.rs`, and `src/decoder/tests.rs`
  were clean before this task began.

## Verdict

The audit finding is accurate. `fetch_range_once` sends an exact bounded Range
header but accepts any successful 2xx response, performs no `Content-Range`
validation, calls `Response::bytes()` without a response-size cap, and returns
the entire body. `RangeStream` then records that vector at the requested
offset. A server that ignores Range can therefore bypass the intended
prefetch bound and a mismatched partial response can feed wrong-offset bytes to
the decoder.

## Existing evidence

- `response_network_error` rejects only non-success statuses.
- `fetch_range_once` does not require 206 or inspect `Content-Range`.
- `Response::bytes()` buffers until EOF before the cancellation recheck.
- Initial and later buffers are assigned `buf_start = requested_start`.
- The initialization probe considers either status 206 or any parseable total
  fragment sufficient and does not verify `bytes 0-0/total`.
- Existing tests use no HTTP server and do not cover response geometry/body
  limits.

## Chosen contract

- Exact 206 only.
- Exact numeric `bytes start-end/total`; wildcard totals are not sufficient for
  a seekable source.
- Requested and returned intervals must match exactly.
- A previously known total must match the response total.
- Optional `Content-Length` must equal the interval size.
- The reader accepts no more and no fewer body bytes than the interval size.
- Invalid Range responses are non-retriable protocol errors.

## Refactor review required before implementation

Re-read the full HTTP source path, not only `fetch_range_once`, and decide with
source evidence whether to: consolidate request/auth helpers, replace the loose
one-byte probe parser with the strict fetch boundary, isolate HTTP code from
local source opening, and co-locate socket fixtures with the protocol parser.
Adopt changes that remove duplicated protocol ownership; reject moves whose
only benefit is a shorter file. Record the decision before code changes.

## Refactor decision

Adopted:

- Move the private HTTP implementation and its socket fixtures to
  `decoder/source/http.rs`. This is justified by ownership, not file length:
  the current mixed module has 23 HTTP cfg gates and combines local file
  opening with roughly 500 lines of transport protocol/state-machine code.
- Replace HEAD advertisement plus a separately interpreted one-byte GET with
  one strict Range fetch boundary. Capability probing and ordinary reads must
  use the same parser and bounded body reader.
- Centralize authenticated GET construction and HTTP client construction.
  Basic-auth attachment is currently repeated four times.
- Remove mandatory HEAD from full-download fallback. The GET headers provide
  the same pre-body `Content-Length` check, avoid a duplicate request, and let
  GET-capable endpoints that reject HEAD work.
- Make a successfully constructed `RangeStream` carry a required numeric total
  rather than the redundant `supports_range: bool` plus optional length.
- Fall back only for an explicit ignored Range or invalid Range protocol. DNS,
  TLS, authentication, status, timeout, and cancellation errors retain their
  structured identity instead of being relabelled as Range unsupported.

Rejected for this task:

- An async transport rewrite, a new HTTP dependency, or a public media-location
  redesign. These do not reduce the immediate duplicated protocol ownership
  enough to justify their compatibility and runtime costs here.
- A broad rewrite of decoder staging/metadata. Those are separate audit
  findings with different correctness contracts.

## Test matrix

- valid ordinary interval;
- valid final interval shortened to known remaining bytes;
- ignored Range (`200 OK`);
- missing and malformed `Content-Range`;
- wrong start, wrong end, and wrong total;
- declared or streamed oversized body;
- short body.

## Implemented result

- HTTP transport/protocol ownership moved from the mixed `source.rs` into the
  private `source/http.rs` module. The local/remote coordinator is now 115
  lines and contains no HTTP protocol parser.
- Basic-auth GET construction has one owner instead of four repeated blocks.
- `supports_range: bool` plus `Option<content_length>` was replaced by a
  constructible `RangeStream` that necessarily owns a validated numeric total.
- Capability probing and every prefetch/seek read call the same strict
  `fetch_range_once` validator and bounded reader.
- Full-download fallback no longer performs a mandatory HEAD and never extends
  its buffer past the configured cap.
- Only explicit ignored/invalid Range responses enter fallback; 404 and other
  structured transport/status failures are returned after one request.
- An async rewrite, public media-location redesign, and broad decoder staging
  changes were rejected as unrelated compatibility/risk expansion.

## Validation

All commands completed on the final dirty-worktree snapshot; no performance or
device-level claim is made.

- `cargo test --all-features decoder::source::http::tests`: 10 passed.
- `cargo test --all-features decoder::`: 31 passed.
- `cargo check --all-targets --no-default-features --features rubato`: passed.
- `cargo check --all-targets --no-default-features --features http,rubato`: passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed (existing LF/CRLF warnings only).
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`: passed.
- `cargo test --all-features`: 393 library, 20 benchmark-support, 25
  resampler-support, 3 Windows deployment, and 6 doctests passed; one native
  shim prerequisite test remained explicitly ignored.
- `cargo test --no-default-features --features rubato`: 428 library, 20
  benchmark-support, 25 resampler-support, 3 Windows deployment, and 6 doctests
  passed; the same native shim prerequisite remained explicitly ignored.
