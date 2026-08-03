# Decide the legacy public surface lifecycle

1.0 release gate 2 of 9.

## Goal

Before 1.0 freezes the public API under SemVer, give every legacy or orphaned
public item an explicit disposition — keep, narrow, or remove — so the 1.0
surface contains nothing the project is not prepared to support for the life of
the major version. Gates 3–8 all operate on whatever this gate leaves behind, so
shrinking the surface here reduces the work in every one of them.

This closes P2 #9 from the `07-28-codebase-maintainability-audit`, which was
deliberately left open with the note: *"Which items are supported, deprecated,
or internal is a product decision, not one this session can make from source
alone."*

## Context established during discovery

All line references verified against current source on 2026-08-03.

Crate is at `version = "0.1.0"`, `rust-version = "1.87"`. Gate 1 landed a
`public-api` baseline (`tests/public-api-all-features.txt`,
`tests/public-api-rubato.txt`) plus an MSRV CI gate, so every surface change
here is reviewable as a baseline diff. **The crate uses `#[deprecated]` nowhere
and `#[doc(hidden)]` nowhere.**

### A real downstream consumer exists

The audit searched only within this repository. `D:/AI/AudioPlayer` is a
separate repo depending on this crate as
`audio-engine-core = { git = "https://github.com/Asaiuta/audio-engine-core", branch = "main" }`
(`AudioPlayer/Cargo.toml:110`). It re-exports whole modules
(`pub use audio_engine_core::{decoder, pipeline, processor};`,
`AudioPlayer/src/lib.rs:20`), so every `processor::*` item is reachable
downstream regardless of this crate's root re-exports.

Checking each item against that consumer splits the audit's single "zero
consumers" list into groups that need materially different handling.

**Group A — genuinely used downstream; removal would be a real breaking change:**

| Item | Downstream usage |
|---|---|
| `FirEq`, `FirPhaseMode`, `STANDARD_BANDS` | an entire API module — `AudioPlayer/src/player/fir_eq_api.rs:108` builds a `FirEq`; `player/mod.rs:60,64,287-288` hold `FirPhaseMode` / `STANDARD_BANDS` as player state |
| `CrossfeedSettings` | return type of `get_crossfeed_info` (`player/effects_api.rs:111-113`) |
| `SaturationSettings` | return type of `get_saturation_info` (`player/effects_api.rs:73-75`) |
| `DEFAULT_STREAMING_TARGET_LUFS` | `server/playback/analysis.rs:223,274` |

`FirEq` is additionally advertised in this crate's own `README.md:51` feature
table and documented at `docs/quality.md:243`. Calling it "benchmark-only" is
true of in-repo *code* consumers and false of the product.

**Group B — orphan on both sides.** Re-verified by locating the `#[cfg(test)]`
boundary in each defining file, because the audit's phrase "used only inside its
own file" conflates *production use* with *its own unit tests*:

| Item | Only consumers | Test-boundary evidence |
|---|---|---|
| `RingBuffer` | own tests + `benches/audio_component_perf.rs:565,587` | def `pipeline.rs:1496`; all call sites `:1652+`, after `#[cfg(test)]` at `:1642` |
| `VolumeController` | own tests | def `dsp.rs:43`; all call sites `:662+`, after `#[cfg(test)]` at `:653` |
| `GainRamp` | own tests | `ramp.rs` sites `:150+` after `#[cfg(test)]` at `:148`; `loudness.rs:44,60` sit inside `#[cfg(test)] mod tests` at `:29` |
| `AtomicDynamicLoudnessState` | **nothing constructs it anywhere** | only def `dynamic_loudness.rs:786`, impls `:795,:846`, re-export `mod.rs:65` |
| `DEFAULT_BROADCAST_TARGET_LUFS` | nothing | only def `loudness_db.rs:36` + re-export `mod.rs:76` |
| `ConvolverControl::publish` | ~25 test sites | production uses `publish_at_rate` (`pipeline.rs:1041`); the `.publish(` hits in `pipeline.rs:1012-1038` are the unrelated private `LifecycleChannel::publish`, those in `lockfree_params.rs` are private snapshot publishes |

**Group C — load-bearing internal type, exported incidentally:**

