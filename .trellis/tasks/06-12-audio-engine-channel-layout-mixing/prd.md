# Channel Layout Mixing and Mapping

## Goal

Add explicit channel-layout metadata, mapping, and downmix/upmix policy so multichannel material (e.g. 5.1/7.1) is handled with correct channel order and predictable mixing instead of implicit per-module assumptions.

## Requirements

- Audit how channel count and order are currently assumed across `decoder/metadata.rs`, `decoder/streaming.rs`, `processor/dsp_chain.rs`, `processor/adapters.rs`, and `processor/loudness/meter.rs`.
- Define a channel-layout representation (channel count + ordered channel roles) carried from decode through the DSP chain.
- Implement a downmix policy (5.1/7.1 to stereo and to mono) with documented coefficients and channel-order handling.
- Provide **two selectable downmix coefficient sets** the frontend can choose between: **ITU-R BS.775** (broadcast standard, LFE discarded) and **ATSC A/85** (cinema-style, LFE folded in with headroom management). Expose the choice as a `DownmixCoefficients` enum; default to ITU-R BS.775. The enum is extensible so a third set can be added later without breaking the API.
- Compute the coefficient matrix for the selected set at configuration time (outside the hot path); the audio callback performs only the matrix multiply (zero-allocation, lock-free).
- Define behavior for upmix or passthrough when no layout mapping is requested.
- Add multichannel validation rules so DSP modules that assume mono/stereo either handle the layout or fail with a clear error rather than silently corrupting channels.
- Keep the audio callback path allocation-free and lock-free; precompute mixing coefficients/layout outside the hot path.
- Document channel-order assumptions and the chosen downmix coefficients so API users can reason about results.

## Acceptance Criteria

- [x] Channel layout metadata flows from the decoder through the DSP chain and is queryable.
- [x] Downmix from 5.1 and 7.1 to stereo and to mono is implemented with documented coefficients and tested against known inputs.
- [x] Both coefficient sets (ITU-R BS.775 and ATSC A/85) are selectable via the `DownmixCoefficients` enum and each is tested against known inputs; switching the selection changes the output predictably.
- [x] Channel-order correctness is asserted (a signal placed in one source channel lands in the expected output channel).
- [x] DSP modules that require a specific layout validate input and return a clear error on mismatch instead of corrupting audio.
- [x] Tests cover mono, stereo, 5.1, and 7.1 layouts.
- [x] Realtime processing tests assert no steady-state allocation for the mixing path.
- [x] Loudness measurement on multichannel input uses the correct per-channel weighting.

## Validation Commands

- `cargo test --lib`
- `cargo test processor::adapters --lib`
- `cargo test processor::loudness --lib`
- `cargo test decoder --lib`
- `cargo clippy --all-targets -- -D warnings`

## Decision (ADR-lite)

**Context**: The DSP chain passes only a channel *count* (`AudioProcessor::process(buffer, channels: usize)`, `traits.rs:68`); there is no positional/role type. Supporting downmix requires representing source/destination layouts. Two API shapes were considered: (1) an additive `Downmixer`/layout stage that runs before the chain while the trait keeps seeing a count, vs (2) a breaking change carrying a layout type through `process()` across all ~10 impls.

**Decision**: Approach 1 (additive). Introduce a `ChannelLayout` type plus a standalone downmix/layout stage that runs *before* the DSP chain. `AudioProcessor::process(buffer, channels: usize)` is unchanged. Layout is carried as read-only, queryable metadata from the decoder.

**Consequences**:
- Zero trait breakage — no churn across the ~10 production impls, the doc example (`traits.rs:38`), or test impls. No merge conflict with parallel `feature-upgrade` subtasks also touching `adapters.rs`.
- Downmix coefficients and channel-order handling live in one auditable place.
- Per-processor channel-role awareness is *not* added (the audit confirms every current module is already channel-count-aware and needs no role info). If a future module genuinely needs roles, it can be added via a default-method on the trait (e.g. `fn set_layout(&mut self, _: &ChannelLayout) {}`) without breaking existing impls — a monotonic upgrade path, never a rewrite.

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
