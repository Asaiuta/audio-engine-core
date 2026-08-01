# Resampler architecture and backend audit

## Snapshot and validation

- Final source snapshot for this area: 2026-07-28 17:06:05 +08:00.
- Branch: `main`.
- None of `src/processor/resampler/**`, `src/processor/output_chain.rs`, or
  `src/config.rs` appeared in the final scoped `git status --short` output.
  Concurrent edits remained elsewhere in the repository and new unrelated
  untracked files appeared while this area was being reviewed.
- Relevant source mtimes at the snapshot:

| File | Last write time (+08:00) |
|---|---|
| `src/config.rs` | 2026-07-17 21:58:39 |
| `src/processor/output_chain.rs` | 2026-07-26 17:01:45 |
| `src/processor/resampler/mod.rs` | 2026-07-27 13:42:06 |
| `src/processor/resampler/soxr_backend.rs` | 2026-07-26 21:44:30 |
| `src/processor/resampler/rubato_backend.rs` | 2026-07-27 13:38:30 |
| `src/processor/resampler/halfband_backend.rs` | 2026-07-24 22:06:47 |
| `src/processor/resampler/polyphase_backend.rs` | 2026-07-25 21:52:57 |
| `src/processor/resampler/spectral_backend.rs` | 2026-07-25 21:37:15 |
| `src/processor/resampler/contiguous_polyphase_backend.rs` | 2026-07-26 01:41:00 |

Focused validation completed against this snapshot. Every command exited with
status 0:

```text
cargo test --all-features processor::resampler
  13 library tests passed

cargo test --no-default-features --features rubato processor::resampler
  66 library tests passed in 104.97 seconds

cargo test --no-default-features --features rubato processor::output_chain::tests
  23 library tests passed
```

The default-feature run selects SoXR even though `--all-features` also enables
Rubato. The second run is therefore required evidence for the pure-Rust
backend. The output-chain suite covers the currently constructed
`Linear + UltraHigh` rate boundary, but it has no nonlinear high-ratio finish
case and no test for the public capacity helpers' stated meanings.

Scope:

- public one-shot and streaming resampler APIs, geometry validation, progress,
  reset, latency, and finite-tail reporting;
- compile-time SoXR/Rubato selection and quality mapping;
- Rubato fixed-chunk adaptation, bounded FIFOs, delay compensation, exact
  duration, and drain termination;
- half-band, FFT/sinc, spectral nonlinear, contiguous polyphase, and test-oracle
  roles;
- the output-chain boundary that consumes public resampler capacity estimates.

## Verdict

The resampler is the most intrinsically complex part reviewed so far, but most
of that complexity is justified by observable DSP and realtime contracts. The
separate half-band, FFT/sinc, spectral, and contiguous-polyphase engines are
not arbitrary layers: they implement different quality/ratio cost envelopes,
and focused tests verify bit equivalence, duration, reset, finite tail, SIMD,
and allocation behavior.

The maintainability problems are concentrated in the public facade around
those engines. The one-shot API silently accepts malformed geometry that the
streaming API rejects, the default backend exposes two quality names for one
actual recipe, and three public sizing helpers mix estimates, internal-step
claims, and a magic margin without a backend/state/tail model. A private output
chain currently remains correct only because it silently fixes the resampler
to linear phase. These are boundary and naming debts, not evidence that the DSP
engines themselves should be collapsed.

## Confirmed findings

### P2 - the one-shot API bypasses validation on equal rates and truncates incomplete frames on unequal rates

**Category**: public correctness defect; inconsistent input boundary.

Evidence:

- `Resampler::new` is infallible and stores arbitrary channel/rate geometry
  (`src/processor/resampler/mod.rs:183-190`).
- `resample_parallel` returns `input.to_vec()` as soon as the two rates are
  equal, before checking zero rates or zero channels (`:215-238`). Thus
  `Resampler::new(0, 0, 0).resample_parallel(...)` reports success.
- For unequal rates it computes `frames = input.len() / channels` and only
  deinterleaves `frames * channels` samples (`:240-243`, `:87-98`). A trailing
  partial frame is silently discarded rather than rejected.