`BiquadSection` is **production-critical**, not an orphan. `Equalizer` stores
`bands: Vec<[BiquadSection; EQ_BANDS]>` and `target_bands` (`eq.rs:91-92`),
builds banks via `build_channel_bank` (`:124`), and constructs sections through
`BiquadSection::peaking_eq` (`:126`, `:186`) — all before `#[cfg(test)]` at
`:317`. The audit listed it under "public items with no in-repo consumer", which
is wrong: it has no consumer *outside its defining file* but is the building
block of the production IIR `Equalizer` inside it.

### Two corrections to the task description

1. **`PolyphaseResampler` is not exported.** It is `pub(super)`
   (`polyphase_backend.rs:22`) and absent from both public-API baselines (0
   matches each), so it is not a public-surface lifecycle decision. What remains
   is hygiene: its two `#[allow(dead_code)]` methods (`:117` `output_delay`,
   `:188` `reset`) are the only dead-code allowances in production code, and
   they rot invisibly because the rubato backend tree compiles solely under
   `--no-default-features --features rubato`.
2. **`RingBuffer` already has a documented support status**, added deliberately
   in `c30f1f7` (2026-08-01, the audit's own P1 remediation):
   *"Supported, but not used by any other type in this crate. It exists for a
   consuming application that needs a decode-side producer/consumer conduit."*

### Already resolved, not in scope

The four orphan effect configs the audit flagged as P1 (`SaturationConfig`,
`DynamicLoudnessConfig`, `CrossfeedConfig`, `DitherConfig` in `config.rs`) were
deleted by the audit's own P1 pass. Confirmed absent from the public-API
baseline; only the `Playback*` facade configs remain.

## Decisions

| Item | Decision | Rationale |
|---|---|---|
| `RingBuffer` | **Keep as-is** | The support status already exists and was written deliberately four days ago. Gate 2's requirement for this item is satisfied; re-opening it would discard a fresh decision. Its benchmark and `docs/quality.md:162` entry stay. |
| `VolumeController` | **Remove** | Strictly slower than the production `VolumeProcessor` and behaviorally different, so keeping it offers library users a worse option presented as an equal one. `process_validated` (`dsp.rs:119-127`) has no settled fast path — at constant volume 1.0 it still does a multiply-add plus a bounds-checked multiply per sample, where `VolumeProcessor` returns without touching the buffer. Its smoothing constant is 20 ms (`dsp.rs:16`) versus 5 ms, and it is wired to neither `AtomicVolumeParams` nor the lifecycle/bypass contract. |
| `GainRamp` | **Remove** | Only its own unit tests use it. The facade's `apply_stop_ramp` (`pipeline.rs:1409-1417`) is the live implementation and the more correct one: it advances gain per *frame*, so both channels of a frame share a gain, and recomputes from `remaining/total`, so it cannot drift. `GainRamp::apply` advances per *sample*, giving L and R different gains within one frame in stereo. Its cheaper inner loop (multiply+add per sample versus one divide per frame) is irrelevant — a stop fade runs once per track stop, 960 divisions for a 20 ms fade at 48 kHz. |
| `AtomicDynamicLoudnessState` | **Remove** | Nothing constructs it in `src/`, `tests/`, or `benches/`. Its role — a lock-free UI-thread→DSP control bridge — is now filled by `lockfree_params` and the facade's `PlaybackParameters`. Keeping it freezes a competing control mechanism into 1.0. |
| `DEFAULT_BROADCAST_TARGET_LUFS` | **Remove** | Zero references anywhere. Accepted consequence: the pair becomes asymmetric — `DEFAULT_STREAMING_TARGET_LUFS` stays because the downstream uses it, while the EBU R128 broadcast counterpart goes. |
| `ConvolverControl::publish` | **Remove** | Self-labelled legacy at `control.rs:9-11`; hardcodes 44,100 Hz; production already moved to `publish_at_rate`. Migrate its ~25 test sites. |
| `DEFAULT_CONVOLVER_SAMPLE_RATE_HZ` | **Remove** (cascade) | Exists solely to serve `publish` — its own doc calls it *"Compatibility rate used by the legacy `ConvolverControl::publish` API"*. After that removal its only remaining consumer is the `#[cfg(test)] pub(crate)` helper `publish_with_drop_probe` (`control.rs:202`), which will take an explicit rate instead. |
| `BiquadSection` | **Narrow to `pub(crate)`** | An implementation detail of `Equalizer`, not a user-facing type; exporting it freezes the biquad memory layout into 1.0. It cannot be removed, so narrowing visibility is the whole decision. No downstream consumer. |
| `PolyphaseResampler` | **Scope under `#[cfg(test)]`** | Verified safe: every reference is inside a test module — `contiguous_polyphase_backend.rs:417,485,591` (after `#[cfg(test)]` at `:415`) and `spectral_backend.rs:322,390,441` (after `:320`). The `rubato_backend.rs` hits are the different production type `ContiguousPolyphaseResampler`. Gating the struct and its `impl` removes both `#[allow(dead_code)]` allowances. The file's `pub(super)` design helpers (`:197-331`) are production-used and stay ungated. |
| Group A (4 entries) | **Keep, add support-status docs** | Each gains a short support note in the RingBuffer style, stating it exists for a consuming application. Export paths are unchanged — no promotion to root re-export, to avoid widening the root namespace on the way into 1.0. |

Performance caveat for the `VolumeController` / `GainRamp` removals: no
benchmark covers either (`benches/` has no case for them; the 50.3 ns figure at
`docs/quality.md:214` is the whole DSP chain). The comparison is read from the
source, not measured.

## Decision (ADR-lite)

**Context.** The crate is at 0.1.0 heading into a 1.0 that will freeze its API
under SemVer. A maintainability audit identified a legacy surface with no
support policy but explicitly declined to decide the policy, because that is a
product judgement. One downstream consumer exists, controlled by the same
author.

**Decision.** Remove rather than deprecate. Six items plus one cascading
constant are deleted outright; two types narrow visibility; `RingBuffer` and the
four downstream-used Group A entries are kept with explicit support statements.
`#[deprecated]` is deliberately not used: pre-1.0 there is no published
obligation to any of these items, so deprecating would ship a 1.0 that is born
carrying deprecated API, buying a migration window nobody needs when the sole
consumer is updated by the same author.

**Consequences.** The 1.0 surface shrinks by nine names, so gates 3–8 have less
to convert, document, and SemVer-check. The downstream breaks on its next
`cargo update` until two names are dropped from its re-export list — recorded as
a follow-up rather than fixed here, because it lives in a different repository.
`DEFAULT_STREAMING_TARGET_LUFS` survives without its broadcast counterpart,
which reads as an inconsistency to anyone who does not know the usage history;
the support note should say so. Narrowing `BiquadSection` means a future user
wanting a standalone RBJ peaking biquad must ask for it back, which is the
intended direction — it is easier to add a type in 1.x than to remove one.

## Spec drift caused by these removals

Trellis specs reference several items being removed or narrowed. Found by
grepping `.trellis/spec/` for every affected name.

| Spec | Line | Reference | Action |
|---|---|---|---|
| `streaming-lifecycle.md` | 71-74 | `VolumeController::with_sample_rate` / `::process` in the documented signature block | delete the block |
| `streaming-lifecycle.md` | 641 | `ConvolverControl::publish(&self, kernel: FFTConvolver) -> u64` | delete the line; `publish_at_rate` on `:642` stays |
| `realtime-safety.md` | 62 | *"`ConvolverControl::publish`, `reclaim_retired`, and `status` are control/offline operations"* | change `publish` to `publish_at_rate` |
| `directory-structure.md` | 39 | `dsp.rs # db<->linear, VolumeController, NoiseShaper` | drop `VolumeController` |
| `directory-structure.md` | 90 | `ramp.rs # GainRamp` | delete the line — the file is gone |
| `directory-structure.md` | 40 | `eq.rs # 10-band IIR (BiquadSection, Equalizer)` | accurate as a file-content description; note `BiquadSection` is crate-internal |
| `dsp-state-correctness.md` | 29 | `BiquadSection::copy_coefficients_from` in the signature block | keep — the section documents internal contracts too; mark it crate-internal |

**One spec needed no change, and it was written before the code was checked.**
`listening-nonlinear-correctness.md:290` states *"`PolyphaseResampler` is
retained `#[cfg(test)]`-only as the parity oracle."* An earlier draft of this
PRD claimed the code had drifted from it. That was wrong: the struct at
`polyphase_backend.rs:21` and its `impl` at `:38` were already gated with
`#[cfg(test)]`. The only real work was deleting the two `#[allow(dead_code)]`
methods (`output_delay`, `reset`), which had no caller even in test builds —
the `output_delay`/`reset` calls in `rubato_backend.rs:720,756` dispatch to the
`ContiguousPolyphaseResampler` variant, a different type.

### Dead code revealed by narrowing `BiquadSection`

Narrowing to `pub(crate)` made rustc report `copy_coefficients_from`
(`eq.rs:83`) as never used — a `pub` method on a `pub` type is reachable from
outside the crate, so dead-code analysis had never flagged it. It had zero
callers in `src/`, `tests/`, or `benches/`. The production `Equalizer` adopts a
fully processed crossfade branch with `clone_from` (`eq.rs:241,313`), which
carries `z1/z2`; `copy_coefficients_from` deliberately retained them, and its
own doc explained why the crossfade path must not use it.

It was removed rather than kept behind a re-introduced `#[allow(dead_code)]`,
which would have contradicted this gate's removal of the crate's last two such
allowances. Three passages in `dsp-state-correctness.md` (`:59`, `:237`, `:299`)
named the method; they now state the same rule — coefficients-only copy versus
whole-value branch adoption — without naming a function the crate no longer
offers.

## Requirements

* Every item in the verified inventory has an explicit disposition, applied in
  code, with no item left implicit.
* Removals are complete: definition, `impl` blocks, unit tests, every re-export
  hop, and any constant orphaned by the removal.
* Group A items and `RingBuffer` carry a support statement in their doc comment
  saying they exist for a consuming application.
* The public-API baselines are regenerated so the surface change is a reviewable
  diff.
* The downstream breakage is recorded as an explicit follow-up, not silently
  left for a future `cargo update` to discover.

## Acceptance Criteria

All verified on branch `chore/gate2-legacy-public-surface`.

* [x] `VolumeController`, `GainRamp`, `AtomicDynamicLoudnessState`,
      `DEFAULT_BROADCAST_TARGET_LUFS`, `ConvolverControl::publish`, and
      `DEFAULT_CONVOLVER_SAMPLE_RATE_HZ` are absent from both public-API
      baselines.
* [x] `BiquadSection` is absent from both baselines and `Equalizer` still
      compiles and passes its tests.
* [x] `RingBuffer` and the four Group A entries are still present in both
      baselines, each with a support statement in its doc comment.
* [x] `polyphase_backend.rs` contains no `#[allow(dead_code)]` — and neither
      does anything else in `src/`. Its `pub(super)` design helpers still
      compile under `--no-default-features --features rubato`.
* [x] The `publish` test sites and the 6 `publish_with_drop_probe` sites are
      migrated (33 rewrites); convolver lifecycle test coverage is unchanged in
      substance.
* [x] `cargo test --all-features` passes: 454 lib, 20 bench-support, 2
      public-api, 25 resampler-support (1 ignored), 3 Windows deployment, 6
      doctests.
* [x] `cargo test --no-default-features --features rubato` passes: 485 lib plus
      the same suites; the polyphase parity-oracle tests still run.
* [x] `cargo clippy --all-targets --all-features -- -D warnings` and the rubato
      equivalent pass.
* [x] `cargo fmt --all -- --check` passes.
* [x] The baseline diff is 229 lines, **all deletions, zero additions** — no
      incidental surface drift.
* [x] Every spec reference listed under *Spec drift* is updated.
* [x] `cargo doc` is warning-free on both feature sets (narrowing
      `BiquadSection` and removing `VolumeController` broke two intra-doc links
      in `processor/mod.rs:13-14`, which would have failed the docs.rs parity
      gate).

Library test count moved 462 → 454 under all-features. The eight removed tests
are accounted for exactly: three `VolumeController` tests in `dsp.rs`, two
`GainRamp` tests in `loudness.rs`, and three inside the deleted `ramp.rs`. The
geometry-rejection contract that `raw_volume_geometry_rejection_is_atomic_and_allocation_free`
covered remains enforced for every surviving raw DSP entry point
(`dsp.rs` NoiseShaper, `dynamic_loudness/tests.rs:545,573`,
`loudness/limiter.rs:986,1011`, `loudness/normalizer.rs:393,425`,
`adapters/tests.rs:61-97`).

## Definition of Done

* Tests updated for every removal; no test deleted without confirming the
  behavior it covered is either gone or covered elsewhere
* Clippy and the full test matrix green on both feature sets
* CHANGELOG entry listing every removed and narrowed item as a breaking change
* README / `docs/quality.md` checked for references to removed items
* The downstream follow-up written down where it will be seen

## Implementation Plan

**PR1 — removals with no cross-file protocol change.**
`VolumeController` (`dsp.rs:43-145` plus the now-orphaned
`VOLUME_SMOOTHING_TIME_MS` at `:16` and its tests at `:662+`);
`AtomicDynamicLoudnessState` (`dynamic_loudness.rs:786-853`);
`DEFAULT_BROADCAST_TARGET_LUFS` (`loudness_db.rs:36`); `GainRamp` — delete
`src/processor/loudness/ramp.rs` (220 lines) entirely, plus `loudness.rs:19`
`mod ramp;`, `:27` `pub use ramp::GainRamp;`, the `:10` doc line, and the two
tests at `:44,:60`. Drop each from `processor/mod.rs` (`:63`, `:65`, `:70`,
`:76`) and `lib.rs` (`:133`, `:139`). Lean on clippy to find orphaned imports.

**PR2 — convolver publish migration.** Remove `ConvolverControl::publish`
(`control.rs:126-135`) and `DEFAULT_CONVOLVER_SAMPLE_RATE_HZ` (`:11`); give
`publish_with_drop_probe` (`:202`) an explicit rate parameter and update its 6
call sites (`adapters/convolver/tests.rs:512,516`,
`adapters/tests.rs:397,399,404,405`); migrate the ~25 `publish` call sites to
`publish_at_rate`. Drop the constant's re-exports at `adapters/convolver.rs:8`,
`adapters.rs:1879`, `processor/mod.rs:95`, `lib.rs:140`.

**PR3 — visibility narrowing.** `BiquadSection` → `pub(crate)` (`eq.rs:26`) and
out of `processor/mod.rs:67`. `PolyphaseResampler` struct and `impl` → under
`#[cfg(test)]` (`polyphase_backend.rs:22-195`), dropping both
`#[allow(dead_code)]`; validate under `--no-default-features --features rubato`,
where this tree actually compiles.

**PR4 — documentation, specs, and baselines.** Support statements on
`RingBuffer`'s four Group A peers; the seven spec edits listed under *Spec
drift*; CHANGELOG breaking-change entries; regenerate both public-API baselines
and review the diff line by line.

## Out of Scope

* Gates 3–8: typed errors, `MediaLocation`, capability model, parameter
  validation policy, resampler facade geometry, `deny(missing_docs)` and
  `cargo-semver-checks` enforcement.
* Re-litigating the already-deleted `config.rs` effect configs.
* Any behavior change to code that stays.
* Promoting Group A items to root-level re-exports.
* **Updating the downstream `AudioPlayer`.** It is a separate repository. After
  this gate lands, its next `cargo update` will fail to compile until
  `VolumeController` and `GainRamp` are dropped from the `pub use processor::{…}`
  list at `AudioPlayer/src/lib.rs:29,31`. Nothing else downstream is affected —
  neither name has any real use there. Recorded here as the follow-up.

## Technical Notes

* Prior audit:
  `.trellis/tasks/archive/2026-08/07-28-codebase-maintainability-audit/research/04-legacy-and-duplication.md`
  (original P2/P3 findings) and `.../08-p2-reverification-and-remediation.md`
  (Part 6 records P2 #9 as deliberately not changed).
* Gate 1 baseline: `tests/public_api.rs`, `tests/public-api-all-features.txt`,
  `tests/public-api-rubato.txt`.
* Feature-matrix caveat: the rubato backend tree compiles only under
  `--no-default-features --features rubato`, so the `PolyphaseResampler` change
  is invisible to default (soxr) builds and must be validated on that feature
  set specifically.
