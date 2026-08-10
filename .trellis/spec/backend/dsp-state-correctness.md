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
* changes sample rate on an existing stateful processor or adapter;
* resets a stateful adapter whose control snapshot survives across streams;
* adds or changes a public control setter that addresses one element of a
  fixed-size DSP bank (an equalizer band, a shelf, a channel slot).

The core distinction is that user controls, coefficient geometry, and delay
history are different kinds of state. An update must explicitly say which of
them it preserves, replaces, or resets.

## 2. Signatures

Relevant signatures include:

```rust
LoudnessNormalizer::set_config(&mut self, config: LoudnessConfig)
    -> Result<(), ProcessError>
LoudnessNormalizer::set_enabled(&mut self, enabled: bool)
LoudnessNormalizer::set_mode(&mut self, mode: NormalizationMode)
LoudnessNormalizer::set_target_lufs(&mut self, target_lufs: f64)
    -> Result<(), ProcessError>
LoudnessNormalizer::set_album_gain(&self, gain_db: f64)
    -> Result<(), ProcessError>
LoudnessNormalizer::set_preamp_gain(&self, gain_db: f64)
    -> Result<(), ProcessError>
AtomicLoudnessState::set_normalization_mode(&self, mode: NormalizationMode)

DynamicLoudness::set_sample_rate(&mut self, sample_rate: f64)
DynamicLoudnessProcessor::apply_cached_params(&mut self)
StreamingProcessor::set_sample_rate(&mut self, sample_rate_hz: u32)
    -> Result<(), ProcessError>
StreamingProcessor::reset(&mut self) -> Result<(), ProcessError>

// Checked public shells over crate-private realtime kernels.
Equalizer::set_band_gain(&mut self, band_idx: usize, gain_db: f64,
    sample_rate: f64) -> Result<(), ProcessError>
Equalizer::set_all_bands(&mut self, gains: &[f64; EQ_BANDS],
    sample_rate: f64) -> Result<(), ProcessError>
Equalizer::set_band_gain_validated(&mut self, ..)      // pub(crate), infallible
Equalizer::set_all_bands_validated(&mut self, ..)      // pub(crate), infallible

PlaybackParameters::set_eq_band_gain_db(&self, band: usize, gain_db: f64)
    -> Result<(), ProcessError>
AtomicEqParams::set_band_gain(&self, band: usize, gain_db: f64)  // infallible
lockfree_params::validate_eq_band_index(processor: &'static str, band: usize)
    -> Result<(), ProcessError>                        // pub(crate)

// Fixed callback adapters (EQ, Crossfeed, Volume, NoiseShaper,
// DynamicLoudness, and Saturation) reset their shared finish lifecycle when
// entering a new sample-rate domain.
```

A coefficients-only copy — one that deliberately retains the destination's
`z1/z2` — is not a branch-adoption API. Adopting an independently processed
branch requires copying or moving its complete filter value. The crate offers
no coefficients-only copy today: `Equalizer` adopts a fully processed target
branch with `clone_from`.

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
constructor and `set_config` validate every config field before mutation, then
publish both `enabled` and all five `NormalizationMode` values; a rejected
configuration leaves the stored config, limiter threshold, meter, gain state,
and atomic snapshot unchanged. Explicit `set_enabled` and `set_mode` update
both the stored config and the atomic runtime state. Target LUFS, album gain,
and preamp gain setters reject non-finite values with `ProcessError` before
publishing. Mode encoding is centralized in
`AtomicLoudnessState::set_normalization_mode` rather than duplicated at call
sites. `AtomicLoudnessState` keeps its writable atomics private, exposes stable
read-only accessors, and has no public raw numeric mode setter.

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

### One control operation is one guarded publication

A control-thread setter that represents a single semantic operation must become
exactly one callback-visible snapshot. Composing it from two publisher calls
lets the callback adopt the intermediate state — for example a saturation block
running the new input makeup gain against the old output makeup gain — which
contradicts the complete-snapshot contract the facade advertises.