- The streaming path validates nonzero geometry at construction
  (`:530-550`) and receives audio through `AudioBlockRef` / `AudioBlockMut`,
  whose checked boundary rejects incomplete frames. The two public resampler
  APIs therefore disagree about the same malformed input.

Consequence:

A caller mistake can be preserved unchanged at equal rates but silently lose
one or more samples as soon as rate conversion is enabled. Zero-rate/zero-
channel configuration can similarly look valid only in the bypass case. This
makes upstream validation dependent on runtime rate equality and makes faults
hard to reproduce when a device rate changes.

Direction:

Use one validated geometry type or make the one-shot constructor fallible, and
require complete interleaved frames before the equal-rate fast path. The
one-shot method should share the same block validation contract as
`StreamingResampler`.

### P2 - `Standard` and `High` are distinct public presets but the default SoXR backend maps both to one recipe

**Category**: inaccurate naming and configuration contract; misleading
benchmark dimension.

Evidence:

- `ResampleQuality` documents four presets trading CPU cost for stopband and
  transition-band quality; `Standard` is "balanced" and `High` is the default
  high-quality tier (`src/config.rs:5-17`).
- `quality_to_recipe` maps both `Standard` and `High` to
  `QualityRecipe::high()` (`src/processor/resampler/soxr_backend.rs:22-31`).
  The nearby historical comment says the defect fix "actually use[s]
  different quality levels", which is false for these two variants.
- The locked `soxr` dependency is 0.6.0 (`Cargo.lock:1197-1200`); its API has a
  separate `QualityRecipe::Medium`, so this is not forced by an absent backend
  tier.
- The matrix benchmark emits separately named `standard` and `high` cases
  (`benches/audio_resampler_matrix_perf.rs:116-126`, `:263-266`). Under the
  default backend those labels do not identify different filter recipes.

Consequence:

Users can change a public setting and observe no quality or CPU change, while
reports can present the two rows as a quality ladder. That weakens both API
trust and benchmark interpretation. Rubato does implement distinct Standard
and High nonlinear/sinc parameters, so behavior also changes by compile-time
backend in a way the enum does not express.

Direction:

Either map Standard to a genuinely distinct reviewed SoXR recipe, or document
and model it as an alias. Benchmark metadata should identify the resolved
backend recipe, not only the requested enum label.

### P2 - the output-chain finish bound depends on an undocumented linear-phase construction invariant

**Category**: unclear cross-layer boundary; future correctness trap.

Evidence:

- `StreamingResampler` calculates a real nonlinear latency and finite tail from
  backend `finish_extension_frames` (`src/processor/resampler/mod.rs:586-625`).
  Rubato drain authorizes duration plus that complete extension
  (`src/processor/resampler/rubato_backend.rs:1748-1755`).
- The public `max_output_len_for_input` does not include latency or tail. It is
  only rounded rate conversion plus a fixed 64-frame margin
  (`src/processor/resampler/mod.rs:480-490`).
- `RateBoundary::finish_frame_limit` nevertheless treats this helper plus one
  render block as a hard finish bound (`src/processor/output_chain.rs:513-524`),
  and `RateBoundary::finish` returns a backend error when drain reaches it
  (`:526-553`). The type itself accepts any `StreamingResampler` reference.
- Current construction fixes this private boundary to
  `PhaseResponse::Linear + ResampleQuality::UltraHigh`
  (`src/processor/output_chain.rs:1477-1487`). Linear Rubato/SoXR compensates
  its native delay and reports no finite extension, so current production
  construction avoids the mismatch. That requirement is not encoded in
  `RateBoundary` or the sizing method.

Why the hidden invariant matters:

For a supported 8 kHz to 192 kHz High nonlinear stream, the reduced ratio is
24:1. The shared High design uses 256 taps per phase
(`src/processor/resampler/polyphase_backend.rs:204-210`), so the kernel has
6,144 taps and declares 6,143 finish-extension frames (`:331-333`). With a
1,024-frame input and 1,024-frame render block, the current helper-derived
bound is `24,576 + 64 + 1,024 = 25,664` frames, while duration plus the
declared extension is 30,719 frames. Merely making output-chain phase
configurable would therefore turn this latent coupling into a deterministic
"resampler finish exceeded its declared bound" error.

Direction:

