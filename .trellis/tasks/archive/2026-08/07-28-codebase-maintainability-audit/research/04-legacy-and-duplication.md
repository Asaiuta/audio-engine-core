# Legacy surface and duplicated sources of truth

## Snapshot and scope

- Local audit window: 2026-07-28 16:32:29 +08:00 onward; HEAD unchanged at
  `0c62febd2b6afdd1800da1591b68f7a600a3835e`.
- The working tree remained dirty with the same tracked modifications as the
  baseline, plus `CHANGELOG.md` and `README.md`.
- Key mtimes at snapshot: `src/pipeline.rs` 99,106 bytes at 15:41:20;
  `src/processor/lockfree_params.rs` 47,638 bytes at 15:41:20;
  `src/lib.rs` 6,309 bytes at 15:24:03; `src/processor/adapters.rs`,
  `src/processor/resampler/mod.rs`, and `src/processor/output_chain.rs`
  unchanged since before the baseline. Line references below are against this
  snapshot and may shift with the ongoing facade work.
- Method note: the audit's dedicated content-search tool returned no matches
  for this workspace path, so all content searches were performed with
  read-only `grep`.

**Coverage limitation.** Two sub-scopes were completed: (1) legacy surface
across `src/`, `tests/`, `benches/`, `examples/`; (2) duplicated sources of
truth inside `src/`. The third sub-scope — duplication between production
code and `tests/`/`benches/`/`examples/` (bench adapters vs production
resampler code, repeated bench helpers, copy-pasted test fixtures) — was
started but not completed; a partial probe found only three signal-generator
helpers across `benches/` (`resampler_comparison_support/quality.rs:624,660`,
`audio_quality_measurements.rs:3006`), which weakly suggests bench helpers are
centralized, but this is not confirmed evidence. That sub-scope remains an
open follow-up for area 05 (tests and benchmarks).

## Verdict

There is no `#[deprecated]` attribute and no removal-TODO anywhere in the
inventory, yet the crate carries a real legacy surface: an entire unused serde
config layer duplicated by the new facade, several superseded public types
with zero in-repo consumers, and a self-labeled compat API. Separately, the
"single source of truth" claim of `lockfree_params.rs` is undermined in
practice: DSP cores re-encode clamp bounds as literals, defaults exist in up
to four copies (one already three-way diverged), and enum/u8 mappings and
field-copy chains require multi-file synchronized edits. Most items are
maintainability debt, but the crossfeed default divergence and the unclamped
saturation gains in the core are behavior-relevant.

## Confirmed findings — legacy surface

### P1 — `src/config.rs` effect configs are an orphaned, drifting duplicate of the facade configs

**Category**: legacy surface + duplicated source of truth.

- `SaturationConfig` (`src/config.rs:111-124`), `DynamicLoudnessConfig`
  (`:144-153`), `CrossfeedConfig` (`:165-172`), and `DitherConfig` (`:183-190`)
  have zero references anywhere in `src/`, `tests/`, `benches/`, or
  `examples/` outside `config.rs` itself, yet remain publicly reachable via
  `audio_engine_core::config::*`.
- The new facade re-implements the same knobs as `PlaybackSaturationConfig`
  (`src/pipeline.rs:282`), `PlaybackCrossfeedConfig` (`:363`),
  `PlaybackDynamicLoudnessConfig` (`:398`), `PlaybackNoiseShapingConfig`
  (`:433`).
- Drift has already occurred: crossfeed default mix is `0.3` in
  `config.rs:169`, `0.5` in `pipeline.rs:373`, and `0.35` via
  `crossfeed.rs:18` / `lockfree_params.rs:872-880` (see duplication P1 below).
- Non-legacy exceptions in the same file: `LoudnessConfig` and
  `NormalizationMode` are production-used by
  `src/processor/loudness/normalizer.rs` and re-exported at `src/lib.rs:113`.

Direction: remove or deprecate the four orphaned structs, or make the facade
configs delegate to them so one definition owns the defaults.

### P2 — superseded public types with no production consumer

**Category**: legacy surface.

- `RingBuffer` (`src/pipeline.rs:1466`, re-exported `src/lib.rs:119`): its
  only consumer, the legacy `AudioPipeline`, was removed (CHANGELOG.md:288-299
  records this). Remaining uses are its own unit tests
  (`pipeline.rs:1624-1687`) and `benches/audio_component_perf.rs`. It now
  shares a file with the unrelated new `PlaybackPipeline` facade.
- `VolumeController` (`src/processor/dsp.rs:35`, exported via
  `processor/mod.rs:63` and the crate root): zero uses outside its own file;
  the production volume path is `VolumeProcessor` + `AtomicVolumeParams`
  (`src/processor/adapters.rs:1445`, wired in `output_chain.rs:1004`).