Patching a stored snapshot goes through `SharedParams::update` (or
`update_if` when the decision to publish depends on the current snapshot).
Both hold the writer lock across read, patch, and publish. Reading a snapshot
with `read()`/`load()` outside the lock, mutating the copy, and then calling
`publish` is a lost update: any concurrent setter that lands in between is
silently overwritten.

When several fields form one operation, add a purpose-built coherent publisher
next to the single-field setters (the `write`-style publishers on the crossfeed,
noise-shaper, dynamic-loudness, and EQ parameter types are the precedent). Do
not expose a generic snapshot-patch closure to callers: that would bypass the
family's sanitization and let a non-finite value be stored directly.

The single-field setters stay available for changing one field, and their
rustdoc names the paired publisher so a caller does not reconstruct the torn
sequence by hand.

### Band-index addressing and control rejection

A bank index is an address, not a value. A gain can be clamped into its
published range; an index cannot, because clamping it edits a different band
than the caller asked for. Any public setter that takes a bank index and returns
`Result` must reject an index at or above the bank size with
`ProcessError::InvalidParameter`, and must not mutate coefficients, target
gains, or transition counters on that path.

`validate_eq_band_index` is the single owner of that policy, so the playback
facade and the raw `Equalizer` report the identical `parameter` identity for the
same mistake. Do not re-encode the bound at a call site.

Non-finite control values follow a two-layer policy that must not be collapsed:

* `Atomic*Params` setters stay **infallible**. A `NaN`/infinite value — or an
  out-of-range band index — publishes nothing and the previously published
  snapshot stays in effect. This protects advanced callers that hold the raw
  parameter types, and keeps one uniform family contract.
* Public control surfaces that return `Result` — the playback facade and the
  raw DSP setters — **report** the rejection instead of discarding it silently.
  A `Result` must not acknowledge a write that never reached DSP state.

A raw DSP setter that returns `Result` must reject non-finite input rather than
clamp it. `f64::clamp` returns `NaN` unchanged, so a clamped `NaN` reaches the
coefficient design function and poisons that band's history for the rest of the
stream. Published range constants (`EQ_BAND_GAIN_DB_MIN`/`_MAX` and siblings)
are the clamp source of truth; a DSP core must not re-encode them as literals.

A checked public shell validates and then delegates to a crate-private
`*_validated` kernel that owns the clamp and the state mutation. Callback-side
parameter sync calls the kernel directly, because the atomic snapshot it reads
is already sanitized — so the audio thread pays no validation cost and handles
no `Result`. A whole-bank write validates every entry before applying any, so a
rejection cannot leave a partially updated bank.

### Reset and adopted control snapshots

A streaming reset starts a logically new signal stream, not a new control
session. `DynamicLoudnessProcessor` clears direct filter/smoother history, then
reapplies its already-adopted cached volume and strength before re-arming the
fixed lifecycle. Constructor setup, changed-generation sync, and reset all use
`apply_cached_params`; no second snapshot-to-DSP mapping may drift from it.

Reset does not reload atomics or change `cached_generation`. The cached value is
the coherent snapshot accepted at the previous block boundary, and no new
publication occurred. A reset adapter must therefore match a fresh adapter
constructed from that same snapshot even if the used instance had advanced
filters and smoothers before reset.

Do not add duplicate volume storage to the direct `DynamicLoudness` solely for
the adapter lifecycle. The adapter owns publication identity; the direct DSP
continues to own signal/smoother state and receives controls through its
existing setters.

All process-path transition completion remains allocation-, lock-, log-, I/O-,
and panic-free.

