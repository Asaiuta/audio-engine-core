# DSP State Correctness

> Executable contracts for separating control state, coefficient geometry, and
> signal history in stateful DSP. Read this with `realtime-safety.md` when
> changing EQ transitions, loudness configuration, biquad design, or
> sample-rate updates.

## 1. Scope / Trigger

This spec applies when code:

* crossfades or otherwise runs two stateful signal branches independently;
* publishes a stored config into callback-facing atomic runtime state;
* implements or changes RBJ/W3C biquad coefficients;
* changes sample rate on an existing stateful processor or adapter.

The core distinction is that user controls, coefficient geometry, and delay
history are different kinds of state. An update must explicitly say which of
them it preserves, replaces, or resets.

## 2. Signatures

Relevant signatures include:

```rust
BiquadSection::copy_coefficients_from(&mut self, other: &BiquadSection)

LoudnessNormalizer::set_config(&mut self, config: LoudnessConfig)
LoudnessNormalizer::set_enabled(&mut self, enabled: bool)
LoudnessNormalizer::set_mode(&mut self, mode: NormalizationMode)
AtomicLoudnessState::set_normalization_mode(&self, mode: NormalizationMode)

DynamicLoudness::set_sample_rate(&mut self, sample_rate: f64)
StreamingProcessor::set_sample_rate(&mut self, sample_rate_hz: u32)
    -> Result<(), ProcessError>

// Fixed callback adapters (EQ, Crossfeed, Volume, NoiseShaper,
// DynamicLoudness, and Saturation) reset their shared finish lifecycle when
// entering a new sample-rate domain.
```

`copy_coefficients_from` deliberately retains the destination `z1/z2`. It is
not a branch-adoption API. Adopting an independently processed branch requires
copying or moving its complete filter value.

## 3. Contracts

### Stateful branch ownership

If current and target filters both consume every transition sample, the target
branch owns the post-transition signal state. Completion copies coefficients
and delay elements from target to active. Combining target coefficients with
the current branch's history creates a discontinuity even when the final
crossfade weight is visually close to one.

Coefficient-only copying is valid only when coefficients are being changed on
one continuing signal branch and retaining that branch's history is the stated
policy.

### Config publication

`LoudnessNormalizer` stores `LoudnessConfig` and publishes callback state. Its
constructor and `set_config` publish both `enabled` and all five
`NormalizationMode` values; explicit `set_enabled` and `set_mode` update both
the stored config and the atomic runtime state. Mode encoding is centralized in
`AtomicLoudnessState::set_normalization_mode` rather than duplicated at call
sites.

### RBJ shelf equations

For shelf slope `S = 1`:

```text
A = 10^(gain_db / 40)
alpha = sin(w0) / sqrt(2)
two_sqrt_a_alpha = 2 * sqrt(A) * alpha
```

Low/high-shelf coefficient equations use `two_sqrt_a_alpha` directly. Do not
multiply it by `sin(w0)` again. Tests compare both normalized coefficients and
the analytical transfer function against a separately written RBJ/W3C oracle;
a helper copied from production code is not sufficient evidence by itself.

### Sample-rate updates

A dynamic-loudness rate change updates the existing processor in place. It
preserves enabled state, strength, reference and transition controls, current
loudness factor, and smoother `current` / `target` / progress. It recomputes
smoother time constants for the new rate.

Old-rate biquad delay elements are not mapped into the new rate domain. Reset
them, rebuild geometry, and immediately restore coefficients from each
preserved current smoother gain. An adapter must delegate to this in-place
update instead of assigning `DynamicLoudness::new(...)`.

Every fixed 1:1 adapter applies the same boundary rule: validate the new rate,
rebuild/reset rate-dependent signal state, clear any partial finish counter,
and re-arm ordinary `process` before returning. A rate update must not leave
the adapter terminal merely because the previous stream had finished.

Crossfeed additionally treats mono and non-stereo layouts as a deliberate
transparent state with `TailSpec::None`; its finish path returns
`Finished(0)` without manufacturing IIR tail samples.

All process-path transition completion remains allocation-, lock-, log-, I/O-,
and panic-free.

## 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Independent target branch reaches transition end | Active coefficients and `z1/z2` equal target exactly |
| Coefficients change on one continuing branch | Destination history may be retained only by explicit policy |
| Constructor or `set_config` receives `enabled=false` | Atomic state is disabled and processing transparently bypasses |
| Any of the five normalization modes is configured | Atomic round-trip returns the identical enum value |
| Shelf coefficient contains another `sin(w0)` factor | Reject in review; RBJ coefficient/response tests must fail |
| Adapter sample rate is zero | `ProcessError::InvalidSampleRate` before mutation |
| Valid dynamic-loudness sample-rate change | Controls/smoothers preserved; filter history zeroed; coefficients rebuilt |
| Fixed adapter rate change after terminal finish | Lifecycle is reset and the next block is accepted |
| Crossfeed block has channels other than two | Exact bypass and no finish tail |
| Transition completion allocates on the callback | Test failure; implementation is not realtime-safe |

## 5. Good / Base / Bad Cases

* Good: a target EQ biquad processes all 1,024 transition frames, then its
  complete value becomes the active filter before the next frame.
* Base: a coefficient update on a single active branch intentionally retains
  its existing delay state and uses `copy_coefficients_from`.
* Good: a 48-to-96 kHz dynamic-loudness update preserves a partially completed
  gain ramp, changes its per-sample smoothing coefficient, resets old-rate
  delay elements, and reinstalls the current gain at 96 kHz.
* Bad: `self.processor = DynamicLoudness::new(...)` inside an adapter rate
  update, because it silently restores user controls and smoothers to defaults.
* Bad: constructor config is stored in a field while callback atomics retain
  unrelated defaults.
* Bad: a production coefficient helper and its test oracle share the same
  erroneous algebra and are treated as independent verification.

## 6. Tests Required

* Crossfade tests use tone and impulse inputs, assert complete active/target
  filter equality at the boundary, and compare continuation output within
  `1e-9` maximum linear error.
* Whole-buffer and irregular frame chunks produce equivalent mono/stereo
  transition output.
* Transition completion has an `assert_no_alloc` regression test.
* Loudness config tests cover `enabled=false`, transparent bypass, constructor
  publication, `set_config`, explicit setters, and all five modes.
* Low/high shelves cover representative rates, positive/negative gains, and
  frequencies with coefficient error `<= 1e-12` and analytical response error
  `<= 1e-9 dB` against the RBJ/W3C oracle.
* Direct processor and adapter tests both prove sample-rate control/smoother
  preservation, coefficient rebuild, and deliberate biquad-history reset.

## 7. Wrong vs Correct

### Wrong

```rust
// target was independently processed, but its signal history is discarded.
active.copy_coefficients_from(&target);

// all user and smoother state silently returns to constructor defaults.
self.dynamic_loudness = DynamicLoudness::new(self.channels, new_rate);

// alpha already contains sin(w0).
let shelf_term = 2.0 * a.sqrt() * alpha * sin_w0;
```

### Correct

```rust
// The branch that accumulated the transition input owns the continuation.
active.clone_from(&target);

// Preserve controls/smoothers and rebuild only rate-dependent state.
self.dynamic_loudness.set_sample_rate(new_rate);

let shelf_term = 2.0 * a.sqrt() * alpha;
```