- `PolyphaseResampler` (`src/processor/resampler/polyphase_backend.rs:22`):
  superseded by `ContiguousPolyphaseResampler` in the production engine
  (`rubato_backend.rs:704,720`); constructed only inside `#[cfg(test)]`
  modules as an oracle. Its two `#[allow(dead_code)]` methods
  (`polyphase_backend.rs:117-120` `output_delay`, `:188-194` `reset`) are the
  only dead-code allowances in production code. The file's design helpers
  (`:197-331`) remain production-used by the contiguous and spectral backends,
  so the file is half-live, half-oracle. Direction: scope the struct behind
  `#[cfg(test)]` instead of per-method `#[allow(dead_code)]`.
- Note: the rubato backend tree only compiles under
  `--no-default-features --features rubato`, so dead-code rot there is
  invisible to default (soxr) builds; these two allowances are exactly that
  rot.

### P2 — self-labeled compat API: `ConvolverControl::publish`

**Category**: legacy surface, self-documented.

- `src/processor/adapters/convolver/control.rs:9-11` documents
  `DEFAULT_CONVOLVER_SAMPLE_RATE_HZ = 44_100` as the "Compatibility rate used
  by the legacy [ConvolverControl::publish] API"; the constant is re-exported
  at the crate root (`src/lib.rs:136`).
- Production uses the newer `publish_at_rate` (`control.rs:138`, called from
  `src/pipeline.rs:1041`); `publish` (`control.rs:126`) is called only from
  tests (~25 sites in `src/processor/adapters/tests.rs` and
  `src/processor/adapters/convolver/tests.rs`).

Direction: mark `publish` `#[deprecated]` (the crate currently uses that
attribute nowhere) and migrate the test call sites.

### P3 — public items with no in-repo consumer at all

**Category**: legacy surface / orphan API; legitimate downstream surface is
possible for a library crate, but none of these is exercised even by tests or
benches unless noted:

- `GainRamp` (`src/processor/loudness/ramp.rs:10`; own tests only) — the new
  facade implements stop-fade itself.
- `BiquadSection` (`src/processor/eq.rs`), `CrossfeedSettings`
  (`crossfeed.rs`), `SaturationSettings` (`saturation.rs`),
  `AtomicDynamicLoudnessState` (`dynamic_loudness.rs`) — exported from
  `processor/mod.rs:62-65` with only their defining files using them.
- `DEFAULT_BROADCAST_TARGET_LUFS`, `DEFAULT_STREAMING_TARGET_LUFS`
  (`src/processor/loudness_db.rs:16,19`; exported `processor/mod.rs:74`) —
  zero uses anywhere.
- `FirEq`, `FirPhaseMode`, `STANDARD_BANDS` (`src/processor/fir_eq.rs`) — a
  parallel EQ implementation whose only consumer is
  `benches/audio_fir_eq_perf.rs`; production EQ is the IIR `Equalizer` in the
  output chain.
- Diagnostics exports `callback_stage_order_csv`,
  `offline_render_stage_order_csv`, `post_render_analysis_order_csv`,
  `RESAMPLER_BACKEND_NAME` (`src/lib.rs:122-137`) are bench-only consumers but
  have a documented diagnostic purpose (`resampler/mod.rs:40-44`) — recorded,
  not flagged.

### P3 — working-tree debris unrelated to the crate

`CACHE_CONTROL_FIX.md`, `FIX_SUMMARY.md`, `RELAY_CACHE_TTL_ISSUE.md`, and
`cache_control_analysis.md` at the repo root are untracked Chinese-language
debugging notes about an unrelated AI tool's API cache TTL issue, alongside
untracked `.pi-subagents/` and `.tmp/`. They must not be committed; deletion
is a user decision, not an audit action.

## Confirmed findings — duplicated sources of truth in `src/`

`src/processor/lockfree_params.rs:35-37` declares itself the single source of
truth for published control-value ranges. The findings below measure the code
against that claim.

### P1 — crossfeed default mix has three diverged values

**Category**: duplicated source of truth, already drifted.

- `src/processor/crossfeed.rs:18` `DEFAULT_MIX = 0.35` (used by
  `CrossfeedParamsSnapshot::default`, `lockfree_params.rs:872-880`);
- `src/pipeline.rs:373` `PlaybackCrossfeedConfig::disabled()` uses `mix: 0.5`;
- `src/config.rs:169` `CrossfeedConfig::default()` uses `mix: 0.3`.

Which default a caller gets depends on entry path. This is the only confirmed
already-diverged value; it should be collapsed to one constant.

### P1 — DSP cores re-encode clamp bounds as literals; saturation gains unclamped in core

**Category**: duplicated validation; one enforcement gap.