## 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Independent target branch reaches transition end | Active coefficients and `z1/z2` equal target exactly |
| Coefficients change on one continuing branch | Destination history may be retained only by explicit policy |
| Constructor or `set_config` receives `enabled=false` | Atomic state is disabled and processing transparently bypasses |
| Any of the five normalization modes is configured | Atomic round-trip returns the identical enum value |
| `LoudnessConfig`, target LUFS, album gain, or preamp gain is non-finite | `ProcessError::InvalidParameter` before any owner or atomic state mutation |
| `LoudnessConfig` contains invalid smoothing time/rate or true-peak limit | `ProcessError` before construction/reconfiguration; prior config and limiter remain intact |
| Atomic loudness gain receives NaN or infinity | No publication; prior atomic bits remain unchanged |
| Atomic loudness smoothing receives a negative/non-finite time or zero rate | No publication; prior smoothing bits remain unchanged; zero duration is valid |
| Caller attempts raw numeric loudness mode publication | No public setter exists; enum-only `set_normalization_mode` is the boundary |
| Shelf coefficient contains another `sin(w0)` factor | Reject in review; RBJ coefficient/response tests must fail |
| Adapter sample rate is zero | `ProcessError::InvalidSampleRate` before mutation |
| Valid dynamic-loudness sample-rate change | Controls/smoothers preserved; filter history zeroed; coefficients rebuilt |
| Dynamic-loudness reset with unchanged published generation | Clear signal/smoother history, reapply cached volume/strength, retain generation |
| Used-then-reset and fresh adapters use the same snapshot | Next-stream samples and band-gain state are bit-identical |
| Fixed adapter rate change after terminal finish | Lifecycle is reset and the next block is accepted |
| Crossfeed block has channels other than two | Exact bypass and no finish tail |
| Transition completion allocates on the callback | Test failure; implementation is not realtime-safe |
| Fallible bank setter receives an index >= bank size | `ProcessError::InvalidParameter` with parameter `eq band index`; no state mutation |
| Fallible raw DSP setter receives a non-finite value | `ProcessError::InvalidParameter` with parameter `eq band gain`; no coefficient rebuild |
| Whole-bank write contains one non-finite entry | Whole write rejected; the finite entries ahead of it are not applied |
| Infallible `Atomic*Params` setter receives an invalid index or value | No publication; previous snapshot stays in effect; no error |
| Facade returns `Ok` for a write the parameter layer discarded | Review rejection; the acknowledgement is false |
| DSP core clamps with a literal instead of the published constant | Review rejection; the range has two owners |
| One facade setter calls two publishers | Review rejection; the generation must advance by exactly one |
| Setter reads a snapshot outside the writer lock and republishes it | Review rejection; use `update`/`update_if` |
| Multi-field operation with any field non-finite | Whole write rejected; previous snapshot survives; generation unchanged |
| Concurrent single-field and multi-field writes | Neither write is lost; published field values never regress |

## 5. Good / Base / Bad Cases

* Good: a target EQ biquad processes all 1,024 transition frames, then its
  complete value becomes the active filter before the next frame.
* Base: a coefficient update on a single active branch intentionally retains
  its existing delay state, copying coefficients only. No such helper exists in
  the crate today; adding one requires stating that retention explicitly.
* Good: a 48-to-96 kHz dynamic-loudness update preserves a partially completed
  gain ramp, changes its per-sample smoothing coefficient, resets old-rate
  delay elements, and reinstalls the current gain at 96 kHz.
* Good: a dynamic-loudness adapter processes one stream, resets without a
  control write, and produces the same next stream as a fresh adapter subscribed
  to its cached non-unity volume/strength snapshot.
* Bad: `self.processor = DynamicLoudness::new(...)` inside an adapter rate
  update, because it silently restores user controls and smoothers to defaults.
* Bad: reset the direct dynamic-loudness state while retaining the adapter's
  generation, then rely on generation-gated sync to restore the old snapshot.
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
* Rejected config and target/album/preamp writes assert the exact typed error
  and bit-identical stored config, limiter threshold, gain state, and atomic
  snapshot against a separately configured reference instance.
* Atomic loudness tests cover NaN/infinity gains, invalid smoothing, zero
  smoothing, and attempted raw mode writes; every rejected write preserves the
  previous bit pattern.
* Low/high shelves cover representative rates, positive/negative gains, and
  frequencies with coefficient error `<= 1e-12` and analytical response error
  `<= 1e-9 dB` against the RBJ/W3C oracle.
* Direct processor and adapter tests both prove sample-rate control/smoother
  preservation, coefficient rebuild, and deliberate biquad-history reset.
