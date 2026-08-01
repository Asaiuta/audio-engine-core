# Raw DSP geometry boundary revalidation

## Audit claim checked

The audit says exported raw processors accept zero or mismatched geometry that
their adapters reject. The claim was checked against the live 2026-07-29 tree,
not accepted from the report at face value.

## Confirmed behavior by type

- `VolumeController::process`: `buffer.len() / channels` panics for zero and
  floors incomplete frames, leaving the suffix untouched.
- `NoiseShaper::new` accepts zero. `process` divides by its caller-supplied
  channel count and floors incomplete frames. A larger-than-setup count is no
  longer an out-of-bounds panic because `bypass_or_recover_invalid` returns the
  original sample for a channel beyond state; the block is instead partially
  shaped and partially bypassed without an error.
- `DynamicLoudness::new`, `PeakLimiter::new`/`with_mode`, and
  `LoudnessNormalizer::new` accept zero channels. Their process methods divide
  by stored channels. Non-multiple sample lengths are silently truncated.
- `SpectrumAnalyzer::new(2, bins)` creates an empty magnitude buffer. Bin range
  construction then calls `usize::clamp` with `min = 1`, `max = 0`. FFT sizes
  zero/one have additional planner/slice hazards. Zero bins create a
  semantically unusable analyzer.
- `AudioBlockMut::new` already rejects zero channels and incomplete frames.
  Adapter `process_fixed_1_to_1` already rejects configured-channel mismatch
  before entering a kernel.

## Refactor decision

Use the existing validated block and process error model. Public raw process
methods validate first and return `Result`; fixed-channel processors receive an
explicit process-time channel count. The existing adapter validator moves to a
shared owner. Raw algorithms move behind crate-private `process_validated`
methods so adapters do not repeat checks after `ProcessBuffers` has already
provided a complete block.

Constructors become fallible where setup geometry can be unusable. Sample-rate
zero is included because the facade already treats it as invalid callback
geometry and several smoothing/filter calculations otherwise receive a
degenerate time domain.

## Refactors rejected

- Do not introduce `DspGeometry` or another public channel/rate DTO. It would
  duplicate `CallbackSpec` and the existing audio-block types without covering
  spectrum-specific geometry.
- Do not retain public unchecked compatibility aliases. They would preserve the
  split safety contract the task exists to remove.
- Do not clamp zero channels to mono/stereo or round sample lengths down. Both
  silently reinterpret caller data.
- Do not make validation assertions or panics. Direct callback callers require
  typed, allocation-free errors.
- Do not fold later numeric parameter validation or loudness-backend reliability
  into this task; those have separate policies and audit rankings.

## Required proof

- Constructor tests for zero channels/rate and invalid spectrum geometry.
- Process tests for zero, incomplete-frame, and channel mismatch across every
  affected raw processor, proving samples and representative state are
  unchanged on rejection.
- Valid-input equivalence against pre-refactor expected output or existing
  bit-exact tests.
- Adapter tests proving the checked outer driver still rejects mismatches and
  the validated kernel stays allocation-free.

## Final implementation and refactor review

The implementation keeps three broader changes because each removes a real
split owner rather than merely shortening a function:

- `traits.rs` owns zero-channel, sample-rate, and configured-channel validation.
  Raw checked shells and adapters now return the same typed errors.
- Exported raw slice APIs validate through `AudioBlockMut` and then call
  crate-private `process_validated` kernels. Adapters enter those kernels only
  after their typed block driver has validated geometry, so callback work does
  not validate twice.
- Geometry-dependent adapter constructors validate before registering realtime
  snapshot readers. Their subsequent setup uses crate-private validated
  constructors, so an invalid request does not allocate or initialize partial
  adapter state.

Focused rejection tests snapshot samples and representative DSP state for
volume, noise shaping, dynamic loudness, peak limiting, and normalization.
They execute the error paths under `assert_no_alloc`. The normalizer test also
continues both the rejected instance and an identically warmed reference to
prove hidden meter/limiter state remains bit-identical. Spectrum tests snapshot
all reusable FFT, scratch, magnitude, result, and bin-range storage.

Broader changes were deliberately rejected:

- No public geometry DTO, unchecked compatibility alias, or second error enum
  was added; all would preserve or duplicate the boundary this task removes.
- Processor channel fields were not mechanically converted to `NonZeroUsize`.
  Fallible constructors establish that invariant, while changing every inner
  loop and backend call would add conversion churn without improving the raw
  boundary.
- The two-line checked-shell pattern was not hidden behind a generic raw-DSP
  trait or macro. Volume is channel-generic, fixed processors have distinct
  state, and spectrum has different geometry; a common abstraction would erase
  useful ownership differences.
- Numeric control ranges, non-finite parameter policy, and `LoudnessMeter`
  backend reliability remain separate audit findings.
- The defensive channel guard inside the crate-private noise-shaper sample
  kernel remains non-panicking. Public block mismatches no longer reach it, but
  removing the guard would turn an internal invariant mistake into a callback
  panic.

## Final verification

- `cargo check --all-targets --all-features`: passed without warnings.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`: passed.
- `cargo test --all-features --no-fail-fast`: 417 library, 20
  benchmark-support, 25 resampler-support, 3 Windows deployment, and 6 doctests
  passed; 1 native-shim evidence test was explicitly ignored because its
  separately built prerequisite was absent.
- `cargo test --no-default-features --features rubato --no-fail-fast`: 450
  library, 20 benchmark-support, 25 resampler-support, 3 Windows deployment,
  and 6 doctests passed; the same 1 prerequisite test was ignored.
- Focused `git diff --check`: passed; only existing LF-to-CRLF working-copy
  warnings were emitted.
- The task remains active with status `in_progress`; no commit, push, or archive
  was performed.