Name the method as a process-call estimate if that is its only contract.
Calculate finish bounds from `latency()` and `tail()`, or encode a
linear-duration-aligned resampler type in `RateBoundary` so unsupported timing
semantics cannot be injected accidentally.

### P2/P3 - public capacity helpers have three different, weakly specified meanings and unchecked arithmetic

**Category**: inaccurate API surface; portability and panic risk.

Evidence:

- `max_output_len_for_input` estimates caller output from only the current
  input length, despite backend state being able to contain a nearly complete
  fixed input chunk. Its fixed `+ channels * 64` margin has no named backend
  contract (`src/processor/resampler/mod.rs:480-490`).
- `max_output_samples_per_chunk` claims to return the maximum from "one
  internal backend step", but it reuses a generic 16,384-input-frame SoXR
  scratch layout under both feature matrices (`:404-455`, `:492-496`). Rubato's
  actual fixed step is 1,024 frames (`src/processor/resampler/rubato_backend.rs:59-60`),
  so the method is not describing its named quantity there.
- `input_frames_for_output_frames` adds another unexplained 64-frame margin
  and has no production, example, test, or benchmark caller
  (`src/processor/resampler/mod.rs:498-505`).
- The float-to-`usize` conversions are followed by unchecked `+ 64`, channel
  multiplication, and addition (`:423-424`, `:483-505`). On a 32-bit target an
  extreme public rate ratio can saturate the cast and overflow immediately;
  on any target a sufficiently large public sample count can do the same.
- Rubato setup similarly multiplies engine output, chunk size, and arbitrary
  public channel count without checked arithmetic before allocation
  (`src/processor/resampler/rubato_backend.rs:1274-1295`).

Consequence:

Callers cannot tell whether a value is a process-call recommendation, a native
step maximum, a whole-stream reservation, or a finish bound. Overestimation
wastes memory; underestimation is survivable only when callers correctly honor
backpressure; using an estimate as a hard bound creates the coupling above.
Extreme geometry turns a typed initialization API into overflow/panic or an
uncontrolled allocation attempt.

Direction:

Replace magic-margin `usize` helpers with checked, unit-explicit contracts such
as `process_output_capacity(input_frames, pending_state)` and a separate
timing-derived finish bound. Reuse exact integer rate arithmetic and checked
sample/channel multiplication throughout construction.

### P3 - one-shot channel-length divergence is silently padded or truncated instead of treated as an invariant failure

**Category**: latent corruption masking; unnecessary fallback complexity.

`interleave_channel_outputs_to_vec` chooses channel zero's length as canonical.
If another backend channel is shorter it inserts zeros; if another is longer it
drops the surplus (`src/processor/resampler/mod.rs:100-139`). The streaming
multi-mono path instead explicitly rejects channel progress or drain divergence
(`:798-835`, `:1018-1044`). Current identical per-channel backends should be
deterministic, so the one-shot fallback is not a useful recovery policy: it
hides a backend invariant violation and returns phase-misaligned audio as
success. Reuse the explicit divergence check and reserve zero-filling for a
separately named caller policy, if one is ever required.

### P3 - equal-rate bypass still constructs and owns an unused native/filter backend

**Category**: avoidable setup complexity and resource ownership.

`with_quality` constructs every SoXR stream or the complete Rubato engine
before it derives zero latency/tail for equal rates
(`src/processor/resampler/mod.rs:530-625`). Processing and finish then bypass
the backend, and `is_enabled` returns false (`:1133-1198`). For non-stereo SoXR
it also allocates adapter scratch (`:627-675`). A no-op rate boundary therefore
pays backend setup and can fail for resources it will never use. An explicit
`Bypass` backend/state would simplify lifecycle reasoning and make equal-rate
working memory match the actual processing path.

### P3 - backend names and historical comments no longer match ownership

**Category**: naming and maintenance debt.

- `MonoBackend` can contain native stereo SoXR and a complete interleaved
  multichannel Rubato engine (`src/processor/resampler/soxr_backend.rs:40-47`,
  `src/processor/resampler/rubato_backend.rs:921-969`). The name describes the
  retired shape, not the current abstraction.
- `legacy_channel_inputs` / `legacy_channel_outputs` are the active non-stereo
  SoXR path, not compatibility code (`src/processor/resampler/mod.rs:634-656`).
