# Restore dynamic-loudness tuning parameters (pre-gain, transition, compensation reference)

## Problem

`DynamicLoudness` owns three tuning values that shape the compensation curve:

| Field | `dynamic_loudness.rs` | Default | Setter on the DSP core |
|---|---|---|---|
| `pre_gain_linear` | :426, initialized :468 | `10^(-3/20)` (−3 dB) | **none** |
| `transition_db` | :422, initialized :467 | 25.0 | `set_transition_db` (:610) |
| `ref_volume_db` (compensation onset) | :420, initialized :466 | −15.0 | `set_reference_volume_db` (:598) |

None of them is reachable through the parameter layer:

- `DynamicLoudnessParamsSnapshot` (`lockfree_params.rs:1300`) carries only
  `enabled` / `volume` / `strength` / `ref_volume_db`.
- `DynamicLoudnessProcessor::apply_cached_params` (`adapters.rs:1743`) forwards
  only `set_volume` + `set_strength`. The two existing DSP-core setters are
  dead code from the chain's point of view.
- `pre_gain_linear` has no setter at all — the −3 dB bass-boost headroom is a
  hardcoded constant.

**Naming hazard (root cause of the confusion):** the snapshot's existing
`ref_volume_db` field means "the listening volume, expressed in dB" — it is
converted to `volume` at `lockfree_params.rs:1396` and never reaches the DSP
core. `DynamicLoudness::ref_volume_db` means something else entirely: the
volume threshold *below which* compensation starts. Two different quantities,
one name. The new parameter is therefore named `compensation_ref_db`, and the
existing `ref_volume_db` keeps its current meaning untouched.

### Downstream evidence

`D:\AI\VCPChat\rust_audio_engine` exposes all three as configuration:

- `config.rs:340` `VCP_AUDIO_DYNAMIC_LOUDNESS_PRE_GAIN_DB` (clamp −6..0)
- `config.rs:328` `VCP_AUDIO_DYNAMIC_LOUDNESS_TRANSITION_DB` (clamp 10..40)
- `config.rs:322` `VCP_AUDIO_DYNAMIC_LOUDNESS_REF_DB` (clamp −30..0)

wired through `player/mod.rs:147-149` → `lockfree_params.rs:931-950` →
`adapters.rs:614-616`. Migrating that app onto this crate currently loses all
three knobs.

## Goal

Publish the three tuning values through the lock-free parameter layer so both
the construction path and the running callback can set them, without breaking
the 1.x public API.

## Non-goals

- Changing any default. `-3 dB` / `25 dB` / `-15 dB` stay exactly as they are;
  a caller that never touches the new API must be sample-identical.
- Touching `DynamicLoudnessParamsSnapshot`. It has public fields and no
  `#[non_exhaustive]`, so adding a field trips `constructible_struct_adds_field`
  under `cargo semver-checks --release-type patch` and would force 2.0.0.
- Restoring `GainRamp`, `VolumeController`, `ChainStats`/`ProcessorStats`,
  `ProcessResult::StaleParams`, `LockfreeParams::has_update`, or a public
  `BiquadSection`. Each is either dead code downstream or has a strictly
  stronger upstream replacement; see the analysis in the session that produced
  this task.

## Design

### 1. New snapshot type (`processor/lockfree_params.rs`)

```rust
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct DynamicLoudnessTuningSnapshot {
    pub pre_gain_db: f64,
    pub transition_db: f64,
    pub compensation_ref_db: f64,
}
```

`#[non_exhaustive]` from day one so later tuning values are additive.
`Default` yields `-3.0 / 25.0 / -15.0`, matching `DynamicLoudness::new_validated`.

New public range constants, mirroring the private ones in `dynamic_loudness.rs`
(`REFERENCE_VOLUME_DB_MIN/MAX`, `TRANSITION_DB_MIN/MAX`) plus a new pre-gain pair:

```
DYNAMIC_LOUDNESS_PRE_GAIN_DB_MIN         = -6.0
DYNAMIC_LOUDNESS_PRE_GAIN_DB_MAX         =  0.0
DYNAMIC_LOUDNESS_TRANSITION_DB_MIN       = 10.0
DYNAMIC_LOUDNESS_TRANSITION_DB_MAX       = 40.0
DYNAMIC_LOUDNESS_COMPENSATION_REF_DB_MIN = -30.0
DYNAMIC_LOUDNESS_COMPENSATION_REF_DB_MAX =  0.0
```

