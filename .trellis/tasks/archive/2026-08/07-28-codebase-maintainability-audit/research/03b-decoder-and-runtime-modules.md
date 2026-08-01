# Decoder, source, metadata, diagnostics, persistence, and runtime audit

## Snapshot and validation

- Final source snapshot for this area: 2026-07-28 16:14:33 +08:00.
- Branch: `main`.
- None of the files reviewed in this area appeared in the final
  `git status --short --branch` dirty-file list. Concurrent changes remained in
  the facade/processor files listed in the research index; this artifact does
  not use those moving files as its primary evidence.
- Relevant source mtimes at the final snapshot:

| File | Last write time (+08:00) |
|---|---|
| `src/channel_layout.rs` | 2026-07-10 13:07:42 |
| `src/decoder.rs` | 2026-07-11 13:51:47 |
| `src/decoder/error.rs` | 2026-07-26 14:59:45 |
| `src/decoder/metadata.rs` | 2026-07-19 11:35:27 |
| `src/decoder/source.rs` | 2026-07-26 15:03:22 |
| `src/decoder/streaming.rs` | 2026-07-19 15:20:58 |
| `src/decoder/tests.rs` | 2026-07-26 14:49:02 |
| `src/diagnostics.rs` | 2026-06-11 20:10:17 |
| `src/processor/loudness_db.rs` | 2026-07-10 13:07:43 |
| `src/runtime.rs` | 2026-06-11 20:10:17 |

Focused validation completed against this snapshot. Every command exited with
status 0:

```text
cargo test --all-features decoder::
  24 library tests passed

cargo test --all-features channel_layout
  5 library tests passed

cargo test --all-features loudness_db
  4 library tests passed

cargo test --no-default-features --features rubato decoder::
  17 library tests passed

cargo test --all-features flushes_denormals
  3 library tests passed
```

The HTTP tests are classification and pre-cancel unit tests; they do not run a
server or validate response body/range behavior. The SQLite tests use simple
in-memory rows and one changed local file. These limits matter for several
findings below.

Scope:

- local and HTTP source opening, Range streaming, retry/cancellation, and full
  download fallback;
- Symphonia probe/build/decode/seek and fixed staging ownership;
- audio/track metadata and channel-layout derivation;
- decode-memory diagnostics and audio-thread floating-point initialization;
- the feature-gated SQLite loudness metadata cache.

## Verdict

The decoder's staged construction, fixed packet buffer, borrowed output, and
exclusive gapless ownership are well-motivated. The main risks sit at external
boundaries. Range responses are trusted more than their requested geometry,
credentials and signed URLs can leak through ordinary diagnostics, mutable
public metadata also controls decoder internals, and several error/cache states
are collapsed into plausible success. Those are correctness and security
problems, not merely stylistic dislike of a large decoder.

## Confirmed findings

### P1 — a Range response can bypass the memory budget and supply bytes from the wrong offset

**Category**: remote-input correctness and availability defect; missing trust
boundary.

Evidence:

- Range mode can be selected from a successful HEAD response that advertises
  `Accept-Ranges: bytes` plus a content length
  (`src/decoder/source.rs:423-465`, `:509-543`).
- `fetch_range_once` sends a bounded `Range: bytes=start-end` request but
  accepts every successful 2xx response, because `response_network_error`
  rejects only non-success statuses (`:342-386`).
- It does not require status 206, parse/verify `Content-Range`, check that the
  returned start matches the requested start, or bound the body to `len`.
  `response.bytes()` buffers the complete response before returning it
  (`:376-386`).
- Initial and subsequent fills install that entire vector as though its first
  byte corresponded to the requested offset (`:509-532`, `:546-583`).
- The explicit decode-memory cap is enforced only by the separate full-
  download loop (`:206-307`); it is not applied to a Range body.

Consequence:

