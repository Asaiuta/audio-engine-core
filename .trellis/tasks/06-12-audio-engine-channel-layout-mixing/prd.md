# Channel Layout Mixing and Mapping

## Goal

Add explicit channel-layout metadata, mapping, and downmix/upmix policy so multichannel material (e.g. 5.1/7.1) is handled with correct channel order and predictable mixing instead of implicit per-module assumptions.

## Requirements

- Audit how channel count and order are currently assumed across `decoder/metadata.rs`, `decoder/streaming.rs`, `processor/dsp_chain.rs`, `processor/adapters.rs`, and `processor/loudness/meter.rs`.
- Define a channel-layout representation (channel count + ordered channel roles) carried from decode through the DSP chain.
- Implement a downmix policy (e.g. 5.1/7.1 to stereo and to mono) with documented coefficients and channel-order handling.
- Define behavior for upmix or passthrough when no layout mapping is requested.
- Add multichannel validation rules so DSP modules that assume mono/stereo either handle the layout or fail with a clear error rather than silently corrupting channels.
- Keep the audio callback path allocation-free and lock-free; precompute mixing coefficients/layout outside the hot path.
- Document channel-order assumptions and the chosen downmix coefficients so API users can reason about results.

## Acceptance Criteria

- [ ] Channel layout metadata flows from the decoder through the DSP chain and is queryable.
- [ ] Downmix from 5.1 and 7.1 to stereo and to mono is implemented with documented coefficients and tested against known inputs.
- [ ] Channel-order correctness is asserted (a signal placed in one source channel lands in the expected output channel).
- [ ] DSP modules that require a specific layout validate input and return a clear error on mismatch instead of corrupting audio.
- [ ] Tests cover mono, stereo, 5.1, and 7.1 layouts.
- [ ] Realtime processing tests assert no steady-state allocation for the mixing path.
- [ ] Loudness measurement on multichannel input uses the correct per-channel weighting.

## Validation Commands

- `cargo test --lib`
- `cargo test processor::adapters --lib`
- `cargo test processor::loudness --lib`
- `cargo test decoder --lib`
- `cargo clippy --all-targets -- -D warnings`

## Out of Scope

- Format/codec support boundaries (owned by `06-12-audio-engine-decoder-format-capability`).
- Spatial/binaural rendering or HRTF-based virtualization.
- EQ/dynamic-loudness/crossfeed quality changes (owned by `06-12-audio-engine-eq-perceptual-dsp`).
- Device output channel routing outside this crate.

## Technical Notes

- This covers correctness that individual DSP modules do not solve on their own: layout metadata and channel-order assumptions.
- EBU R128 loudness weighting is channel-dependent; multichannel handling here must align with the meter's expectations.
- Depends conceptually on layout metadata surfaced by the decoder task; this task owns the mixing/mapping policy.
- Source anchors: `src/decoder/metadata.rs`, `src/decoder/streaming.rs`, `src/processor/loudness/meter.rs`, `src/processor/dsp_chain.rs`, `src/processor/adapters.rs`.
- Shared audit: `../06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
