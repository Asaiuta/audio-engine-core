# Realtime DSP and analysis module audit

## Snapshot and validation

- Final source snapshot for this area: 2026-07-28 15:53:59 +08:00.
- Branch: `main`.
- None of the production files reviewed in this area appeared in the final
  `git status --short --branch` dirty-file list. Concurrent work remained in
  `src/pipeline.rs`, `src/lib.rs`, `src/processor/lockfree_params.rs`,
  `src/processor/mod.rs`, and `src/processor/traits.rs`; those moving files are
  not used as unversioned evidence in this artifact.
- Relevant source mtimes at the final snapshot:

| File | Last write time (+08:00) |
|---|---|
| `src/processor/automix_analysis.rs` | 2026-07-17 19:09:19 |
| `src/processor/adapters.rs` | 2026-07-19 00:16:19 |
| `src/processor/crossfeed.rs` | 2026-07-17 22:04:07 |
| `src/processor/downmix.rs` | 2026-07-10 13:07:42 |
| `src/processor/dsp.rs` | 2026-07-18 18:13:13 |
| `src/processor/dynamic_loudness.rs` | 2026-07-26 15:22:43 |
| `src/processor/eq.rs` | 2026-07-18 18:55:25 |
| `src/processor/fir_eq.rs` | 2026-07-24 01:22:59 |
| `src/processor/convolver.rs` | 2026-07-26 15:22:43 |
| `src/processor/loudness/limiter.rs` | 2026-07-17 09:57:55 |
| `src/processor/loudness/meter.rs` | 2026-07-18 18:51:54 |
| `src/processor/loudness/normalizer.rs` | 2026-07-17 13:28:18 |
| `src/processor/saturation.rs` | 2026-07-26 15:22:43 |
| `src/processor/spectrum.rs` | 2026-07-10 13:07:44 |
| `src/processor/saturation/tests.rs` | 2026-07-26 15:23:38 |
| `src/processor/convolver/tests.rs` | 2026-07-26 15:23:37 |

Focused validation completed while reviewing this area. Every command exited
with status 0:

```text
cargo test --all-features processor::automix_analysis::tests  # 7 passed
cargo test --all-features processor::eq::tests                # 8 passed
cargo test --all-features processor::saturation::tests        # 28 passed
cargo test --all-features processor::dsp::tests               # 16 passed
cargo test --all-features processor::dynamic_loudness::tests  # 21 passed
cargo test --all-features processor::loudness                 # 42 passed
cargo test --all-features processor::spectrum::tests          # 3 passed
cargo test --all-features processor::downmix::tests           # 14 passed
cargo test --all-features processor::convolver::tests         # 19 passed
cargo test --all-features processor::crossfeed::tests         # 10 passed
cargo test --all-features processor::fir_eq::tests            # 10 passed
cargo test --all-features dynamic_loudness                    # 23 passed
```

These green suites are useful quality signals, but they do not exercise the
AutoMix overlap-duration case, reset/fresh equivalence for dynamic loudness,
invalid raw DSP geometry, or non-finite values sent through the standalone DSP
setters described below.

Scope:

- AutoMix analysis and analysis result construction;
- direct EQ, FIR EQ, spectrum, crossfeed, downmix, saturation, volume, and
  noise-shaping primitives;
- dynamic loudness, loudness measurement/normalization, and peak limiting;
- short- and long-IR convolution public processing entry points;
- callback adapters only where needed to verify a direct processor's lifecycle
  or publication boundary.

## Verdict

Most algorithmic complexity in this area is justified by realtime or signal-
quality contracts and is supported by focused tests. The strongest problems
are not large functions or abstraction count. They are inconsistent public
boundaries: two lifecycle/analysis paths currently produce wrong results, and
the safe checked callback layer sits beside exported raw processors that still
accept geometry or numeric values capable of panicking or poisoning output.

## Confirmed findings

### P1 — AutoMix Full mode omits the tail for tracks between one and two analysis windows