A server can advertise Range support and then ignore the header with `200 OK`
and a full multi-gigabyte body. The supposedly bounded streaming path buffers
the whole response and can exhaust process memory. On a non-zero seek it also
labels byte zero as the requested offset, feeding duplicated/wrong container
bytes to Symphonia. An oversized or mismatched 206 response has the same
unchecked path.

Direction:

Require and validate a syntactically correct 206 `Content-Range` for the exact
requested interval (with a documented short-final-range policy), cap reads
before allocation, and reject/fallback before exposing a `RangeStream` if the
server does not honor the contract. Tests need a local server that returns
200/full-body, wrong start/end, oversized 206, short 206, and a valid final
partial range.

### P1 — credentials and signed URL secrets are printable through normal diagnostics

**Category**: secret exposure.

Evidence:

- `HttpCredentials` derives `Debug` while containing a public plaintext
  password (`src/decoder/source.rs:28-36`). Any derived/container debug output
  prints both username and password.
- Both Range success and full-download fallback log the complete URL
  (`src/decoder/source.rs:137-149`). Query strings and URL userinfo commonly
  carry CDN signatures, access tokens, or temporary credentials.
- The project's logging contract explicitly forbids full credentials/tokens,
  even on these otherwise valid non-realtime logging paths.

Consequence:

Enabling ordinary info/debug logging or formatting a public credential value
can copy secrets into persistent logs, crash reports, or issue attachments.

Direction:

Implement a redacted `Debug` representation for credentials and log a sanitized
URL identity that drops userinfo/query/fragment (or a stable hash), never the
raw string.

### P1 — unknown positioned channels are silently re-labelled as a conventional layout

**Category**: audio correctness defect; channel-layout boundary violation.

Evidence:

- `ChannelPosition` documents that height/discrete roles the crate cannot
  classify become `Unspecified`, which downstream loudness excludes and
  downmix drops rather than guesses (`src/channel_layout.rs:25-31`).
- `layout_from_codec` collects only eleven known Symphonia positions. If the
  number collected differs from the real channel count, it discards the known
  mask and calls `ChannelLayout::from_count`
  (`src/decoder/streaming.rs:615-659`).
- `from_count` assigns conventional speaker roles to every count through eight
  (`src/channel_layout.rs:139-181`). It does not preserve known slots plus
  `Unspecified` slots in this fallback.

Consequence:

For example, a four-channel file containing front L/R plus two height channels
can be re-labelled front L/R plus rear L/R. Loudness then applies surround
weighting and downmix folds the height channels as rear program content. The
explicit layout becomes more dangerous than an unknown layout because it
confidently communicates the wrong speakers.

Direction:

Walk every positioned-mask bit in interleave order and emit `Unspecified` for
unsupported roles, preserving known roles and the exact count. Use count-based
guessing only when the container supplies no positional mask at all.

### P1/P2 — `decode_all` performs unchecked size arithmetic on untrusted duration metadata

**Category**: panic/memory-limit bypass risk; portability defect.

Evidence:

- `raw_total_frames` originates in container track metadata. `decode_all`
  casts it to `usize`, then multiplies by the publicly mutable channel count
  and sample width with ordinary unchecked arithmetic
  (`src/decoder/streaming.rs:478-503`). The multiplication is repeated for the
  allocation and log calculation.
- Overflow can panic in checked/debug builds or wrap in optimized builds before
  the result is compared with the configured memory limit.
- `DecodeMemoryBudget` similarly permits 32,768 MiB but computes bytes with
  unchecked `usize` multiplication (`src/diagnostics.rs:3-10`, `:31-47`), which
  cannot represent that maximum on a 32-bit target.

Consequence:

A malformed container or externally mutated decoder metadata can turn a
controlled typed "too large" result into a panic, a wrapped small estimate, or
an attempted capacity inconsistent with the budget. The current Windows x64
target makes ordinary media safe, but the public crate and its constants do
not declare a 64-bit-only contract.

Direction:

Use `usize::try_from` and `checked_mul` once to derive a validated sample/byte
count, compare it to the budget, and reuse that value. Resolve the budget with
checked arithmetic and clamp to addressable `usize` capacity on each target.