The private constants in `dynamic_loudness.rs` are re-pointed at these so the
two layers cannot drift. All setters route through the existing `sanitized`
helper (non-finite input keeps the previous value).

### 2. Second `SharedParams` inside the existing publisher

```rust
pub struct AtomicDynamicLoudnessParams {
    shared: SharedParams<DynamicLoudnessParamsSnapshot>,
    tuning: SharedParams<DynamicLoudnessTuningSnapshot>, // private, additive
}
```

Adding a private field to a struct that has no public fields is not a SemVer
break. New public methods on the same type:

- `set_pre_gain_db(f64)` / `set_transition_db(f64)` / `set_compensation_ref_db(f64)`
- `write_tuning(pre_gain_db, transition_db, compensation_ref_db)` — one coherent publish
- `read_tuning() -> DynamicLoudnessTuningSnapshot`
- `subscribe_realtime_tuning() -> (RealtimeSnapshotReader<..>, .., u64)`
- `load_realtime_tuning_if_changed_since(&reader, u64) -> Option<(.., u64)>`

Independent generation counters mean a tuning change never invalidates the
hot `volume`/`strength` snapshot and vice versa.

### 3. DSP core setter (`processor/dynamic_loudness.rs`)

Add `DynamicLoudness::set_pre_gain_db(&mut self, db: f64)`, clamped and
`sanitized`, storing `10^(db/20)` into the existing `pre_gain_linear` field.
This is a plain field assignment — no coefficient redesign, no allocation.
`set_transition_db` / `set_reference_volume_db` already exist and are reused.

### 4. Adapter wiring (`processor/adapters.rs`)

`DynamicLoudnessProcessor` gains `tuning_reader` + `tuning_generation`,
registered in `new` via `subscribe_realtime_tuning` (setup-time allocation,
which the realtime spec permits). `sync_params` grows a second
`load_realtime_..._if_changed_since` branch that forwards the three values.

Realtime-safety: the added hot-path cost is one `Acquire` load of a generation
counter per block when nothing changed, and on change a hazard-slot copy of a
24-byte `Copy` struct plus three field assignments (one `powf` for pre-gain,
executed on the control-owned change edge only, same as the existing
`set_volume`/`set_strength` path). No allocation, lock, log, or panic is added.

### 5. Facade (`pipeline.rs`)

- `PlaybackDynamicLoudnessConfig` (already `#[non_exhaustive]`) gains the three
  fields; `disabled()` / `enabled()` fill in the defaults; `validate` range-checks
  them through the existing `checked_config_value`.
- `PlaybackBuilder::build` publishes them via `write_tuning`.
- `PlaybackParameters::set_dynamic_loudness_tuning(...) -> Result<(), ProcessError>`
  and a `dynamic_loudness_tuning() -> (f64, f64, f64)` reader, matching the
  existing `set_dynamic_loudness` / `dynamic_loudness` pair.

`OutputChainParams` is untouched — it already carries the
`Arc<AtomicDynamicLoudnessParams>`, which now transports the tuning snapshot too.
That matters because `OutputChainParams` is *not* `#[non_exhaustive]`.

## Acceptance criteria

1. Setting each of the three values through `AtomicDynamicLoudnessParams` from a
   control thread changes the audible output of a running `DynamicLoudnessProcessor`.
2. Defaults are unchanged: a chain built without touching the new API produces
   bit-identical output to `main`.
3. Non-finite input keeps the previous value; out-of-range input clamps.
4. Tuning publication does not bump the `volume`/`strength` generation
   (and vice versa).
5. `cargo semver-checks --release-type patch` passes against both committed
   baselines — i.e. the change is purely additive.
6. `tests/public-api-*.txt` regenerated and reviewed; every added line is an
   addition, no line is removed or altered.
7. `cargo fmt --check`, `cargo clippy -- -D warnings`, full `cargo test` green.

## Version impact

Additive-only → **1.1.0** (minor). `CHANGELOG.md` `[Unreleased]` → `### Added`.