**Category**: correctness defect; incorrect interval boundary.

Evidence:

- The head always decodes up to `max_analyze_time_sec`
  (`src/processor/automix_analysis.rs:277-283`).
- Full mode seeks and decodes a tail only when
  `duration > max_analyze_time_sec * 2.0` (`:285-297`). A track for which
  `max_window < duration <= 2 * max_window` therefore has unanalysed audio after
  the head but an empty tail.
- `finalize_analysis` nevertheless passes the empty tail to `detect_silence`
  as a Full-mode tail (`:398-408`).
- With an empty tail, `detect_silence` interprets the head window's last active
  envelope sample as the absolute track fade-out (`:532-543`). That value then
  feeds cut-out and mix-center construction (`:450-459`).

Consequence:

For a 90-second track with a 60-second analysis window, Full mode can report a
fade/cut/mix location near the end of the first 60 seconds even though another
30 seconds of program material exists. The strict `>` also skips the tail at
exactly two windows. Avoiding overlapping windows is reasonable, but treating
the omitted interval as silence is not.

Direction:

Decode a tail whenever the head does not cover the complete track, with an
explicit overlap/deduplication policy. Add cases immediately above one window,
exactly two windows, and above two windows, and assert absolute fade/cut/mix
positions rather than only DTO shape.

### P1 — `DynamicLoudnessProcessor::reset` loses the published compensation until another control update

**Category**: correctness defect; lifecycle/configuration boundary violation.

Evidence:

- Adapter construction applies the cached volume and strength to the direct
  processor (`src/processor/adapters.rs:1765-1767`).
- Subsequent application occurs only when the atomic parameter generation
  changes (`:1780-1789`).
- `DynamicLoudness::reset` resets every filter and smoother, sets the current
  loudness factor to zero, and clears active bands
  (`src/processor/dynamic_loudness.rs:667-678`).
- The adapter reset delegates to that method and resets only its streaming
  lifecycle; it does not reapply the still-current cached volume/strength
  (`src/processor/adapters.rs:1833-1836`). The cached generation is unchanged.

Consequence:

After a stream reset, the next stream can run without the compensation that is
still visibly published on the control side. Because no generation changed,
ordinary processing does not restore it. This is a persistent state divergence,
not a one-block transition artifact.

Direction:

Define direct `reset` as clearing signal history while retaining/rebuilding
current control targets, or have the adapter explicitly reapply its cached
configuration after reset. Prove reset/fresh equivalence using the same
published snapshot and a non-unity volume/strength.

### P1/P2 — exported raw DSP processors do not share the checked layer's geometry contract

**Category**: callback-panic risk for direct users; systemic boundary debt.

The severity is P1 when these documented realtime-capable primitives are used
directly in an audio callback. Canonical adapters usually validate geometry
first, which reduces the facade path to P2 boundary debt but does not make the
public raw surface safe.

Evidence:

- `VolumeController::process` divides by caller-supplied `channels` without
  rejecting zero (`src/processor/dsp.rs:90-97`).
- `NoiseShaper::new` sizes channel state from one count (`:257-278`), while
  `process` divides by a second caller count and then indexes the setup-sized
  channel state (`:547-565` and `:541-544`). Zero channels panic; a larger
  process-time count can index past the allocated state.
- `DynamicLoudness::new` accepts zero channels and creates no channel filter
  state (`src/processor/dynamic_loudness.rs:423-450`); `process` later divides
  by the stored count (`:619-625`).
- `PeakLimiter::new`/`with_mode` accept zero channels and build zero-length
  per-channel storage (`src/processor/loudness/limiter.rs:177-239`), then
  `process` divides by that count (`:273-276`).
- `LoudnessNormalizer::new` stores any channel count (`src/processor/loudness/normalizer.rs:30-53`), and `process` divides by it
  (`:234-242`).