### P1 on affected targets — unsupported-architecture initialization logs on the audio thread

**Category**: hard realtime contract violation; target-specific.

`audio_thread_init` is explicitly called from the actual callback/playback
thread (`src/runtime.rs:6-18`). On architectures other than x86/x86_64/aarch64,
its selected implementation calls `log::warn!` (`:67-70`) and then marks the
thread initialized. Formatting/log dispatch may allocate or lock on the exact
path whose primary contract forbids both. The fallback sample-flush logic is
reasonable; the warning must be emitted during non-RT capability discovery or
omitted from the callback.

### P2 — source selection uses a filesystem path as a case-sensitive URL discriminator

**Category**: boundary and type-model mismatch.

All combined local/remote entry points accept `AsRef<Path>`, convert it
lossily, and recognize only lowercase `http://`/`https://`
(`src/decoder/source.rs:94-127`; `src/decoder/streaming.rs:163-181`). An uppercase
scheme or another valid URL spelling is treated as a local path and returns a
file-open error. Conversely, later helpers repeat their own string-prefix URL
tests. A typed `MediaLocation::{Local(PathBuf), Http(Url)}` (or distinct public
entry points) would put parsing, feature availability, credentials, logging,
and cache identity in one place.

### P2 — HTTP fallback treats every Range initialization failure as “Range unsupported” and requires HEAD twice

**Category**: error/fallback boundary defect; unnecessary retry amplification.

Evidence:

- `open_http_media_source` preserves only cancellation; every other
  `RangeStream::new` error falls into `_`, logs that Range is unsupported, and
  starts a full download (`src/decoder/source.rs:130-154`). This includes DNS,
  TLS, authentication, 404, timeout, and rate-limit failures.
- Range initialization requires a successful HEAD before it will probe with a
  one-byte GET (`:423-505`).
- The full-download fallback immediately performs another mandatory HEAD and
  returns on its failure before attempting the actual GET (`:225-269`).

Consequence:

A GET-capable endpoint that rejects HEAD with 405 cannot be decoded by either
path. A retriable 429/5xx may pay the entire three-attempt Range retry sequence
and then a second three-attempt HEAD sequence. The original error is obscured
by a false fallback log and possibly by the later request's error.

Direction:

Treat HEAD as an optional optimization, use a bounded GET/Range probe when it
is unsupported, and fall back only from a proven “server does not honor Range”
state. Propagate unrelated transport/status failures unchanged.

### P2 — steady-state Range reads lose retry, cancellation, and typed network classification

**Category**: cross-layer error erasure.

Evidence:

- The initial range fetch uses `with_network_retry`
  (`src/decoder/source.rs:509-523`), but `RangeStream::fetch_range` calls the
  one-shot function directly for every later cache miss (`:546-555`).
- `ensure_buffered` converts every non-cancellation `DecoderError` into an
  `io::Error` containing only its display string (`:573-580`).
- `decode_next_span` converts any `next_packet` failure other than
  `ResetRequired` into `DecoderError::Decoder(e.to_string())`
  (`src/decoder/streaming.rs:344-365`). Even an `Interrupted` cancellation that
  occurs inside `Read` can therefore cease to be `DecoderError::Canceled`.
- Blocking `send`/`response.bytes()` checks the token only before/after the
  call, so “during request” cancellation can wait for the 30- or 120-second
  request timeout.

Consequence:

The same network failure has a typed/retriable result during initialization and
a generic codec/decode string during playback. Callers cannot apply consistent
retry or cancellation policy, and a transient connection reset on a later
range has no bounded retry despite the documented network-error contract.

Direction:

Centralize all range fetches behind the same cancellation-aware retry policy
and preserve a structured transport cause through the `Read`/Symphonia
boundary (for example a downcastable source error or explicit source state).
Map `Interrupted` plus an asserted token back to `Canceled` at minimum.

### P2 — public mutable `AudioInfo` is also the decoder's private operational state

