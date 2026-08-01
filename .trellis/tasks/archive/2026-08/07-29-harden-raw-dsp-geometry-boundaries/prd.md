# Harden raw DSP geometry boundaries

## Goal

Give exported standalone DSP processors the same typed, allocation-free
geometry boundary as the canonical callback layer. Invalid setup or block
geometry must be rejected before a raw kernel divides, indexes, mutates DSP
history, or silently drops an incomplete frame.

## Revalidation verdict

Audit finding 6 is accurate in the current tree, with one correction.
`VolumeController` divides by caller channels; `DynamicLoudness`,
`PeakLimiter`, and `LoudnessNormalizer` divide by a constructor-supplied count;
all can accept zero. Their slice APIs also silently ignore an incomplete final
frame. `SpectrumAnalyzer` accepts FFT/bin combinations whose empty magnitude
domain later reaches an invalid `usize::clamp` bound. `NoiseShaper` still
divides by zero and silently truncates incomplete frames, but a process-time
channel count larger than setup no longer indexes past state: its existing
per-sample guard bypasses unknown channels. That bypass is still a split
contract and must become a typed mismatch error.

## Requirements

- Reuse `AudioBlockMut`, `AudioBlockError`, and `ProcessError`; do not add a
  competing public geometry DTO or error enum.
- Centralize configured-versus-actual channel validation so adapters and raw
  checked shells use the same implementation owner.
- Make public raw constructors reject unusable zero channel/sample-rate setup
  before allocating or initializing DSP state.
- Make public raw processing for `VolumeController`, `NoiseShaper`,
  `DynamicLoudness`, `PeakLimiter`, and `LoudnessNormalizer` accept explicit
  block channels and return typed errors for zero channels, incomplete frames,
  and configured-channel mismatches.
- Keep the inner DSP kernels crate-private and callable only after geometry has
  been validated. Canonical adapters must use those kernels after their
  existing block validation so the hot path does not validate twice.
- Make `SpectrumAnalyzer` construction reject FFT sizes with no usable
  non-DC/non-Nyquist magnitude and zero bins. Analysis must reject a zero sample
  rate without mutating cached FFT/bin state.
- Preserve valid-input DSP output, smoothing, limiter delay/tail behavior,
  noise-shaping state, dynamic-loudness state, and spectrum values.
- Preserve realtime constraints: no allocation, deallocation, lock, logging,
  I/O, panic, or unbounded work in a valid process call or its error path.

## Acceptance Criteria

- [x] Every affected constructor rejects its invalid setup with `ProcessError`
      before a raw processing kernel can run.
- [x] Raw public process calls reject zero channels, incomplete frames, and
      configured-channel mismatches without mutating samples or processor
      state.
- [x] `NoiseShaper` mismatch is a typed error rather than a partial processed /
      partial bypass block.
- [x] Spectrum construction rejects FFT sizes below four and zero bins;
      analysis rejects a zero sample rate without panic or cache mutation.
- [x] Existing valid-output tests remain bit-identical, and focused regression
      tests cover every affected raw processor and the shared validator.
- [x] Adapters use crate-private validated kernels and retain their existing
      typed geometry behavior and no-allocation guarantees.
- [x] Both strict feature matrices pass Clippy and tests; rustfmt, focused diff
      check, and Trellis validation pass.
- [x] Final review records adopted and rejected broader refactors.

## Definition of Done

- Invalid public geometry is not representable as an apparently successful raw
  setup/process call.
- One shared helper owns configured-versus-actual channel mismatch policy.
- Checked public shells and crate-private kernels have explicit names and
  responsibilities.
- The spec documents raw setup/process geometry alongside adapter geometry.
- Existing unrelated dirty work remains untouched.
- No commit, push, or archive occurs without explicit user direction.

## Decision (ADR-lite)

**Context**: The crate already has a validated interleaved block abstraction and
typed process errors, but exported standalone processors bypass them. Adapters
validate blocks and then call raw methods whose signatures still accept invalid
geometry.

**Decision**: Public raw APIs become checked, fallible shells. They validate
with the existing block/error contract and then call crate-private
`process_validated` kernels. Adapters call those kernels only after their shared
validated-block driver has enforced the same contract.

**Consequences**: This is an intentional source-level API tightening in the
pre-1.0 crate. Direct callers must handle setup/process errors and provide the
block channel count explicitly. Valid callback work stays allocation-free and
does not pay duplicate validation in adapters.

## Out of Scope

- Non-finite or out-of-range control values from later audit findings.
- `LoudnessMeter` backend initialization/reliability state.
- Redesigning `CallbackSpec`, `StreamingProcessor`, or processor capability
  modeling.
- Changing DSP algorithms, defaults, coefficient design, latency, or tail.
- Compatibility aliases that leave an unchecked public method beside the new
  checked API.

## Technical Notes

- Primary code: `src/processor/traits.rs`, `src/processor/adapters.rs`,
  `src/processor/dsp.rs`, `src/processor/dynamic_loudness.rs`,
  `src/processor/loudness/limiter.rs`,
  `src/processor/loudness/normalizer.rs`, and `src/processor/spectrum.rs`.
- Call sites and focused tests in those modules, adapters, output-chain setup,
  examples, and benchmark support must compile against the fallible API.
- Contracts: `.trellis/spec/backend/realtime-safety.md`,
  `.trellis/spec/backend/streaming-lifecycle.md`,
  `.trellis/spec/backend/dsp-state-correctness.md`, and
  `.trellis/spec/backend/error-handling.md`.