Canonical bounds live at `lockfree_params.rs:40-95`, but the cores re-clamp
with independent literals (all currently agreeing): EQ gain ±15 dB
(`eq.rs:115`), saturation drive/threshold/mix/highpass
(`saturation.rs:385,390,395,439`), crossfeed mix (`crossfeed.rs:247-253`),
volume (`dsp.rs:80`), noise-shaper bits (`dsp.rs:182,262,308`), dynamic
loudness strength (`dynamic_loudness.rs:549,731,737`). If a lockfree constant
changes, cores silently re-clamp to the stale literal and the change never
reaches audio.

Gap: saturation input/output gain has ±24 dB bounds only at
`lockfree_params.rs:73-75`; the core setters `saturation.rs:399-408` do not
clamp at all, so the core alone accepts any finite gain. Two core-only ranges
have no lockfree counterpart: `dynamic_loudness.rs:558` (`-30..0`) and `:563`
(`10..40`).

Model citizen for contrast: crossfeed cutoff bounds are defined once in
`crossfeed.rs:239-245` and aliased by `lockfree_params.rs:81-83` — one truth,
two enforcement points.

### P2 — enum/u8 mappings are split-brain match tables

**Category**: duplicated mappings.

- `SaturationType` (`saturation.rs:22-28`) ↔ `SaturationTypeValue`
  (`lockfree_params.rs:533-571`) and `SaturationQuality` (`saturation.rs:35-41`)
  ↔ `SaturationQualityValue` (`lockfree_params.rs:574-612`): three hand-written
  match tables each; `From<u8>` silently maps unknown discriminants to a
  default variant. Adding a variant needs 4+ edits.
- `NoiseShaperCurve` is defined in `dsp.rs:110-132` but its u8 encoding lives
  in `lockfree_params.rs:614-637`; a new curve compiles clean in `dsp.rs` and
  misroutes in the u8 tables.
- Enum-default disagreement: `SaturationType` declares `#[default] Tape`
  (`saturation.rs:23`) while every default constructor picks `Tube`
  (`pipeline.rs:295-307`, `lockfree_params.rs:656-672`, `config.rs:111-124`,
  `saturation.rs:297-326`) — a fifth, disagreeing "default" for anyone calling
  `SaturationType::default()`.

### P2 — hand-copied field chains for saturation state

**Category**: duplicated state representation.

Flow is `PlaybackConfig` → `*ParamsSnapshot` → adapter cache → core fields,
each hop a hand-written field list: `PlaybackBuilder::build()` constructs the
11-field `SaturationParamsSnapshot` at `pipeline.rs:1098-1110`; the adapter
re-lists 8 setters twice (`adapters.rs:369-380` and `:410-437`); the core
hand-lists ~20 fields in `copy_from_preallocated` (`saturation.rs:340-381`)
and again in `get_settings` (`:1077-1090`); the adapter additionally clones
the core three times for quality crossfade (`adapters.rs:381-384`). A new
snapshot field must be added in three to five places or it silently never
reaches audio.

Related: snapshot `Default`s are effect-ON (saturation `lockfree_params.rs:668-669`,
crossfeed `:877`, limiter `:963`, noise shaper `:1117`, dynamic loudness
`:1201`) while facade `disabled()` constructors are all OFF; the builder
overwrites everything, so behavior is correct, but `Default` on snapshots is
a misleading second truth.

### P3 — duplicated helpers and unit-system duplication

**Category**: maintainability smell.