**Category**: ownership/invariant boundary violation.

Evidence:

- Both `StreamingDecoder` and `StreamingDecoderBuilder` expose `pub info`
  (`src/decoder/streaming.rs:33-56`, `:92-103`), and every `AudioInfo` field is
  public (`src/decoder/metadata.rs:25-44`).
- The decoder subsequently trusts those mutable fields for channel-count
  validation/staging slices and gapless trimming
  (`src/decoder/streaming.rs:368-410`), decode-all capacity (`:478-503`), seek
  frame conversion (`:565-582`), and public position (`:585-590`).
- Decoder tests mutate `encoder_delay` and `end_padding` directly as a fixture
  hook (`src/decoder/tests.rs:440-503`), demonstrating that observation data is
  also an unvalidated control channel.

Consequence:

A caller can make displayed metadata, allocated geometry, codec output,
gapless counters, and seek math disagree without any builder/setter validation.
Even benign metadata decoration requires holding a mutable decoder whose
internal invariants are then exposed.

Direction:

Keep operational geometry private and expose `info(&self) -> &AudioInfo` or a
cloned immutable DTO. Tests needing synthetic delay/padding should use a
crate-private validated fixture constructor, not the production public field.

### P2 — decoder error variants classify operation failures as format failures

**Category**: inaccurate naming and policy-hostile error model.

Evidence:

- Probe maps every `UnexpectedEof` to `UnsupportedFormat`
  (`src/decoder/streaming.rs:594-612`). EOF can also mean a recognized but
  truncated file or a prematurely ended remote source.
- Seek maps Symphonia `Unsupported` to the same `UnsupportedFormat` variant
  (`:662-672`), even after the format was successfully opened and decoded. The
  unsupported capability is seek, not format.
- An HTTP URL in a build without the HTTP feature becomes `Probe(String)`
  (`src/decoder/source.rs:107-117`), although no probe was attempted.
- Playback Range errors can collapse further into `Decoder(String)` as
  described above.

Consequence:

UI/retry/fallback code can tell a user their playable format is unsupported
when only seeking is unavailable, or treat truncation/network loss as a
capability issue. Free-form messages are then required to recover operation
context.

Direction:

Model at least unsupported container/codec, corrupt or truncated input,
unseekable source, feature-disabled source, cancellation, and transport failure
as distinct typed states. Preserve the source error chain instead of moving
operation names into strings.

### P2 — ReplayGain metadata accepts non-finite values and feeds them directly into gain policy

**Category**: untrusted metadata validation defect.

Both ReplayGain helpers accept anything `f64::parse` accepts, including NaN and
infinities (`src/decoder/metadata.rs:268-291`), with no positive/finite check for
peak values or finite/range policy for gains. `LoudnessNormalizer` then uses the
optional tag directly to calculate and return gain
(`src/processor/loudness/normalizer.rs:174-207`). A malicious or malformed tag
can therefore produce non-finite gain instead of falling back to measurement.
Validate metadata at extraction and defensively at use.

### P2 — loudness-cache freshness treats several unknown/stale states as fresh

**Category**: cache correctness defect; invalid state collapsed into success.

Evidence:

- `needs_scan` invalidates only versions lower than the current one, so a row
  produced by a newer, incompatible scanner is accepted by older code
  (`src/processor/loudness_db.rs:351-367`).
- HTTP resources skip freshness checks entirely (`:369-391`); a stable URL can
  serve different content indefinitely without invalidating the measurement.
- For local files, failure of `std::fs::metadata` is silently ignored and the
  row is declared fresh (`:371-391`). Deleted, inaccessible, or non-file paths
  therefore look valid.
- The basic test creates no `/music/test.flac` fixture yet explicitly asserts
  that its inserted row does not need scanning (`:577-599`), so the missing-file
  policy is currently cemented as success rather than merely untested.
- Modification times are stored only to whole seconds (`:91-110`), so a same-
  size replacement within the timestamp granularity can evade detection.

