# P1 re-verification and remediation of the unclaimed findings

## Scope and snapshot

- Re-verification and remediation session: 2026-07-30, local time 20:30–21:15
  +08:00.
- Branch: `main`. HEAD is still
  `0c62febd2b6afdd1800da1591b68f7a600a3835e`; all work described here is in the
  uncommitted working tree.
- Purpose: the user asked to re-verify the audit's findings against current
  source and then remediate, judging per finding whether a refactor, a
  simplification, or a minimal fix is warranted.
- This extends the audit task beyond its original read-only PRD scope. The
  extension is user-authorized; see the PRD's remediation addendum.

## Part 1 — re-verification of P1 #1 to #10

Between the 2026-07-28 audit and this session, eleven sibling remediation tasks
were created and their fixes landed in the working tree. Each of P1 #1 to #10 was
re-checked against current source rather than assumed from task existence.

| # | Finding | Status | Evidence in current source |
|---:|---|---|---|
| 1 | HTTP Range responses trusted without 206/`Content-Range`/bounded body | fixed | `src/decoder/source/http.rs:170-240` parses and rejects unit, syntax, missing total, multiple totals, `start > end`, and `end` beyond total |
| 2 | Credentials and signed URLs printable | fixed | `src/decoder/source.rs:29-30` redacts username/password in `Debug`; `src/decoder/source/http.rs:926-931` asserts secrets are absent from diagnostics |
| 3 | Unsupported positioned channels discarded then guessed | fixed | new `src/decoder/channel_layout.rs:9-27`: explicit metadata owns its slot count; count inference only when metadata is absent; unspecified positions stay `Unspecified` |
| 4 | AutoMix Full mode omits the tail on overlapping windows | fixed | `src/processor/automix_analysis.rs:413-431` plans an explicit tail window with a checked `start - realized_start` skip |
| 5 | `DynamicLoudnessProcessor::reset` leaves the cached generation | fixed | `src/processor/adapters.rs:186-221` tracks `cached_generation` through `load_realtime_if_changed_since` |
| 6 | Raw DSP processors accept zero/mismatched geometry | fixed | `src/processor/spectrum.rs:24-33,60` and `src/processor/loudness/normalizer.rs:34-42` validate before constructing state |
| 7 | `decode_all` / budget resolver use unchecked conversions | fixed | `src/decoder/streaming.rs:64-80,170-203` use `usize::try_from` plus `checked_mul`/`checked_add` |
| 8 | Unsupported-target audio-thread init logs from the callback | fixed | `src/runtime.rs:1-23` gates on a thread-local `Cell` and contains no logging |
| 9 | `set_eq_band_gain_db` returns `Ok` for an out-of-range band | fixed | `src/pipeline.rs:706-713` calls `validate_eq_band_index` before the write |
| 10 | `set_saturation_gains_db` performs two publications | fixed | `src/pipeline.rs:801-816` validates both, then one `saturation.set_gains_db` |

Conclusion: no remediation was needed for #1 to #10. They are complete in the
working tree and covered by the sibling tasks' own tests.

## Part 2 — re-verification of P1 #11 to #17

These seven had no dedicated task. All were confirmed still live, with one
partially remediated.

| # | Finding | Status at re-verification |
|---:|---|---|
| 11 | Invalid `ChainFinishPolicy` first rejected during callback drain | live: `dsp_chain.rs:63` `validate` was private and only reachable from `finish_with_policy`; `PlaybackConfig::validate` never inspected `drain_policy` |
| 12 | Gapless `--enforce` false-green | live: a failed probe was pushed to `skipped` and `continue`d, and `enforce_report` inspected only `validations` |
| 13 | Release checklist requires backend-less builds | live: `quality-guidelines.md:1102,1123-1125,1162-1171` contradicted the same file's line 190-192 backend invariant |
| 14 | `docs/quality.md` overstates every `--enforce` path | live: line 48 claimed performance `--enforce` "always validates ... complete work" |
| 15 | Four orphan public effect configs | live: `SaturationConfig`, `DynamicLoudnessConfig`, `CrossfeedConfig`, `DitherConfig` reachable through `pub mod config` with zero consumers in `src/`, `tests/`, `benches/`, `examples/` |
| 16 | Crossfeed default mix divergence | live: `0.3` (`config.rs:169`), `0.35` (`crossfeed.rs:18`), `0.5` (`pipeline.rs:373`) |
| 17 | DSP cores re-encode published clamp ranges | partially fixed: `eq.rs:181` had adopted `EQ_BAND_GAIN_DB_MIN/MAX`, but `saturation.rs`, `dsp.rs`, `crossfeed.rs`, and `dynamic_loudness.rs` still used literals, and the saturation gain setters applied no bound at all |