- `SpectrumAnalyzer::new` is infallible for arbitrary FFT/bin geometry
  (`src/processor/spectrum.rs:21-40`). For `fft_size = 2` and `num_bins > 0`,
  the magnitude slice is empty; bin construction reaches
  `usize::clamp(idx_low + 1, 0)` with a minimum greater than its maximum
  (`:61-69`, `:79-96`) and panics.

Consequence:

The crate has two incompatible meanings of a public DSP entry point: checked
block/adapter APIs return typed geometry errors, while adjacent standalone APIs
can panic on values their constructors accept. External users must rediscover
which layer is safe, and a refactor from an adapter to a raw primitive can
silently remove callback safety.

Direction:

Use validated constructors/geometry types or checked processing entry points
for raw processors, then keep a clearly named crate-private unchecked kernel
for already-validated adapters. At minimum, direct APIs must reject zero,
incomplete frames, and setup/process channel mismatches before entering their
inner loops.

### P2 — standalone numeric setters accept non-finite values that the publication boundary rejects

**Category**: inconsistent validation ownership; output/state poisoning.

Evidence:

- `Equalizer::set_band_gain` clamps without an `is_finite` check and immediately
  designs target coefficients (`src/processor/eq.rs:111-124`).
- Saturation drive, threshold, mix, input gain, output gain, and high-pass
  cutoff setters likewise store/clamp and derive coefficients or linear gains
  from the value (`src/processor/saturation.rs:383-440`).
- `VolumeController::set_target` accepts a NaN through `f64::clamp`; the
  smoother then propagates it into every sample (`src/processor/dsp.rs:79-95`).
- `FirEq::set_band` and `set_bands` accept NaN and regenerate an impulse
  response from it (`src/processor/fir_eq.rs:101-118`).
- Both limiter threshold setters convert any input directly, and are themselves
  duplicates (`src/processor/loudness/limiter.rs:354-362`).
- The facade/atomic publication boundary reviewed in
  `01-public-api-and-control-boundaries.md` does reject or normalize invalid
  control values, so behavior depends on which equally public route a caller
  selects.

Consequence:

NaN can become coefficients, smoother state, an FIR, or output samples; an
infinite threshold/gain can silently disable protection or create non-finite
audio. Validation at the facade does not protect legitimate standalone use.
The split also makes tests for one route poor evidence for the other.

Direction:

Put numeric policy in one validated parameter type or shared validator used by
both raw setters and publishers. Infallible setters need a documented and
consistent invalid-value policy; fallible setters should return a typed setup
error rather than silently retain or poison state.

### P2 — a failed `LoudnessMeter` can report that its measurement is reliable

**Category**: invalid state made indistinguishable from a valid measurement.

Evidence:

- `LoudnessMeter::with_layout` converts an `EbuR128::new` failure into `None`
  and ignores a channel-map installation failure
  (`src/processor/loudness/meter.rs:57-75`). Construction remains successful
  with no explicit disabled/error state.
- `has_reliable_measurement` checks only whether processed samples exceed
  `sample_rate * 0.4` (`:188-191`); it does not require an EBU state.
- A zero sample rate is rejected by the current `ebur128` constructor but also
  makes the reliability threshold zero, so a newly disabled meter reports
  reliable immediately with its fallback values.

Consequence:

Callers cannot distinguish a valid sufficiently long measurement from a meter
that never initialized. Silent fallback is especially dangerous for analysis
or normalization decisions because its values look finite and stable.

Direction:

Make construction fallible for invalid geometry/backend initialization, or
model reliability as a state/result that requires a live backend. Do not use
elapsed samples as the sole capability check.

### P2 — AutoMix loudness and true peak can extend beyond the configured analysis window

**Category**: analysis-scope correctness defect.

Evidence:

- `decode_segment` enforces `max_frames` inside the per-frame analysis loop
  (`src/processor/automix_analysis.rs:331-370`).
- Before that check, it submits the complete decoded packet/chunk to
  `LoudnessMeter::process` (`:334-345`).