Consequence:

Playback/analysis can reuse loudness and true-peak values for content that is
missing, replaced, remote-mutated, or generated by a scanner version this code
does not understand.

Direction:

Require `version == CURRENT_SCAN_VERSION`; distinguish not-found/unreadable from
fresh; add a remote validator identity (ETag/Last-Modified/content hash) or do
not claim HTTP freshness; and use a stronger local fingerprint when correctness
matters.

### P2 — track identity is neither canonical local identity nor safe URL identity

**Category**: ambiguous key ownership / collision and duplication risk.

`compute_track_id` only replaces separators and, on Windows, lowercases the
entire input (`src/processor/loudness_db.rs:113-130`). It does not canonicalize
relative versus absolute paths, `.`/`..`, links, or volume identity, so one
local file can occupy multiple rows. Conversely, the Windows branch also
lowercases HTTP paths/query strings even though those components can be case-
sensitive, so two distinct remote resources can collide on one row. URL
detection itself is repeated with a case-sensitive prefix (`:91-97`,
`:369-371`). Use the typed source identity proposed above and separate local
canonicalization from URL canonicalization/fingerprinting.

### P2 — loudness persistence returns strings and silently drops row errors

**Category**: weak error boundary; incomplete result reported as success.

Every `LoudnessDatabase` operation exposes `Result<_, String>` and repeatedly
formats I/O, mutex, migration, query, and transaction errors
(`src/processor/loudness_db.rs:161-250`, `:254-538`). This contradicts the
crate's typed-library error convention and prevents callers from distinguishing
busy/locked, corrupt schema, I/O, and invalid data. Worse,
`get_outdated_tracks` explicitly `filter_map`s row decoding errors away and
returns the remaining paths successfully (`:396-410`). A typed database error
should retain the rusqlite/I/O source and any row failure should fail the query
unless an explicit partial-result type reports omissions.

### P3 — track and album artist are conflated into one inaccurately named field

**Category**: metadata data loss / naming debt.

`TrackMetadata` has only `artist` (`src/decoder/metadata.rs:6-23`), and both
`StandardTag::Artist` and `StandardTag::AlbumArtist` compete to fill it using
first-value-wins (`:92-111`). Raw `artist`, `albumartist`, and `album_artist`
keys are similarly merged. A compilation/various-artists album therefore loses
one of two distinct identities. Add `album_artist` or document a deliberately
chosen precedence under a broader contributor model.

Related parsing gaps are common `"3/12"` track/disc strings and raw ISO date
strings: `tag_value_to_u32` accepts only a complete integer
(`src/decoder/metadata.rs:259-265`), while standard date tags use a separate
first-four-character parser. These should share explicit, tested tag policy.

### P3 — a one-entry `Cell` cache makes `TrackLoudness` non-`Sync` for a trivial conversion

**Category**: over-design with a hidden trait cost.

`TrackLoudness` embeds two `Cell`s to cache the last target-to-linear conversion
(`src/processor/loudness_db.rs:25-53`, `:132-147`). The computation is one
subtraction and `powf`, occurs off the realtime path, and the cache helps only
when identical targets repeat. The interior mutability makes an otherwise data-
record-like value non-`Sync`, complicates serialization/equality, and requires
manual cache initialization on every DB read (`:296-325`). Remove it unless a
profile demonstrates material benefit, or move caching to a clearly owned
consumer.

### P3 — cancellation ownership leaks its atomic implementation to every caller

**Category**: awkward public boundary.

`DecodeCancelToken` can only be constructed from caller-owned
`Arc<AtomicBool>` and exposes no `cancel()` operation
(`src/decoder/error.rs:156-172`). Callers must retain and mutate an implementation
detail with the correct ordering, while the token is merely a read handle. A
token/source pair or a clonable token with `cancel()` would encapsulate the
protocol and make cancellation tests/call sites less repetitive.

### P3 — one full-download warning contradicts the implemented safety check