## Part 3 — remediation judgement

The audit deliberately warned against file-size-driven rewrites. Each finding
was therefore classified before any edit.

| # | Judgement | Rationale |
|---:|---|---|
| 11 | minimal fix | The validation rule already existed and was correct; only its call site was too late. Widening `validate` to `pub(crate)` and calling it from `PlaybackConfig::validate` moves the failure to build time without duplicating the rule or removing the callback-side check. |
| 12 | minimal fix | The enforcement predicate was right; the input was lossy. Separating "work never owed" from "work owed and failed" fixes the gate without a new harness. |
| 13, 14 | documentation correction | No runtime defect. The executable spec and the public quality prose asserted things the code contradicts. |
| 15 | simplification (delete) | Not a refactor candidate. A second configuration model with no consumer and already-drifted defaults has negative value; deprecation would preserve the drift for another release. User chose deletion. |
| 16 | follows from 15, plus one shared default | Deleting `CrossfeedConfig` removes the `0.3` owner. The remaining `0.5` was an arbitrary literal in a bypassed config, so it now reads the core's own default. |
| 17 | minimal fix, no new module | The published-range block in `lockfree_params.rs` already declares itself the single source of truth. Cores importing it is a smaller and more honest change than introducing a fourth constants module. |

No structural refactor was judged worthwhile for this set. The P2 boundary work
(typed `MediaLocation`, narrowed `DspChain` capability model, resampler capacity
contracts, loudness-cache identity) remains genuine but is multi-session
architectural work and was explicitly left out of this session's scope.

## Part 4 — changes applied

### #11 build-time drain policy validation

- `src/processor/dsp_chain.rs`: `ChainFinishPolicy::validate` is now
  `pub(crate)` with a doc note that the first `finish_with_policy` still
  validates, because a chain can be driven directly.
- `src/pipeline.rs`: `PlaybackConfig::validate` ends with
  `self.drain_policy.validate()?`, and `with_drain_policy` documents build-time
  rejection.
- New test `pipeline::playback_facade_tests::build_rejects_a_drain_policy_that_cannot_bound_a_tail`
  covers non-finite threshold, positive threshold, zero silence hold, and a cap
  below the hold, plus one accepted narrow policy.

### #12 gapless enforcement

- `benches/audio_gapless_comparison_perf.rs`: new `ProbeFailure` record and
  `Report::probe_failures`. An attempted fixture whose probe errors is recorded
  there instead of in `skipped`; `enforce_report` fails first on any probe
  failure; `print_report` emits `probe-failed` lines and now reports
  `verified_fixtures=` and `probe_failed=` instead of an ambiguous `fixtures=`.
- `skipped` keeps its original meaning: work the run never owed.

### #13 spec feature matrix

- `.trellis/spec/backend/quality-guidelines.md`: the code-review checklist, the
  release signature list, the good/base/bad examples, and the required tests now
  carry a resampler backend in every buildable combination, state that bare
  `--no-default-features` must fail the guard, and drop the stale "SoXR remains
  required" wording now that `rubato` exists.

### #14 quality prose

- `docs/quality.md`: performance `--enforce` is now described as validating
  finite timing, stable case keys, and report integrity **for the work each
  probe recorded**, with the gapless completeness rule stated explicitly. A new
  paragraph records that `audio_lockfree_params_perf` is an exploratory
  machine-local probe with no JSON artifact, no baseline or environment
  identity, no CI execution, and a fixed 3% same-run assertion.

### #15 and #16 orphan configs and crossfeed default

- `src/config.rs`: `SaturationConfig`, `DynamicLoudnessConfig`,
  `CrossfeedConfig`, and `DitherConfig` deleted, along with the
  `SaturationQuality`/`SaturationType` re-exports that existed only for them. A
  module doc now states why callback-stage knobs do not live here.
- `src/processor/lockfree_params.rs`: `pub(crate) CROSSFEED_MIX_DEFAULT` and
  `CROSSFEED_CUTOFF_HZ_DEFAULT` alias the crossfeed core's own defaults.
- `src/pipeline.rs`: `PlaybackCrossfeedConfig::disabled` reads those instead of
  `0.5`/`700.0`.
- New test `pipeline::playback_facade_tests::disabled_crossfeed_config_matches_the_core_profile`
  compares the bypassed config against `Crossfeed::new(48_000.0).get_settings()`.

### #17 published clamp ranges

- `src/processor/saturation.rs`: drive, threshold, mix, and high-pass cutoff read
  the published constants; `set_input_gain` and `set_output_gain` now clamp to
  `SATURATION_GAIN_DB_MIN`/`_MAX`, where previously they applied no bound.
