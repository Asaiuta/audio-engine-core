# Review Findings 2026-08-11 — Decoder & HTTP I/O

Source: decoder/streaming deep-review agent report from the 2026-08-11
six-track review, plus one discovery made during the 1.0.1 fix session.
A1 (metadata revision shadowing), A2 (Range body error classification), A3
(post-seek coordinate mixing), and A4 (Artist/AlbumArtist order) were fixed
in 1.0.1 and are omitted. Line numbers are pre-1.0.1.

## Design concerns

### B1 (medium, expectation management) — AAC/M4A gapless fallback is a no-op
Symphonia 0.6's isomp4 demuxer parses neither iTunSMPB nor edit-list delay
(`with_delay/with_padding`: zero call sites in its source), so
`track.delay/padding = None` ⇒ fallback counters stay 0 ⇒ M4A's 2112-frame
priming and end padding pass straight through. Upstream limitation.
**1.0.1 documented this in the README**; the remaining work is tracking
upstream and removing the caveat when isomp4 fills the fields — at which
point the (fixed-in-1.0.1) seek raw-coordinate path gets its first big
real-world trigger surface, so keep the CAF-derived regression tests.

### B2 (low-medium) — Silent geometry guesses
`streaming.rs:341-346`: `sample_rate.unwrap_or(44100)`,
`channels.unwrap_or(2)`. Wrong rate ⇒ wrong `duration_secs` fallback and
wrong no-time-base seek conversion; wrong channels ⇒ first packet errors with
a misleading "channel count changed from 2 to N". Prefer typed probe-time
rejection.

### B3 (low-medium) — Unknown total ⇒ no end-padding trim, undocumented
`streaming.rs:534`: `raw_total_frames.unwrap_or(u64::MAX)` — no-Xing streams
and unseekable sources never trim `end_padding` and there is no EOF-lookahead
fallback; not documented anywhere.

### B4 (low-medium) — Full-download fragility
`http.rs:229-272`: retry wraps only `send()` + status check; body-read
failures are not retried and force a full restart; 120 s total timeout dooms
big files on slow links; an unbounded no-Content-Length stream (Icecast)
consumes up to 256 MB or 120 s before erroring — no early unbounded-stream
classification.

### B5 (low) — Backoff ignores the cancel token
`error.rs:288`: `thread::sleep(1s/2s)` between retries; worst-case ~3 s
cancellation latency.

### B6 (low) — Negative seek passes through
`streaming.rs:665-666`: `Time::try_from_secs_f64` only rejects
non-finite/over-i64 (symphonia units.rs:580); negative seconds reach the
demuxer and surface as a stringly OutOfRange. No local guard; no
out-of-range seek tests at all.

### B7 (low) — `current_frame()` domain drift
Fallback path counts raw frames; native path counts presentation frames.
**1.0.1 made the fallback path uniformly raw (was mixed after seek) and
documented both domains in the rustdoc**; residual work is only deciding
whether a unified presentation-domain accessor is worth adding for
cross-codec UI use.

### B8 (low) — IPv6 SSRF list gaps
`http.rs:87-100` missing: 6to4 `2002::/16`, Teredo `2001::/32`, NAT64
`64:ff9b::/96`, deprecated site-local `fec0::/10`.

### B9 (low) — Test blind spots
No MP3/Vorbis native-gapless end-to-end fixtures (fixtures are all WAV; the
realtime-safety spec itself says MP3 must not be claimed corpus-verified
without a real LAME fixture, and calls for an enforced real Ogg/Vorbis seek
comparison); no seek past-duration tests; no seek(0)-replay semantic test;
`RangeStream` Read/Seek state machine (window hit, fallback seek, EOF) has
no unit tests; truncation is tested for WAV only — FLAC/MP3 take different
Symphonia error paths.

## Discovered during the 1.0.1 fix session

### Fixed-packet primed CAF `raw_total` flavor overcount
Symphonia's CAF **fixed**-packet path derives `Track::num_frames` from the
data-chunk size (raw frames, trim regions included: demuxer.rs:478-481)
while the **variable** path reports `pakt.valid_frames` (valid frames:
demuxer.rs:511). Our `raw_total = num_frames + delay + padding`
reconstruction (streaming.rs) is therefore exact for variable packetization
(all compressed CAF: AAC/ALAC — verified) and overcounts by `delay+padding`
for primed *uncompressed* CAF, whose end padding is then under-trimmed.
Both CAF paths stamp the first packet at `pts = -priming` (verified:
demuxer.rs:508 fixed; chunks.rs `current_frame` init variable), which is the
invariant the 1.0.1 seek fix relies on. A code comment at the
`raw_total_frames` computation records this gap. Options: upstream issue
asking for a uniform `num_frames` contract, or container-aware handling.

## Style notes routed to `08-11-style-docs-cleanup`

`streaming.rs:498-505` capacity()-vs-len() check fragility; `:474` corrupt
packet `continue` with zero diagnostics (at least `log::warn` + a counter);
`sample_buf: Option<Vec>` always-`Some` dead branches; `metadata.rs` raw
`"date"` full-string parse (misses "2023-05-01") and RG parser not accepting
uppercase "DB"; `http.rs:274-278` debug log prints initial capacity rather
than actual; `error.rs:294` `unreachable!` could be restructured away.
