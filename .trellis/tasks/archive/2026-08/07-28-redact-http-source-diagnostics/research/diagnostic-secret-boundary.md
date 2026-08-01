# HTTP diagnostic secret-boundary revalidation

## Snapshot and verdict

- Revalidated on 2026-07-28 after the strict Range-source refactor.
- The audit finding remains accurate: credential `Debug` and two raw URL info
  logs expose normal routes into logs/crash output.
- The affected source files are already modified by the preceding in-session
  Range task; this task must build on those edits rather than reverting them.

## Additional source/dependency evidence

- `NetworkError::from(reqwest::Error)` may reach `e.to_string()` and the result
  is printed by bounded retry logging.
- reqwest 0.12.28 explicitly warns that errors may include the full request URL
  and directs callers with sensitive URLs to `Error::without_url()`.
- Sanitizing only explicit `log::info!` arguments would therefore leave a
  second URL-exposure path through returned/retried errors.

## Refactor decision

Adopt one diagnostic policy at each ownership boundary:

- the credential type owns its redacted `Debug`;
- the HTTP source module owns origin-only lifecycle identity;
- the reqwest conversion owns URL removal before error classification.

Reject a public credential newtype/API migration and a typed public URL source
for this task. Both may improve encapsulation later, but they carry compatibility
and call-site costs unrelated to closing current diagnostic leaks.

## Required tests

- direct credential `Debug` redaction;
- origin-only URL identity with userinfo, password, path, query, and fragment;
- local refused-connection reqwest error containing a signed URL before
  conversion and no URL/token after `NetworkError` conversion;
- source search confirming raw URL is absent from HTTP log arguments.

## Implemented result

- `HttpCredentials` now owns a manual `Debug` implementation that redacts both
  Basic-auth fields, including when another type renders it through `Debug`.
- `source/http.rs` owns one `http_url_log_identity` helper. Every HTTP lifecycle
  log uses its origin-only result; invalid input produces
  `<invalid-http-url>`. Raw URLs remain limited to request construction and
  media-format hint parsing.
- `NetworkError::from(reqwest::Error)` removes the URL before classification,
  source-chain inspection, fallback rendering, retry logging, or return.
- Unknown HTTP body I/O errors retain only `ErrorKind`, and malformed
  `Content-Range` diagnostics do not reflect the untrusted header value.
- The existing HTTP-module split was retained because it gives request,
  response, and diagnostic policy one owner. Credential request construction
  was also consolidated into one helper instead of repeating Basic-auth setup.

The broader public refactors were deliberately rejected. Making credential
fields private or adding a secrecy wrapper would be a public compatibility
change; replacing path/string inputs with a typed URL/source abstraction would
also cross the decoder API boundary. Neither is needed to establish the
diagnostic redaction invariant, so both remain separate design work rather
than being hidden inside this security fix.

## Verification result

Verified on 2026-07-28:

- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`:
  passed.
- `cargo test --all-features`: passed (396 library, 20 benchmark-support,
  25 resampler-support, 3 Windows deployment, and 6 doctests; one native-shim
  prerequisite test remained ignored by design).
- `cargo test --no-default-features --features rubato`: passed (429 library,
  20 benchmark-support, 25 resampler-support, 3 Windows deployment, and
  6 doctests; the same native-shim prerequisite test remained ignored).
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed; Git reported only existing LF-to-CRLF checkout
  notices.
- `task.py validate 07-28-redact-http-source-diagnostics`: passed with five
  implementation-context and five check-context entries.
- A complete PowerShell source scan of every `log::*` call under `src/decoder`
  found no HTTP log that formats the raw URL. The two URL-correlated lifecycle
  logs format only `log_identity`.

No performance benchmark was run because the change makes no performance
claim and does not alter the realtime path.
