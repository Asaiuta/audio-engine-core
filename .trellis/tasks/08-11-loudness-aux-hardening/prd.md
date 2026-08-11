# Loudness Auxiliary Hardening

2026-08-11 full-code-review follow-up, batch 6 of 8. The gain-trajectory
items went to batch 1; this task holds the remaining loudness-stack findings:
input clamping, persistence robustness, memory scaling, engine efficiency,
and two documentation debts.

## Goal

Harden the loudness stack's non-realtime edges: bound pathological gains,
make the SQLite layer concurrency-tolerant, validate persisted values, and
record the deliberate design envelopes (streaming AGC, HTTP cache policy) so
they read as decisions rather than accidents.

## What I Already Know

- **Track-mode normalization gain is unclamped**
  (`src/processor/loudness/normalizer.rs:156` checks finiteness only) while
  Streaming mode clamps to ±20 dB (`:299`). A finite-but-absurd measurement
  (-60 LUFS) yields +46 dB straight into the limiter — flattened noise, not
  music. Clamp Track mode to the same published bound (or a documented
  wider one).
- **`TrackLoudness::new` accepts non-finite LUFS**
  (`src/processor/loudness_db.rs`): -inf integrated loudness round-trips the
  database and becomes +inf gain downstream, relying on consumers to defend.
  Validate at construction/insert.
- **No SQLite `busy_timeout`**: cross-process concurrent access fails
  immediately with SQLITE_BUSY instead of briefly waiting; also cache
  freshness compares mtime truncated to whole seconds (sub-second rewrites
  invisible). Both are one-line hardening items.
- **`analyze_track` requires the whole track resident as interleaved f64**
  (~220 MB for 5-minute stereo 48 kHz): fine for the current callers, but
  the API shape invites misuse on long material; either document the bound
  or add a streaming analysis entry point.
- **OverlapSave engine efficiency** (`src/processor/convolver.rs:303-334`):
  complex FFT for real signals (2× waste) and `fft_size ≥ 2×ir_len`
  regardless of callback block size — a 4096-tap IR at 128-frame callbacks
  does two 8192-point complex FFTs per callback per channel below the
  partitioned-path threshold. Functionally verified correct; throughput
  tradeoff worth a real-FFT or threshold revisit with bench evidence.
- **HTTP loudness cache is write-only under current policy** (documented
  honest): `needs_scan` returns stale for validator-less HTTP
  (`loudness_db.rs:705-707`) and the cache key hashes the full URL including
  signed query (`:66-70`), so `get_fresh` is永-None for HTTP. If
  ETag/Last-Modified support ever lands, the key must simultaneously drop
  the query component — the two changes are coupled; record that here so
  they do not land separately.
- **Streaming mode is a short-term AGC** (3 s window; 400 ms already counts
  as `has_reliable_measurement`): it follows musical dynamics within its
  ±20 dB clamp — pumping by design, and early values are high-variance. Not
  a defect; needs public rustdoc stating it is not R128 normalization.

## Research References

- [`research/review-findings-2026-08-11.md`](research/review-findings-2026-08-11.md)
  — findings S1-S5 and supporting verification context from the loudness
  review report.

## Requirements

- Track-mode gain clamp with a test at an extreme measured LUFS; constant
  sourced from the published bound, not a literal.
- `TrackLoudness` finiteness validation (typed error), plus a DB round-trip
  test proving non-finite never persists.
- `busy_timeout` (a few hundred ms) on connection open + a two-connection
  contention test; document the mtime second-granularity caveat.
- `analyze_track`: document the memory bound in rustdoc now; streaming
  variant only if a concrete consumer exists (defer otherwise, note here).
- OverlapSave: measure real-FFT conversion and/or partition-threshold
  adjustment; adopt only with `audio_convolver_perf` evidence; keep the
  1e-8 oracle equivalence tests green.
- Rustdoc: streaming-AGC semantics; HTTP cache coupled-change note.

## Out of Scope

- Gain trajectory items (batch 1).
- R128 measurement internals (delegated to `ebur128`, verified correctly
  wired incl. the 7.1 `with_layout` fix).
- New cache validators (ETag) — only the coupling note.

## Technical Notes

- Files: `src/processor/loudness/normalizer.rs`, `src/processor/
  loudness_db.rs`, `src/processor/automix_analysis.rs`,
  `src/processor/convolver.rs`.
- Specs: `database-guidelines.md` (identity/freshness contracts),
  `quality-guidelines.md` (bench-evidence rule for the engine change).
- The DB layer's secret-hygiene properties (hashed keys, origin-only labels,
  DROP+VACUUM migration) were verified byte-level in review — preserve
  their tests untouched.

## Implementation Plan

1. Clamps + validation + DB hardening (items 1-3) with tests.
2. Documentation batch (AGC, cache coupling, memory bound).
3. OverlapSave measurement spike; adopt or record numbers and close.