When Content-Length is absent, the code warns that it is downloading “without
size check (may cause OOM)” (`src/decoder/source.rs:245-253`), but the following
loop enforces `max_download_bytes` after every 64 KiB append (`:277-300`). The
unknown-length path may exceed the cap by one chunk of temporary capacity, but
it is not unchecked. The warning should describe bounded streaming with a late
cap, otherwise operators may distrust a safeguard that does exist.

## Important non-findings / justified complexity

### Open/probe/build is a meaningful staged ownership boundary

`OpenedMediaSource` preserves the already-open local/Range source and its hint;
`StreamingDecoderBuilder` then exposes exact fixed staging bytes before codec
allocation. This supports player memory reservation without reopening a URL or
losing cancellation/credentials. The three stages are not needless wrappers.

### Borrowed, append-into, packet-owned, and decode-all APIs serve distinct allocation contracts

`decode_next_borrowed` avoids caller staging, `decode_next_into` appends into
reused storage, `decode_next` is an allocating convenience, and `decode_all`
enforces a whole-file budget. Their names and ownership differ materially; they
should not be collapsed merely to reduce method count. The size arithmetic in
`decode_all` still needs correction.

### Exclusive native-versus-fallback gapless ownership is justified

The MP3/Vorbis allowlist, true-start flag, realized seek timestamp, and
fallback padding counters prevent double trimming across codecs and seek. Tests
cover native ownership selection, start/end trim, and post-seek no-retrim. This
state machine is contract-driven, although its metadata must be made immutable.

### Range prefetch and a seekable media-source adapter are appropriate abstractions

A bounded prefetch window is necessary to give Symphonia `Read + Seek` over
HTTP without full download. The defect is that response geometry and steady-
state errors are not validated/preserved, not that a Range adapter exists.

### Standard and raw metadata fallbacks reflect real tag diversity

Trying Symphonia standard tags first and then common raw aliases is justified
for heterogeneous audio libraries. The repeated assignments could be factored,
but preserving first-value-wins and source precedence is more important than
minimizing lines.

### SQLite serialization, transactions, and additive schema inspection are reasonable

The cache is explicitly off-RT. A connection mutex, transactions for batch
updates, and `PRAGMA table_info` for additive migration are appropriate for a
small embedded database. String errors, duplicated SQL, and freshness identity
are the maintenance problems; the existence of these mechanisms is not.

### Per-thread FTZ/DAZ assembly is necessary platform code

MXCSR/FPCR are thread-local registers and must be set on the actual audio
thread. TLS idempotence and tiny target-specific assembly blocks are a good fit
for preventing denormal slowdowns. Only the unsupported-target logging branch
violates the callback contract.

## Test gaps exposed by this review

- local HTTP server cases for ignored/wrong/oversized/short Range responses and
  exact `Content-Range` validation;
- HEAD 405 with successful GET, auth/404/DNS/TLS propagation without false
  fallback, and bounded retry counts across initialization and later reads;
- cancellation during blocking send/body read and preservation of
  `DecoderError::Canceled` through Symphonia;
- log capture proving URL query/userinfo and Basic password never appear;
- large/unrepresentable track counts and 32-bit-safe memory-budget arithmetic;
- positioned layouts containing height/unknown channels, asserting known slots
  plus `Unspecified` rather than count-based speaker guesses;
- immutable decoder-info API or explicit rejection of builder geometry
  mutation; reset/seek/gapless tests should use private fixtures;
- distinct typed outcomes for truncated probe, unseekable source, disabled HTTP
  feature, and range transport errors;
- non-finite ReplayGain, `3/12` track/disc, ISO raw date, and separate track/
  album artist fixtures;
- loudness cache rows for deleted/unreadable local files, same-size rapid
  replacement, mutable HTTP URL content, future scan versions, and Windows
  case-sensitive URL identities;
- database row-decoding failure must not yield a silently partial outdated list;
- compile/runtime verification for the unsupported-architecture audio-thread
  fallback with an assertion that initialization emits no log or allocation.
