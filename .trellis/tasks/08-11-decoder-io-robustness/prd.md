# Decoder and HTTP I/O Robustness

2026-08-11 full-code-review follow-up, batch 3 of 8. The decoder review
verified the load-bearing claims (gapless ownership exclusivity against
upstream sources, strict Range trust boundary, SSRF/credential hygiene,
checked memory budgets) and 1.0.1 fixed the three confirmed defects (metadata
revision shadowing, Range body error classification, post-seek raw-coordinate
accounting). This task addresses the remaining robustness and test-coverage
gaps.

## Goal

Replace the decoder's silent guesses with typed rejections, make the HTTP
full-download path survivable on imperfect networks, close the SSRF list
gaps, and build the test fixtures the gapless/seek/truncation contracts have
always claimed to deserve.

## What I Already Know

- **Silent geometry guesses** (`src/decoder/streaming.rs:341-346`):
  `sample_rate.unwrap_or(44100)` and `channels.unwrap_or(2)`. A wrong rate
  guess corrupts the `duration_secs` fallback and no-time-base seek math; a
  wrong channel guess later surfaces as a misleading "channel count changed
  from 2 to N" error. Should be a typed probe-time rejection.
- **Unknown total frames disables end-padding trimming silently**
  (`streaming.rs:534`, `raw_total_frames.unwrap_or(u64::MAX)`): no-Xing
  streams / unseekable sources get no padding trim and no EOF lookahead
  fallback; undocumented.
- **Fixed-packet primed CAF `raw_total` overcount** (found during the 1.0.1
  seek fix, documented in a code comment at the `raw_total_frames`
  computation): Symphonia's CAF fixed-packet path derives `num_frames` from
  data size (raw, includes trim regions) while the variable path reports
  `pakt.valid_frames` (valid) — our `valid + delay + padding` reconstruction
  is exact for variable (all compressed CAF) and overcounts for primed
  uncompressed CAF, under-trimming its padding. Needs either upstream
  clarification or container-aware handling.
- **Full-download fragility** (`src/decoder/source/http.rs:229-272`): retry
  wraps only `send()` + status; a body-read failure is not retried and
  restarts the entire download; the 120 s total timeout kills large files on
  slow links; a no-Content-Length unbounded stream (Icecast live) burns up to
  256 MB or 120 s before failing — no early "unbounded stream" detection.
- **Cancel latency during backoff** (`src/decoder/error.rs:288`):
  `thread::sleep(1s/2s)` between retries never polls the cancel token —
  worst-case ~3 s cancellation delay.
- **IPv6 SSRF list gaps** (`http.rs:87-100`): missing 6to4 `2002::/16`,
  Teredo `2001::/32`, NAT64 `64:ff9b::/96`, deprecated site-local
  `fec0::/10`.
- **Negative seek passthrough** (`streaming.rs:665`):
  `Time::try_from_secs_f64` rejects only non-finite/overflow (symphonia
  units.rs:580), so negative seconds reach the demuxer and come back as a
  stringly OutOfRange; no local typed guard, no out-of-range seek tests.
- **Test blind spots** (also mandated by `realtime-safety.md`'s gapless
  section): no real LAME-header MP3 fixture (spec: "MP3 may not be claimed
  corpus-verified until a real LAME fixture is present"), no Ogg/Vorbis
  native-path end-to-end seek comparison, no seek past-duration/at-duration
  tests, no compressed-format truncation tests (FLAC/MP3 take different
  Symphonia error paths than the tested WAV), no `RangeStream` Read/Seek
  state-machine unit tests (window hit, fallback seek, EOF).

## Research References

- [`research/review-findings-2026-08-11.md`](research/review-findings-2026-08-11.md)
  — findings B1-B9 and style notes from the decoder review report, plus the
  fixed-CAF discovery from the 1.0.1 fix session.

## Requirements

- Probe-time typed rejection (new `DecoderError` usage, no new variants
  unless unavoidable) for absent sample rate / channel count instead of
  44100/stereo defaults; document any deliberate remaining default.
- Document the unknown-total no-padding-trim behavior in `StreamingDecoder`
  rustdoc; if a cheap EOF-lookahead bound exists for seekable sources, note
  it as an option but do not build it in this task.
- Full-download path: wrap body reads in the retry policy with Range-style
  resumption if the server supports it (it reached this path because Range
  is unsupported — so restart-from-zero is the only resume; cap restarts),
  poll the cancel token inside backoff sleeps (chunked sleep), and fail fast
  with a typed error when Content-Length is absent and the stream exceeds a
  small probe budget without EOF (unbounded-stream detection).
- Extend `is_disallowed_ip` with the four missing IPv6 ranges + tests.
- Local typed rejection for negative/NaN seek seconds before demuxer
  dispatch; add seek tests at 0, mid, exactly-duration, past-duration,
  negative.
- Fixtures: add a real LAME-tagged MP3 and an Ogg/Vorbis fixture with the
  enforced native-seek comparison the spec calls for; add FLAC/MP3
  truncation tests; add `RangeStream` state-machine unit tests.
- CAF raw_total: file upstream issue or decide container-aware handling;
  until then keep the documented-gap comment accurate.

## Out of Scope

- Opus support, new codecs, or accurate-seek mode (explicitly out of the
  crate's contract).
- Loudness HTTP cache key design (in `08-11-loudness-aux-hardening`).
- Icecast/streaming playback features beyond early unbounded detection.

## Technical Notes

- Files: `src/decoder/streaming.rs`, `src/decoder/source/http.rs`,
  `src/decoder/error.rs`, `src/decoder/tests.rs`.
- Specs: `decoder-correctness.md` (typed boundaries, checked planning),
  `error-handling.md` (Range trust boundary table — the loopback-fixture
  test style is mandatory), `realtime-safety.md` (gapless regression list).
- The 1.0.1 chunked-truncation loopback pattern
  (`mid_body_transport_failure_keeps_its_structured_identity`) is the
  template for new transport-fault tests; the timing-based approach hangs on
  slow localhost resolution — do not reintroduce it.
- LAME fixture: generate once with `lame --nogap`-style encode in CI-free
  committed form (small, mono, short), or synthesize the Xing/LAME header;
  record provenance beside the fixture.

## Implementation Plan

1. Typed geometry rejection + seek guards + their tests.
2. SSRF IPv6 additions + tests.
3. Full-download retry/cancel/unbounded-detection rework + loopback tests.
4. Fixture build-out (LAME MP3, Vorbis, truncated FLAC/MP3, RangeStream
   units).
5. CAF raw_total decision (upstream issue vs container-aware fix).
6. Both feature matrices + full suite.
