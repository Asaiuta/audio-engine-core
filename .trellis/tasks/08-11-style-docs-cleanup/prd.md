# Style and Documentation Cleanup

2026-08-11 full-code-review follow-up, batch 8 of 8 (P3). Roughly twenty
style-level findings across six review reports: dead code, names and comments
that say something other than what the code does, parser edge cases, and
missing diagnostics. None affects correctness; each is a small honesty or
readability repair. Suitable as a single mechanical sweep or as drive-by
fixes whenever the owning file is next touched.

## Goal

Make identifier names, comments, and small control-flow structures tell the
truth, and delete code that cannot execute.

## Item List

### Dead code
- `src/processor/dsp.rs:378-388, 409-419` — `process_sample_with_taps`'s
  `TAPS==9` branch never instantiates (9-tap always routes to
  `process_sample_9tap_ring`); `process_sample_lipshitz5/tpdf_only/9tap`
  are forward-only aliases.
- `src/decoder/streaming.rs` — `sample_buf: Option<Vec<f64>>` is `Some` from
  construction and never `take`n: two `None` arms are dead and the `Option`
  layer is spurious.
- `src/processor/resampler/mod.rs:357-363, 395-404` — length-mismatch
  zero-fill fallback branches in `interleave_channel_outputs_to_*` are
  unreachable (upstream enforces per-channel progress equality).

### Misleading names/comments
- `src/processor/dynamic_loudness.rs:249` — comment says "Direct Form I";
  implementation is transposed direct form II.
- `src/processor/loudness_db.rs:897-903` — `chrono_timestamp` uses
  `std::time`, not chrono.
- `src/processor/loudness/meter.rs:39,146` — `samples_processed` counts
  frames.
- `src/processor/dynamic_loudness.rs:331` — `samples_remaining = usize::MAX`
  as a boolean sentinel.
- `src/processor/loudness/limiter.rs` — `is_enabled()` constitutionally
  true; adapter comment "If enabled state changed" describes a different
  condition (semantic half is tracked in `08-11-realtime-lifecycle-polish`;
  the naming/comment half lands here if that task defers).
- Resampler docs — "SoX VHQ" used as a blanket backend label while the
  default High tier maps to Bits20/HQ; only UltraHigh is VHQ.

### Structure/readability
- `src/processor/saturation.rs:1175-1180` — `use` statements after 1100+
  lines of implementation.
- `src/processor/saturation.rs:62-80` — `OVERSAMPLING_2X_FILTER` carries
  ulp-level coefficient asymmetry from numeric export; symmetrize since the
  suite asserts bit-level properties elsewhere.
- `src/processor/eq.rs` — `set_band_gain` re-takes `sample_rate` per call
  with no cross-check against construction; storing the rate removes a
  caller-error class (evaluate against the validated-kernel pattern before
  changing signatures — crate-private only).
- `src/processor/fir_eq.rs:17-28` — `(f64, f64)` tuples for band
  (frequency, gain); a named pair type reads better than `.1`.
- `src/processor/resampler/mod.rs:54-88` and backend routing — heavy
  per-variant `#[cfg(feature)]` duplication; an internal trait/type alias
  could collapse it. `rubato_backend.rs:122` `should_use_fft(_quality)`
  unused parameter; `:94-101` zero-rate routed through a constructor error
  by returning `true` — works, reads oddly.
- `src/processor/resampler/rubato_backend.rs:1774` — bare
  `(expected_total - emitted) as usize` vs the `usize::try_from` style at
  `:1237`.
- `src/decoder/streaming.rs:498-505` — capacity check uses `capacity()`
  while slicing depends on `len()`; equal today (`vec![0.0; n]`), fragile
  under refactor — unify on `len()`.
- `src/decoder/error.rs:294` — `unreachable!` guaranteed by a constant
  relationship; restructure the retry loop to remove the panic point.

### Diagnostics & parsing edges
- `src/decoder/streaming.rs:474` — corrupt-packet `DecodeError => continue`
  with no log and no counter: heavily damaged files skip silently to EOF
  with zero diagnostics. Policy allows the skip; add `log::warn!` (non-RT
  path) and/or a skipped-packet counter on `AudioInfo`/stats.
- `src/decoder/metadata.rs:215-217` — raw `"date"` key parses the whole
  string as `u32` ("2023-05-01" yields nothing) vs the standard-tag path's
  four-char year prefix; `parse_rg_gain_str` accepts "dB"/"db" but not
  "DB".
- `src/decoder/source/http.rs:274-278` — debug log prints intended initial
  capacity rather than the actual buffer state.
- `src/processor/loudness.rs:41` — module test `test_peak_limiter` drives
  crate-internal `new_validated` in a position that reads like a public
  example.

## Requirements

- Behavior-preserving except where the item is *about* behavior-adjacent
  diagnostics (corrupt-packet warn/counter — additive only).
- Public API untouched; run both public-API snapshot tests to prove it.
- Each deleted "dead" branch gets a one-line justification in the commit
  message (why it was unreachable), so the deletion is reviewable.

## Out of Scope

- Anything with a behavioral decision attached (those live in batches 1-7).

## Implementation Plan

Single sweep in file order, `cargo fmt`/clippy/full matrix at the end; or
absorb items opportunistically into batches 1-7 as their files get touched,
checking items off this list either way.