- `src/processor/dsp.rs`: `VolumeController::set_target` uses
  `VOLUME_MIN`/`VOLUME_MAX`; both bit-depth clamps use
  `NOISE_SHAPER_BITS_MIN`/`_MAX`.
- `src/processor/crossfeed.rs`: `sanitize_mix` uses
  `CROSSFEED_MIX_MIN`/`_MAX`.
- `src/processor/dynamic_loudness.rs`: `set_strength` and
  `AtomicDynamicLoudnessState::set_volume`/`set_strength` use the published
  dynamic-loudness constants.
- New tests `saturation::tests::standalone_gain_setters_clamp_to_published_range`
  and `standalone_setters_clamp_to_published_ranges`.

## Part 5 — validation performed

All commands were run to completion in this session on the current dirty working
tree. Exact outcomes:

| Command | Outcome |
|---|---|
| `cargo fmt --all -- --check` | passed (after one `cargo fmt --all` pass on two files) |
| `cargo clippy --all-targets --all-features -- -D warnings` | passed |
| `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings` | passed |
| `cargo test --all-features` | passed: 439 library, 20 benchmark-support, 25 resampler-support, 3 Windows deployment, 6 doctests; 1 native-shim support test ignored because its separately built shim was absent |
| `cargo test --no-default-features --features rubato` | passed: 472 library, 20 benchmark-support, 25 resampler-support, 3 Windows deployment, 6 doctests; same 1 ignored |
| `git diff --check` | passed; only the pre-existing LF-to-CRLF working-copy warnings |

Feature matrix, run to confirm the spec text in #13 is executable rather than
aspirational:

| Command | Outcome |
|---|---|
| `cargo build --no-default-features --features rubato` | built |
| `cargo build --no-default-features --features soxr` | built |
| `cargo build --no-default-features --features rubato,http` | built |
| `cargo build --no-default-features --features rubato,loudness-db` | built |
| `cargo build --no-default-features` | failed on the missing-backend guard, as the spec now requires |

Gapless enforcement, reproducing the exact false-green scenario the audit
described (one passing fixture plus one attempted fixture whose probe fails):

- Fixtures: `good.flac` copied from `target/decoder-bench-corpus/stereo_s16_48k_80s.flac`,
  and `corrupt.flac` of 4,096 random bytes, both under a temporary
  `target/gapless-enforce-check/` directory that was deleted afterwards.
- `AUDIO_GAPLESS_FIXTURES="...good.flac;...corrupt.flac" cargo bench --bench
  audio_gapless_comparison_perf -- --quick --enforce` printed
  `validation path=...good.flac status=pass`, then
  `probe-failed path=...corrupt.flac error=project open: UnsupportedFormat`,
  then failed with
  `gapless correctness probes failed: ...corrupt.flac (project open: UnsupportedFormat)`
  and exit code 1. Before this change the same input would have been green.
- The good fixture alone, with `--enforce --out`, exited 0 and produced a report
  whose `probe_failures` is empty, whose single validation is `pass`, and whose
  `skipped` contains only the two genuinely absent MP3/CAF corpora.

No other benchmark was executed. This session makes no timing, regression,
device, driver, DAC, or end-to-end latency claim. The gapless run above is
correctness and enforcement evidence only; its printed millisecond figures are
incidental and were not compared against any baseline.

## Part 6 — deliberately not changed

Recorded so a later session does not read silence as completion.

- **`Report::probe_failures` is an additive field under the unchanged global
  `REPORT_SCHEMA_VERSION` (1).** The version is shared by every probe, so
  bumping it for one probe's added field would invalidate all baselines. The
  gapless report is `Serialize`-only and has no baseline consumer, so nothing
  deserializes the old shape. The audit's separate P3 point — that one global
  schema version covers unrelated report shapes — remains open.
- **Non-finite input to the standalone DSP setters.** `set_drive`,
  `set_threshold`, `set_mix`, and now the gain setters use plain `f64::clamp`,
  which returns `NaN` unchanged. This matches the surrounding setters and is
  unchanged behavior, but it is the P2 item "standalone DSP validation does not
  share the facade policy" and is still open.
- **`DynamicLoudness::set_reference_volume_db` (`-30.0..0.0`) and
  `set_transition_db` (`10.0..40.0`).** These literals were left alone because
  no published constant exists for either range, so they are core-owned rather
  than a re-encoding of a public contract. Publishing them is a candidate
  follow-up, not part of #17.
- All P2 boundary debt and P3 naming/duplication themes from
  `99-final-report.md`, except where a P1 fix incidentally touched them.
- The four untracked root Markdown files, `.pi-subagents/`, and `.tmp/` remain
  untouched, as in the original audit.
