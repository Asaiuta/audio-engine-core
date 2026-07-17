# P0 DSP State Correctness Contracts

## Scope

This research freezes the current failure mechanisms and the independent
oracles for the `07-17-p0-dsp-state-correctness` implementation batch.

## EQ transition state

`Equalizer::process_sample_no_counter_update` advances both the current and
target biquad branches for every transition sample. When the counter reaches
zero, `copy_coefficients_from` copies only `b0/b1/b2/a1/a2`; the active branch
therefore continues with the old branch's `z1/z2`, even though the audible mix
has converged to the target branch.

Contract:

* the target branch is authoritative after the last crossfade frame;
* active coefficients and `z1/z2` must match the target branch exactly;
* continuation output must match a target filter that accumulated every
  transition input, within `1e-9` maximum linear error;
* the behavior must be identical for whole-buffer and irregular frame chunks,
  mono and stereo, without heap allocation in process.

## Loudness config publication

`LoudnessNormalizer::new` constructs `AtomicLoudnessState` with hard-coded
`enabled=true` and `mode=Track`. `set_config` updates threshold/smoothing and
target gain but also omits config `enabled` and `mode`. Explicit `set_enabled`
and `set_mode` update runtime state without updating the stored config.

Contract:

* construction and `set_config` publish `enabled` plus all five
  `NormalizationMode` values;
* explicit setters keep stored config and runtime atomics consistent;
* disabled processing is a transparent bypass;
* mode encoding is centralized so constructor, config updates, and explicit
  setters cannot drift.

## W3C/RBJ shelf coefficients

For shelf slope `S=1`:

```text
A = 10^(gain_db / 40)
alpha = sin(w0) / 2 * sqrt(2)
two_sqrt_a_alpha = 2 * sqrt(A) * alpha
```

The low/high shelf coefficient equations use `two_sqrt_a_alpha` directly. The
current implementation multiplies it by `sin(w0)` a second time. Existing
`legacy_low_shelf_coeffs` / `legacy_high_shelf_coeffs` helpers duplicate that
same expression and must be replaced rather than updated as the only oracle.

Oracle:

* a separately expressed W3C/RBJ coefficient reference at multiple rates,
  gains, and shelf frequencies;
* analytical `H(e^jw)` response comparisons at DC, shelf frequency,
  representative transition points, and Nyquist;
* coefficient absolute error `<= 1e-12`, response error `<= 1e-9 dB`.

Reference: <https://www.w3.org/TR/audio-eq-cookbook/>.

## Dynamic-loudness sample-rate updates

`DynamicLoudnessProcessor::set_sample_rate` currently assigns a fresh
`DynamicLoudness::new`, restoring strength and volume factor to defaults. The
direct processor also recreates smoothers, discarding their current and target
gains.

Contract:

* update rate-dependent geometry in place;
* preserve enabled state, strength, reference/transition controls, current
  loudness factor, and smoother current/target state;
* recompute smoother time constants for the new rate;
* reset biquad histories because old-rate delay state is not valid at the new
  geometry;
* rebuild each band's coefficients from its preserved current gain before the
  next processed frame;
* adapter and direct-processor tests assert the same policy.

## Validation matrix

| Failure mechanism | Regression assertion |
| --- | --- |
| EQ adopts coefficients only | active and target state equality plus continuation reference |
| EQ depends on callback chunking | whole vs irregular chunks, max error `<= 1e-9` |
| Config defaults overwrite caller | `enabled=false`, `Album` constructor round-trip |
| Config update omits runtime fields | all modes and enabled transitions reflected atomically |
| Shelf duplicates `sin(w0)` | W3C/RBJ coefficient and response oracle |
| Adapter reconstructs processor | volume factor/strength survive rate update |
| Direct rate update resets smoothers | current/target gains remain unchanged |
| New hot-path work allocates | transition and steady process no-allocation assertions |