* Dynamic-loudness reset/fresh equivalence publishes non-unity volume and
  strength once, advances prior-stream history, resets without another write,
  retains `cached_generation`, and compares next-stream samples and band gains
  bit-for-bit. The reset call is enclosed by `assert_no_alloc`.
* Every fallible bank setter is tested with `bank_size`, `bank_size + 1`, and
  `usize::MAX`, asserting the exact typed error and bit-identical coefficients,
  target gains, and transition counters against a separately configured
  reference instance.
* Non-finite gains (`NaN`, `+inf`, `-inf`) are tested at each fallible setter,
  followed by a process call that asserts every output sample is finite.
* A whole-bank write with one non-finite entry asserts that no earlier finite
  entry was applied.
* Checked-shell and `*_validated`-kernel writes of the same valid bank produce
  bit-identical state, so the callback path cannot drift from the public one.
* The clamp is asserted against `EQ_BAND_GAIN_DB_MIN`/`_MAX`, not against
  numeric literals.
* Every value-bearing facade setter is covered by a generation-delta assertion
  proving it publishes exactly one snapshot, so a future paired setter cannot
  silently tear.
* A paired write is also observed through a realtime subscriber, asserting one
  generation step and both fields already updated in that single snapshot.
* Guarded read-modify-publish setters have a concurrency regression: a second
  thread writes a strictly monotonic field while the setter runs, and an
  observer asserts that field never regresses.

## 7. Wrong vs Correct

### Wrong

```rust
// target was independently processed, but its signal history is discarded.
active.copy_coefficients_only(&target);

// all user and smoother state silently returns to constructor defaults.
self.dynamic_loudness = DynamicLoudness::new(self.channels, new_rate);

// generation stays current, so process() never restores these lost controls.
self.dynamic_loudness.reset();
self.lifecycle.reset();

// alpha already contains sin(w0).
let shelf_term = 2.0 * a.sqrt() * alpha * sin_w0;

// The index is silently discarded, and the caller is told the edit succeeded.
pub fn set_band_gain(&mut self, band_idx: usize, gain_db: f64, sr: f64) {
    if band_idx >= EQ_BANDS {
        return;
    }
    let gain_db = gain_db.clamp(-15.0, 15.0); // NaN survives clamp
    ...
}

// One operation, two callback-visible snapshots: the block in between runs the
// new input gain against the old output gain.
self.saturation.set_input_gain(input);
self.saturation.set_output_gain(output);

// Lost update: a concurrent set_strength between read() and publish() is gone.
let mut snapshot = self.shared.read();
snapshot.ref_volume_db = Some(db);
self.shared.publish(snapshot);
```

### Correct

```rust
// The branch that accumulated the transition input owns the continuation.
active.clone_from(&target);

// Preserve controls/smoothers and rebuild only rate-dependent state.
self.dynamic_loudness.set_sample_rate(new_rate);

// A new signal stream retains the already-adopted control snapshot.
self.dynamic_loudness.reset();
self.apply_cached_params();
self.lifecycle.reset();

let shelf_term = 2.0 * a.sqrt() * alpha;

// Checked shell: one validator owner, then the crate-private kernel.
pub fn set_band_gain(
    &mut self,
    band_idx: usize,
    gain_db: f64,
    sample_rate: f64,
) -> Result<(), ProcessError> {
    validate_eq_band_index("Equalizer", band_idx)?;
    let gain_db = checked_band_gain(gain_db)?;
    self.set_band_gain_validated(band_idx, gain_db, sample_rate);
    Ok(())
}

// Audio thread: the snapshot is already sanitized, so no Result appears here.
self.eq
    .set_all_bands_validated(&self.cached.gains, self.sample_rate);

// One operation, one guarded publication carrying both fields.
self.saturation.set_gains_db(input, output);

// The decision and the mutation both happen inside the writer lock.
self.shared.update_if(|snapshot| {
    if snapshot.ref_volume_db == Some(db) {
        return false;
    }
    snapshot.ref_volume_db = Some(db);
    snapshot.volume = volume;
    true
});
```