Consequence:

The envelope, spectrum, tempo, and vocal data respect the selected window, but
integrated loudness and true peak can include the decoder packet suffix beyond
it. The result combines metrics from different time intervals, and the amount
of overrun depends on codec/packetization.

Direction:

Limit the meter input to the same complete-frame prefix accepted by the other
accumulators and add a decoder fixture whose final packet crosses the boundary.

### P2 — AutoMix erases decoder and cancellation error classes into strings

**Category**: weak error boundary; inaccurate contract for callers.

Both public analysis functions return `Result<AutomixAnalysis, String>`
(`src/processor/automix_analysis.rs:247-260`). Decoder open/seek/decode errors
are formatted or stringified (`:263-268`, `:285-296`, `:331-339`), and
cancellation is another free-form string (`:376-380`). Callers therefore
cannot reliably distinguish cancellation, unsupported media, seek failure, and
I/O failure without parsing message text. This is inconsistent with the
crate's typed decoder and processing error boundaries and makes retry/UI policy
brittle.

### P3 — the public AutoMix key DTO reserves contradictory states before a capability exists

**Category**: premature public surface / over-design.

`AutomixKeyStatus` is a public non-exhaustive enum with only `Unsupported`,
while `AutomixAnalysis` publicly exposes four optional key payload fields
(`src/processor/automix_analysis.rs:45-88`). Finalization always emits
`Unsupported` plus four `None` values (`:510-515`). The documentation is honest,
so this is not a false detection claim, but the public struct still permits
manually constructed contradictory combinations and burdens every schema
consumer with fields for a feature that does not exist.

A smaller capability/result model should be introduced when a detector and
evidence-backed states exist. If forward schema reservation is a hard external
compatibility requirement, construction should at least be private/validated
so status and payload cannot disagree.

### P3 — the AutoMix energy profile allocates by full duration despite bounded analysis

**Category**: avoidable memory scaling / responsibility mismatch.

`build_energy_profile` allocates `ceil(duration * 10)` entries for the complete
track (`src/processor/automix_analysis.rs:780-789`), even though only bounded
head/tail envelopes are available and most interior entries remain zero. The
cost grows with declared media duration rather than the amount of evidence
collected. This is offline work, so it is not a realtime violation, but long or
malformed durations can create disproportionate memory use and a profile that
visually implies unobserved silence. A sparse/segmented profile or an explicit
maximum would match the analysis contract more accurately.

### P3 — duplicate aliases and inaccurate setter names enlarge the low-level API without behavior

**Category**: naming debt and redundant public surface.

- `FFTConvolver::process_into` is only an alias for `try_process_into`
  (`src/processor/convolver.rs:125-169`). `process_inplace` is only an alias for
  `try_process_inplace` (`:181-197`, `:291-295`), and the wet-ramp pair repeats
  the same pattern (`:200-235`). Both names return `Result`; `try_` does not
  communicate an additional checked/fallible behavior.
- `PeakLimiter::set_threshold_db` and `set_threshold` have identical bodies and
  both accept dB (`src/processor/loudness/limiter.rs:354-362`). The latter name
  normally implies a linear threshold and is therefore actively misleading.

Choose one checked processing name per operation and one unit-bearing limiter
setter. Compatibility shims, if required, should be explicitly deprecated
rather than presented as separate capabilities.

### P3 — local documentation contains both current drift and obsolete ticket archaeology

**Category**: documentation drift and maintenance noise.

- `Saturation::set_channel_count` says zero preserves the default stereo size
  (`src/processor/saturation.rs:463-468`), but the implementation converts zero
  to one (`:468-479`). The focused test explicitly describes and asserts mono
  fallback (`src/processor/saturation/tests.rs:413-424`).
