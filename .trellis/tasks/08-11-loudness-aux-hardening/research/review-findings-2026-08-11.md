# Review Findings 2026-08-11 — Loudness Auxiliary

Source: loudness/convolution deep-review agent report from the 2026-08-11
six-track review. D1 (constructor validation) was fixed in 1.0.1; D2-D5 are
tracked in `08-11-gain-trajectory-continuity`; D6 (Automix Full rustdoc)
was fixed in 1.0.1. Line numbers pre-1.0.1.

## S1 — Track-mode gain unclamped
`normalizer.rs:156` finiteness-only, vs Streaming's ±20 dB clamp at `:299`.
A finite -60 LUFS track yields +46 dB into the limiter: squashed noise.
Clamp like Streaming.

## S2 — HTTP loudness cache is effectively write-only
`needs_scan` returns stale for HTTP without a validator
(`loudness_db.rs:705-707`, documented) and the cache key hashes the whole
URL including signed query (`:66-70`) so key identity rotates with
signatures. Together: `get_fresh` never hits for HTTP. Security posture
correct and documented; the cache value for HTTP is zero. If ETag/
Last-Modified lands, the key must simultaneously stop hashing the query —
coupled changes.

## S3 — OverlapSave engine efficiency
`convolver.rs:303-334`: complex FFT on real signals (2× waste); `fft_size ≥
2×ir_len` independent of callback block size — 4096-tap IR at 128-frame
callbacks: two 8192-point complex FFTs per callback per channel (below the
partitioned threshold). Correctness verified (alignment, normalization,
oracle equivalence ≤1e-8); throughput tradeoff only.

## S4 — Streaming mode is a 3-second-window AGC
Normal musical dynamics are followed (within the ±20 dB clamp): pumping by
design; `has_reliable_measurement` is true after 400 ms when short-term
variance is still high. Needs rustdoc stating it is not R128-sense
normalization.

## S5 — Miscellany
- `analyze_track` requires whole-track interleaved f64 in memory
  (5-minute stereo 48 kHz ≈ 220 MB).
- loudness_db mtime truncated to seconds; no SQLite `busy_timeout`
  (cross-process concurrency ⇒ immediate SQLITE_BUSY).
- `TrackLoudness::new` does not validate `integrated_lufs` finiteness
  (-inf would persist and produce +inf gain at consumers).
- `TruePeakDetector` has no flush: the final ~6 samples' intersample
  interval is unevaluated (matches the reference implementation; negligible
  — no action, recorded for completeness).

## Verified-good context (preserve)

True-peak = real 49-tap Hann-windowed 4× polyphase FIR shared by meter and
limiter; limiter sliding-window timing derivation closed with exact
`delay_frames()` reporting; partitioned convolution alignment/normalization/
Bresenham quantum scheduling closed with bitwise chunking-invariance tests;
DB secret hygiene (domain-separated SHA-256 keys, origin-only labels,
DROP+VACUUM migration wipe) verified at the raw-byte level; 7.1 channel
weighting fixed via explicit `with_layout` mapping (+1.5 dB surround per
BS.1770-4 table 4).

## Style notes routed to `08-11-style-docs-cleanup`

`dynamic_loudness.rs:249` comment says "Direct Form I", implementation is
TDF-II; `loudness_db.rs:897-903` `chrono_timestamp` uses std::time;
`meter.rs:39/146` `samples_processed` counts frames;
`dynamic_loudness.rs:331` `usize::MAX` boolean sentinel;
`loudness.rs:41` module test drives internal `new_validated` in a
public-example position.