- dB↔linear: canonical helpers in `dsp.rs:16-29` ("Shared across all
  processor modules"), yet `pipeline.rs:944-952` privately re-implements them
  and its `linear_to_db` lacks the ≤0 guard (`dsp.rs` returns
  `NEG_INFINITY`) — an edge-behavior difference. Further inline conversions:
  `lockfree_params.rs:1279`, `crossfeed.rs:40-43`,
  `dynamic_loudness.rs:444,512-516`, `fir_eq.rs:151,184`, `loudness_db.rs:144`,
  `output_chain.rs:183`, `automix_analysis.rs:526`.
- Tail/finish policy in two unit systems: `ChainFinishPolicy::default()`
  (`dsp_chain.rs:88-91`, −120 dBFS / 12,000 / 1,440,000 frames) equals
  `UnknownTailPolicy::default()` (`output_chain.rs:60-68`, −120 dBFS / 250 ms
  / 30,000 ms) only at 48 kHz; the threshold→linear conversion also differs in
  form (`powf(x/10.0)` at `dsp_chain.rs:84` vs amplitude-then-square at
  `output_chain.rs:183-185`) — equivalent today, breakable independently.
- Loudness band count 7: single-sourced as `LOUDNESS_BANDS_N`
  (`dynamic_loudness.rs:380`) but hardcoded as `[f64; 7]` in `pipeline.rs:654`
  and `lockfree_params.rs:1322,1334,1347-1349`; parallel telemetry structs
  `DynamicLoudnessTelemetry` (`pipeline.rs:654`) and
  `AtomicDynamicLoudnessTelemetry` (`lockfree_params.rs:1320-1351`).
- Peak limiter release default: snapshot 150 ms
  (`lockfree_params.rs:959-968`) vs limiter doc 100 ms
  (`loudness/limiter.rs:174-176`); lookahead 10 ms is a bare literal at
  `adapters.rs:1190-1197`. Doc-level drift.
- Default sample rate 44,100 appears as at least seven independent literals
  (`crossfeed.rs:16`, `convolver/control.rs:11`, `adapters/convolver.rs:52,64`,
  `adapters.rs:392,1465,1476`, `dsp.rs:46`, `saturation.rs:297-326`,
  `decoder/streaming.rs:232`); numerically agreeing, no shared constant.
- RBJ peaking-EQ coefficient math duplicated between `eq.rs:18-41` and
  `dynamic_loudness.rs:149-233` with separate biquad types; formulas agree.
- Minor: both resampler backends define a max reduced rate of 1,024
  (`polyphase_backend.rs:18-19`, `rubato_backend.rs:68`).
- `pipeline.rs:96` `MAX_STOP_FADE_MS = 60_000` is packed into a u16 lifecycle
  field (`pipeline.rs:230-258`); 60,000 < 65,535 holds but is unchecked by
  type — implicit coupling, not duplication.

## Important non-findings / justified cases

- **Two-policy validation is one truth, two policies.** Facade strict
  validation (`pipeline.rs:110-141,550-638`) checks against constants
  re-exported from `lockfree_params.rs`; runtime setters clamp via
  `sanitized()` (`lockfree_params.rs:104-106`). Area 01 already accepted this
  as coherent; it is not drift. The Layer-3 core literals above are the actual
  duplication.
- **"Legacy"-named production modes are intentional**: `LimiterMode::SamplePeak`
  (`loudness/limiter.rs:31`; CHANGELOG.md:162 records it as preserved) and
  `SaturationQuality::Direct` (`saturation.rs:32`, the default quality) are
  live behavior choices, not dead code.
- **All `legacy_*`/`Legacy*` test structures are bit-exactness oracles**
  (`dsp.rs:613`, `loudness/limiter.rs:399`, `saturation/tests.rs:4`,
  `spectrum.rs:146-195`, `resampler/mod.rs:636-653`,
  `benches/audio_lockfree_params_perf.rs:396`,
  `benches/audio_quality_measurements.rs:1763-1803`) — justified.
- **The soxr/rubato dual backend has no dead alternative**: `compile_error!`
  guard and documented precedence (`resampler/mod.rs:9-38`); both backends are
  tested and benched. `OutputChainBuilder`/`DspChain` are not orphaned by the
  facade — construction delegates to them (`pipeline.rs:10`, CHANGELOG:56).
- **File-wide `#![allow(dead_code)]` in shared bench/test support**
  (`tests/resampler_comparison_support.rs:1`,
  `benches/audio_resampler_comparison_perf.rs:1`,
  `benches/audio_gapless_comparison_perf.rs:18`) is the shared-module idiom,
  though the blanket allowance would hide genuinely dead support code.
- **EQ band count is the model constant**: `EQ_BANDS = 10` single-sourced at
  `lockfree_params.rs:440` and re-exported/used generically (`eq.rs:3`,
  `pipeline.rs:471`).
- Facade dynamic-loudness volume in dB (−120..0, `pipeline.rs:102-104`) vs
  lockfree linear 0..1 (`lockfree_params.rs:89-91`) is an intentional domain
  conversion at the boundary, not a duplicate.

## Ranked risks for the final synthesis

1. Crossfeed default mix three-way divergence (already drifted; behavior
   depends on entry path).
2. Core clamp literals shadowing lockfree constants, plus unclamped
   saturation gains in the core.
3. Enum/u8 split-brain tables with silent fallback arms.
4. Orphaned `config.rs` effect configs duplicating the facade configs.
5. Hand-copied saturation field chains (3-5 synchronized edits per new field).
6. Private dB helpers in `pipeline.rs` with differing edge behavior.
7. Tail policy duplicated in frames vs ms, equal only at 48 kHz.
8. Superseded public types (`RingBuffer`, `VolumeController`,
   `PolyphaseResampler`, `ConvolverControl::publish`).

## Open follow-ups

- Complete the production-vs-bench/test duplication sub-scope (hand to area
  05): bench adapter re-implementation of resampler design code, repeated
  bench measurement helpers, copy-pasted `#[cfg(test)]` fixtures, and whether
  `examples/` uses the current facade.
- Decide the fate of the four root markdown debris files before any commit.