- Production comments still encode old review/ticket history such as
  `FIX for Defect 36` (`src/processor/dsp.rs:33`, `:51`), `P1-5 fix` and
  `MINOR-03` (`src/processor/saturation.rs:279`, `:313`, `:323`, `:749`), and
  `P1-4 fix` (`:1114`). Similar archaeology appears elsewhere and will be
  counted globally in the documentation/legacy review.

The first item can mislead a caller about state capacity. The second obscures
the enduring invariant behind identifiers that are not locally resolvable.
Comments should state why behavior is required and link durable documentation
only when historical provenance remains useful.

## Lower-confidence follow-up question

`DownmixCoefficients::AtscA85` is named like an exact standards profile, while
its own documentation says the coefficients are only representative and not a
bit-exact normative table (`src/processor/downmix.rs:60-78`). The implementation
may be entirely appropriate for the product, but a name such as
`CinemaStyleClipSafe` or `AtscA85Inspired` would communicate that distinction
more accurately unless external API compatibility or a documented conformance
interpretation requires the current name.

## Important non-findings / justified complexity

### EQ dual banks and per-band transitions are state-correctness machinery

The active and target filter banks are independently processed during a gain
transition so the branch that received the transition samples owns the
continuation history. Per-band counters avoid restarting unrelated bands. This
is more state than coefficient replacement, but it prevents zippering and the
tests cover chunk independence and branch adoption; it should not be collapsed
into one bank merely to reduce fields.

### Saturation's quality-state bank and fixed timeline are deliberate

Preallocated Direct/2x/4x state, scratch/history buffers, and the fixed delayed
timeline support allocation-free quality/enable automation without latency
jumps. The implementation is large, but its complexity maps to explicit
continuity, alias-reduction, and callback constraints. The documentation drift
above does not justify removing that state model.

### True-peak and limiter data structures are appropriate

The true-peak FIR, preallocated lookahead ring, and monotonic maximum queue keep
the callback bounded while enforcing intersample peak behavior. A simpler
per-block maximum would change the limiter contract; the current focused tests
support the chosen structures.

### Two convolution strategies and control-side ownership are justified

Overlap-save provides the low-latency head/short-IR path while partitioned
convolution bounds long-IR callback work. Preplanned FFT scratch and the
control-side publication/retirement model avoid callback allocation and
destruction. The duplicate method names are removable API debt; the underlying
engines and ownership hand-off are not needless abstraction.

### Spectrum caches and downmix matrix precomputation are useful

Spectrum FFT scratch and log-bin range caches remove repeated planning,
allocation, and range work. Downmix precomputes a matrix so the sample loop is a
fixed multiply/accumulate. Both are straightforward setup-versus-hot-path
separations, not premature caching.

### Crossfeed ramps and dynamic-loudness state separation are necessary

Crossfeed coefficient/mix ramps preserve Bauer filter history across control
updates. Dynamic loudness separately tracks control inputs, smoother geometry,
filter coefficients, and signal history so sample-rate changes can retain
controls while discarding old-rate delay elements. The reset bug is an
incorrect transition between these state classes, not evidence that the
classes themselves should be merged.

## Test gaps exposed by this review

- Full AutoMix mode with `window < duration <= 2 * window`, including exact
  `2 * window`, and absolute fade/cut/mix assertions;
- AutoMix decoder packet crossing `max_frames`, proving every metric sees the
  identical prefix;
- `DynamicLoudnessProcessor` reset/fresh equivalence with a published non-unity
  volume and strength and no intervening parameter publication;
- zero-channel and setup/process mismatch behavior for every exported raw DSP
  processor, plus incomplete-frame behavior where applicable;
- `SpectrumAnalyzer` construction/analyze behavior for FFT sizes 0, 1, and 2
  and non-zero bins;
- non-finite tests for direct EQ, FIR EQ, saturation, volume, dynamic loudness,
  and limiter setters, asserting a shared typed/retention policy and finite
  output;
- failed loudness-backend construction cannot report a reliable measurement;
- public AutoMix callers can distinguish cancellation from decoder/open/seek
  failure without parsing strings.
