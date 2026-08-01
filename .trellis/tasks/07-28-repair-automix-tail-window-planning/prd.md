# Repair AutoMix tail window planning

## Goal

Make Full-mode AutoMix analyze the track tail whenever the head window does not
cover the complete known track. Plan non-overlapping frame windows, ensure every
metric consumes the same exact selected frames, and keep absolute tail timing as
explicit segment metadata rather than reconstructing it from vector lengths.

## Revalidation verdict

The 2026-07-28 audit finding is accurate. The head always decodes one configured
window, while a tail is decoded only for `duration > 2 * window`. For
`window < duration <= 2 * window`, Full mode passes an empty tail into
finalization, so the final active head sample is treated as the absolute track
fade-out and drives cut/mix positions even though program material remains.

The same boundary contains two adjacent inconsistencies worth fixing together:

- Simply seeking the last full window for shorter tracks would overlap the head
  and double-count loudness.
- `decode_segment` currently sends an entire final decoder packet to the
  loudness meter but stops envelope/spectral processing at `max_frames`, so
  metrics do not describe the same selected prefix.
- Silence, vocal, and energy-profile paths each infer tail start independently
  as `duration - vector_length / rate`, which is biased by partial envelope
  windows and duplicates timeline ownership.

## Requirements

- Plan in integer frames using `AudioInfo::total_frames` when available, with
  finite duration converted to frames only as a fallback.
- Head covers `[0, min(track_frames, window_frames))` for known tracks and up to
  `window_frames` when length is unknown.
- Full mode gets a tail whenever known `track_frames > head.end`.
- Tail start is `max(head.end, track_frames - window_frames)`, yielding:
  - the remaining non-overlapping suffix for tracks up to two windows;
  - the final full window with an intentional unanalysed middle gap for longer
    tracks.
- Exactly two windows are non-overlapping and both are analyzed.
- Coarse seek preroll before the planned tail start is skipped before any
  metric consumes samples.
- Decoder packets crossing skip/take boundaries are sliced once; loudness,
  envelopes, vocal ratios, and spectral flux consume the identical frame set.
- `AnalysisSegment` owns its absolute `start_time`; silence/vocal/energy output
  uses that value rather than reverse-engineering it from vector length.
- Unknown/invalid duration retains the current bounded head-only behavior; no
  end-relative seek is possible without a known track length.
- Public DTO fields and `ANALYSIS_VERSION = 2` remain unchanged.

## Refactor scope

Introduce small private frame-window planning types/functions and make
`decode_segment` accept exact `skip_frames`/`take_frames`. Keep the existing
feature extraction, BPM, silence, vocal, cut, mix, and energy algorithms.

Do not redesign AutoMix heuristics, add a key detector, change public error
types, or alter the decoder's documented coarse-seek API. Those are independent
contracts and would obscure whether the interval fix is correct.

## Acceptance Criteria

- [x] Full mode plans and analyzes a non-overlapping tail immediately above one
      window, at exactly two windows, and above two windows.
- [x] Head mode and tracks no longer than one window plan no tail.
- [x] Coarse-seek preroll and a packet crossing the take boundary cannot enter
      one metric without entering all others.
- [x] Absolute `fade_out_pos`, `cut_out_pos`, and `mix_center_pos` reflect final
      program material for end-to-end WAV fixtures at all three boundaries.
- [x] Tail start is explicit and reused by silence, vocal, and energy-profile
      placement.
- [x] The implementation reduces duplicated timeline arithmetic without
      changing the public AutoMix schema.
- [x] Focused tests, both supported strict Clippy/test matrices, rustfmt, diff
      check, and Trellis validation pass.
- [x] The final review records adopted and rejected broader refactors.

## Definition of Done

- One pure planner owns head/tail frame intervals.
- One decode loop owns packet skip/take slicing for every metric.
- Regression coverage includes window-boundary planning, exact prefix slicing,
  and end-to-end absolute positions.
- The AutoMix code-spec captures interval and metric-consistency contracts.
- Existing unrelated dirty work remains untouched.
- No commit, push, or archive occurs without the user's explicit direction.

## Out of Scope

- Full-track analysis for long tracks; the bounded head/tail budget remains.
- Accurate seeking or changes to `StreamingDecoder::seek`.
- Public typed AutoMix errors/cancellation changes.
- AutoMix heuristic tuning or performance claims.
