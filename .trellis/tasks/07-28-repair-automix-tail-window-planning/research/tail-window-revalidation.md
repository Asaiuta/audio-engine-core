# AutoMix tail-window revalidation

## Snapshot and verdict

- Revalidated on 2026-07-28 against current `automix_analysis.rs`.
- The audit finding remains accurate: tail decoding is gated by
  `duration > 2 * max_analyze_time_sec`, while Full-mode finalization interprets
  an empty tail as though the head reached the actual end.
- A 90-second track with a 60-second window can therefore report fade/cut/mix
  near 60 seconds despite 30 seconds of remaining program. Exactly 120 seconds
  is also skipped by the strict greater-than condition.

## Additional evidence

- Seeking the final full window for every `duration > window` would overlap the
  head for shorter tracks and double-count the shared `LoudnessMeter`.
- `decode_segment` currently calls `meter.process(&chunk)` before its per-frame
  `max_frames` check. A decoder packet crossing the window end contributes its
  suffix to loudness/true peak but not envelopes, spectral flux, or vocals.
- `detect_silence`, `detect_vocals`, and `build_energy_profile` independently
  calculate `duration - tail.len() / rate`. Envelope accumulators emit only
  complete 20 ms blocks, so vector length is not an authoritative segment
  origin.
- `StreamingDecoder::seek` is coarse and exposes `current_frame()`. It lands at
  or before the target, so the analyzer must skip seek preroll before analysis.

## Refactor decision

Use an integer-frame `AnalysisWindowPlan` and an exact packet slicer. The plan
keeps head and tail disjoint; the slicer drops coarse-seek preroll and truncates
the final packet before every feature consumer. Store the planned/realized
segment start once on `AnalysisSegment` and reuse it for absolute placement.

Reject changing Full mode to full-track decoding: bounded analysis is an
intentional cost contract. Reject a public DTO version change because no field
shape or meaning is newly introduced; existing positions are corrected to the
audio intervals they already claim to represent. Reject changing decoder seek
mode because precise interval selection can be enforced in the analyzer after
the existing coarse seek.

## Required tests

- Pure plan cases for `duration <= window`, just above one window, exactly two
  windows, and above two windows in both Head and Full modes.
- Packet slicing with both leading skip and trailing take inside one chunk,
  asserting the meter/features receive the same selected frames.
- End-to-end PCM WAV fixtures whose final active audio ends near track end at
  just above one window, exactly two windows, and above two windows; assert
  absolute fade/cut/mix positions, not only DTO shape.
- Existing tempo, key-status, silence, loudness, and serialization tests remain
  green under both feature matrices.

## Implemented result

- `AnalysisWindowPlan` is the single owner of integer head/tail intervals.
  `AudioInfo::total_frames` wins over finite-duration fallback, and the planner
  produces `[60,61)`, `[60,120)`, and `[61,121)` tails for 61/120/121-frame
  tracks with a 60-frame window.
- `decode_segment` now accepts exact skip/take frame counts. It computes one
  packet subrange and passes only that interleaved slice to `SegmentAnalyzer`,
  which fans the same frames into loudness, envelope, vocal, and spectral
  consumers.
- Tail seek uses `StreamingDecoder::current_frame()` to discard coarse preroll.
  A realized position after the planned start is rejected instead of silently
  shifting the analysis interval.
- `AnalysisSegment` owns `start_time` and `frames_analyzed`. Production silence,
  vocal, and energy-profile placement no longer derives a tail origin from a
  feature-vector length; unknown-duration fallback uses realized frames.
- The public `detect_silence` helper retains its existing signature and legacy
  inferred-origin behavior for callers, while production finalization uses the
  explicit-origin internal path. Public DTOs and `ANALYSIS_VERSION = 2` are
  unchanged.

## Refactor review

Adopted the planner and `SegmentAnalyzer` because they remove three competing
timeline owners and make the metric-scope invariant structural. Kept them
private and colocated with AutoMix because no other module has the same bounded
analysis contract.

Rejected splitting the file or extracting a generic media-window utility: the
types have one consumer and a separate module would add navigation without
removing another dependency or fact source. Also rejected full-track decoding,
decoder seek changes, public error/schema redesign, envelope-algorithm changes,
and BPM/cut heuristic tuning as independent concerns. The first end-to-end
fixture used a steady sine that exercised beat snapping; it was replaced with
a stable non-periodic level so the regression isolates interval ownership.

## Verification

- Focused AutoMix tests pass under all-features and Rubato-only: 11 tests each.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`
  passes.
- `cargo test --all-features --no-fail-fast` passes: 407 library, 20
  benchmark-support, 25 resampler-support plus 1 explicit native-shim ignore,
  3 Windows deployment, and 6 doctests.
- `cargo test --no-default-features --features rubato --no-fail-fast` passes:
  440 library, 20 benchmark-support, 25 resampler-support plus the same 1
  explicit native-shim ignore, 3 Windows deployment, and 6 doctests.