- Public rustdoc still contains `FIX for Defect 30/33` narration
  (`:378`, `:524-527`), and one such claim is contradicted by the current SoXR
  mapping above.

These labels force maintainers to reconstruct history before they can identify
the present owner. Rename around `SelectedBackend` / `PlanarSoxrScratch` and
state current invariants in rustdoc; retain issue history in commits or task
records.

## Important non-findings / justified complexity

### Compile-time backend precedence is explicit and centralized

The cfg boundary selects exactly one backend and exports the resolved backend
name from the same decision (`src/processor/resampler/mod.rs:16-45`). SoXR
winning when both features are enabled is documented and verified by running
both feature matrices. This is clearer than duplicating precedence in reports
or callers.

### The four Rubato execution routes have distinct measured responsibilities

The exact-2x High half-band route, common-ratio FFT route, pathological-ratio
sinc fallback, and nonlinear spectral/contiguous split have different quality,
block-size, and complexity envelopes (`src/processor/resampler/rubato_backend.rs:59-139`,
`:761-918`). Bounds reject pathological nonlinear geometry instead of silently
substituting a linear-phase engine. Collapsing them into one generic resampler
would erase phase and performance contracts.

### Fixed rings and specialized Rubato adapters are realtime mechanisms, not decorative wrappers

`SampleRing`, split input, direct/split/terminal output views, partial-zero FFT
drain, and delay discard avoid allocation and redundant interleaved/planar
copies under backpressure (`src/processor/resampler/rubato_backend.rs:148-700`,
`:971-1252`). The 66-test Rubato suite verifies ring order, bit equality with
fallback paths, reset freshness, exact duration, terminal idempotence, and
no-allocation execution. Their unsafe adapter implementations are locally
bounded by checked dimensions and focused equivalence tests.

### Enum delegation is preferable to callback-path trait-object indirection here

`RubatoEngine` and `NonlinearEngine` repeat a small set of match arms for
process/reset/timing dispatch. Given the fixed compile-time variants and the
realtime hot path, this explicit static dispatch is a reasonable tradeoff. The
repetition should not be replaced with dynamic polymorphism merely to save
lines.

### The test-only polyphase implementation is a valuable independent oracle

`PolyphaseResampler` is compiled only in tests while its filter-design helpers
are shared by both optimized nonlinear engines
(`src/processor/resampler/polyphase_backend.rs:21-195`, `:197-333`). Spectral
and contiguous implementations compare against it across rates, qualities,
phases, reset, and timing. Keeping a slower structurally different oracle is
stronger evidence than testing two optimized paths only against each other.

### Latency and finite tail are intentionally separate

Linear Rubato removes its native leading delay and returns an exact-duration
stream. Nonlinear phase preserves the causal response and reports peak latency
plus the remaining finite tail (`src/processor/resampler/mod.rs:586-625`,
`src/processor/resampler/rubato_backend.rs:840-860`, `:1748-1755`). The shared
tests confirm minimum/maximum energy order and complete output length. This is
required timing semantics, not over-modeling.

### SIMD selection belongs at setup

Half-band AVX2+FMA and contiguous-polyphase AVX2 kernels are selected once at
construction, with scalar equivalence tests covering vector and remainder
lengths. No runtime feature detection enters the callback. Separate scalar and
vector kernels are justified by the no-allocation/no-unbounded-work contract.

## Test gaps exposed by this review

- one-shot equal-rate zero geometry, incomplete interleaved frames, and the
  same malformed input under equal versus unequal rates;
- one-shot channel-output divergence must fail rather than pad/truncate;
- an assertion that every public quality preset resolves to the intended SoXR
  recipe, or an explicit alias contract in tests and benchmark metadata;
- checked sizing behavior for extreme rates, sample counts, channel counts,
  and 32-bit targets;
- property tests comparing each public capacity helper with the exact thing it
  claims to bound, across queued-prefix, spill, delay, and finite-tail states;
- a test that makes the `RateBoundary` linear-phase dependency explicit, plus a
  nonlinear high-ratio fixture before phase can become configurable there;
- equal-rate construction should prove whether backend/native allocation is a
  deliberate contract or avoid it entirely;
- error-injection tests for multi-mono channel progress/drain divergence in the
  one-shot path.
