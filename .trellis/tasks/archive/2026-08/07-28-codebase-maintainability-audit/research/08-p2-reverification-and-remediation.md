# P2 re-verification and remediation

## Scope and snapshot

- Session: 2026-07-31, local time, continuing the user-authorized remediation
  extension recorded in the PRD addendum.
- Branch: `main`. HEAD is still `0c62febd2b6afdd1800da1591b68f7a600a3835e`; all
  work described here is in the uncommitted working tree.
- Purpose: the user asked to continue into the P2 boundary debt from
  `99-final-report.md`, re-verifying each finding against current source before
  editing, then choosing refactor / simplification / minimal fix per finding.
  Destructive change was authorized where warranted, over-design was not.
- Every P2 item was re-read against current source. Several had been fixed
  incidentally by the `07-28`/`07-29` sibling tasks and by this task's own P1
  pass; those are recorded as fixed rather than re-fixed.

## Part 1 — re-verification of all twelve P2 findings

| # | Finding | Status at re-verification | Evidence |
|---:|---|---|---|
| 1 | Processor capability broader than schedulers can honor | mostly fixed | `traits.rs:746` now declares `supports_bypass`, documenting that rate conversion and volume return `false`; `dsp_chain.rs:174-190` `add` rejects a zero chain rate and any stage that does not map the chain rate to itself. Residual: `DspChain::new`/`with_capacity` still accept rate `0`. |
| 2 | Callback/offline/single-consumer ownership mixed | partly fixed | The lease test now proves post-acquisition release (`output_chain/tests.rs:72-94` asserts the failure is *not* `InvalidSampleRate`); `output_chain.rs:1379-1405` documents the rate-validation order. Residual: `OutputChainParams.source_sample_rate` still required by the callback build; `PlaybackController` proxied three `PlaybackParameters` operations. |
| 3 | Standalone DSP validation does not share facade policy | partly fixed | `lockfree_params.rs:1327-1343` `set_ref_volume_db` now converts and publishes inside the writer guard via `update_if`; `loudness/meter.rs:198-220` added `is_available` / `has_reliable_measurement`. Residual and live: every infallible standalone setter still accepted non-finite input. |
| 4 | AutoMix inconsistent scopes and weak result types | partly fixed | `automix_analysis.rs` `decode_segment` now passes one selected `&chunk[sample_range]` to the meter and the envelopes together, so the metric scopes agree; `AutomixKeyStatus` reports `Unsupported` explicitly. Residual and live: `Result<_, String>`; **unbounded `energy_profile` allocation from container-declared duration**. |
| 5 | Decoder source/error identity string- and path-driven | mostly fixed | `decoder/source.rs:43-78` introduces the typed `MediaLocation` with case-insensitive scheme matching and a single local branch; `decoder/error.rs` classifies transport by `io::ErrorKind` before message text and splits `FeatureUnavailable` from `UnsupportedFormat`. Residual: `DecodeCancelToken::new` still took an `Arc<AtomicBool>`. |
| 6 | Public metadata is also mutable decoder control state | live | `streaming.rs` had `pub info: AudioInfo` on both `StreamingDecoder` and `StreamingDecoderBuilder`; `decoder/tests.rs:513,548,570` mutated `info.encoder_delay` / `info.end_padding` directly, and the same fields drive staging size, trim counters, seek math, and position. |
| 7 | Loudness cache has no stable identity/freshness contract | mostly live | `loudness_db.rs`: `version < CURRENT_SCAN_VERSION` treated a newer row as fresh; `if let Ok(metadata)` fell through to "fresh" for an unreadable local file; `get_outdated_tracks` dropped row-decoding errors via `filter_map(.ok())`; scheme checks were case-sensitive, unlike the decoder's router. Partly fixed earlier: track-id case folding is now Windows-only. |
| 8 | Resampler engine contracts hidden behind a weak facade | live | `resampler/mod.rs:483-503`: `max_output_len_for_input` and `input_frames_for_output_frames` still carry magic `64` margins and unchecked arithmetic; `max_output_samples_per_chunk` swallows a layout error as `0`. |
| 9 | Legacy public surface without a lifecycle policy | partly fixed | The four orphan effect configs were deleted by this task's P1 pass. Residual: `RingBuffer`, `VolumeController`, the oracle `PolyphaseResampler`, `ConvolverControl::publish`, and the benchmark-only FIR EQ remain exported without a documented support status. |
| 10 | State representations require synchronized manual edits | mostly fixed | `lockfree_params.rs:582-655` now holds exhaustive `From` conversions in both directions with no `_` fallback arm, so a new variant fails to compile rather than silently defaulting; every `[f64; 7]` is now `[f64; LOUDNESS_BANDS_N]`, including the facade DTO at `pipeline.rs:663-668`. Residual: saturation config field fan-out. |
| 11 | Benchmark authority uneven outside the shared harness | partly fixed | The P1 pass corrected `docs/quality.md` and the gapless `--enforce` false-green. Residual: gapless still accepts and ignores shared baseline flags; the ten per-probe baseline/enforcement branches still have no synthetic tests. |
| 12 | Playback facade has no Trellis-owned contract | live | No spec described `PlaybackPipeline`; worse, `directory-structure.md:25` still called `pipeline.rs` "RingBuffer streaming primitive" and `logging-guidelines.md:24` listed it among files where **logging is allowed**, although it now contains the realtime callback entry point. |

## Part 2 — a P1-grade defect found inside P2 #4

`build_energy_profile` sized a whole-track `Vec<f64>` at ten slots per second
directly from `effective_duration`, which came from `AudioInfo::duration_secs`
or `total_frames` — container metadata nobody has verified. The only filter was
`is_finite() && > 0.0`.

A file declaring `1e12` seconds therefore requested `1e13` `f64` slots (~80 TB).
`vec![0.0; n]` has no fallible form, so this aborts the process rather than
returning `Err`. It is reachable from the public `analyze_automix` entry point
with an ordinary untrusted file, which makes it a denial-of-service defect
rather than the boundary smell its P2 rank implied. It is fixed here rather than
deferred with the rest of P2.

## Part 3 — remediation judgement

| # | Judgement | Rationale |
|---:|---|---|
| 4 (allocation) | minimal fix, discard not clamp | The existing code already had a "declared duration unusable → fall back to measured head evidence" path. Extending the unusable predicate with a ceiling reuses that path, so every derived field stays mutually consistent. Clamping instead would report a confident 24-hour timeline the file never supported. |
| 3 | minimal fix, one shared policy | `lockfree_params::sanitized` already encoded exactly the right rule for the atomic layer. Promoting it to `pub(crate)` and routing the cores through it is smaller than a new validation module and removes the split policy the audit named. |
| 6 | simplification (breaking) | An accessor plus one narrowly named crate-private test hook removes the control channel outright. Deprecating the field would preserve it for another release for no benefit; the crate is pre-1.0 and the field has three in-repo consumer sites. |
| 7 | minimal fix plus recorded limits | The freshness rule was right in shape and wrong in three specific predicates. Fixing the predicates is a few lines. The remaining identity problems (remote replacement, sub-second mtime, non-canonical paths) genuinely need typed local/remote identities, so they are written into the spec as known limitations instead of being half-fixed. |
| 2 (residual) | simplification (delete) | The three proxies had zero consumers in `src/`, `benches/`, `examples/`, `tests/`, or the docs. Deleting them gives every ordinary control exactly one owner. |
| 5 (residual) | minimal API fix | `new()` + `cancel()` lets the token own its protocol; `from_flag` retains the previous constructor for callers that must adopt an existing flag, so no capability is lost. |
| 12 | documentation correction plus one owned contract | The logging half was actively dangerous and is a correction, not an enhancement. The facade contract follows the file's existing `## Scenario:` idiom rather than adding a new spec document. |

No structural refactor was judged worthwhile for this set. In particular the
audit's own warning was honoured: nothing was rewritten for file size, and the
justified-complexity list in `99-final-report.md` was left intact.

### One judgement corrected mid-session

The first implementation of #3 also clamped `PeakLimiter::set_threshold` and
`set_release_ms` to the published `LIMITER_*` ranges, matching the other cores.
Re-reading the callers showed this was wrong: `adapters.rs:1265` deliberately
drives the core *below* `LIMITER_THRESHOLD_DB_MIN`, because the intersample-peak
guard subtracts its additive bound from the user's ceiling before the value
reaches the limiter. Clamping there would have silently disabled that guard for
a user ceiling at the published minimum. The limiter therefore keeps only the
non-finite rejection, and
`limiter_threshold_is_drivable_below_the_published_user_range` pins the
distinction. The general rule — published ranges bound what a *user* may
request, not what internal machinery may drive — is now written into
`realtime-safety.md`.

## Part 4 — changes applied

### #4 unbounded energy-profile allocation

- `src/processor/automix_analysis.rs`: new `MAX_DECLARED_DURATION_SEC`
  (24 hours) and named `ENERGY_PROFILE_RATE`. `is_plausible_duration` gates both
  `duration_secs` and the `total_frames`-derived duration;
  `build_energy_profile` clamps at the allocation site as well, so it does not
  depend on caller discipline. `frames_to_seconds` replaces two inline
  conversions.
- Tests `an_absurd_declared_duration_cannot_size_the_energy_profile` and
  `an_implausible_declared_duration_falls_back_to_measured_head_evidence`.

### #3 non-finite input to standalone DSP setters

- `src/processor/lockfree_params.rs`: `sanitized` is now `pub(crate)` and
  documented as the shared policy for both parameter layers.
- `saturation.rs`: `set_drive`, `set_threshold`, `set_mix`, `set_input_gain`,
  `set_output_gain`, `set_highpass_cutoff` drop non-finite writes.
- `dsp.rs`: `VolumeController::set_target` likewise.
- `dynamic_loudness.rs`: `set_volume`, `set_volume_percent`, `set_volume_db`,
  `set_strength`, `set_reference_volume_db`, `set_transition_db` likewise. The
  last two gained named private constants (`REFERENCE_VOLUME_DB_MIN/MAX`,
  `TRANSITION_DB_MIN/MAX`) with the same values as the previous literals; they
  stay core-owned because no public control exposes them.
- `loudness/limiter.rs`: `set_threshold`, `set_threshold_db`, `set_release_ms`
  drop non-finite writes and deliberately apply no published-range clamp.
- `fir_eq.rs`: `set_sample_rate` rejects non-positive/non-finite rates;
  `set_band` and `set_bands` route through `sanitized`, and `set_bands` is now
  all-or-nothing.
- `crossfeed.rs` needed no change: its `sanitize_*` helpers already fell back to
  defaults on non-finite input.
- Tests: `standalone_setters_drop_non_finite_writes` (saturation, including a
  post-write process pass asserting finite output),
  `volume_target_drops_non_finite_writes`,
  `limiter_setters_drop_non_finite_writes`,
  `limiter_threshold_is_drivable_below_the_published_user_range`,
  `fir_eq_setters_drop_non_finite_writes`, `fir_eq_set_bands_is_all_or_nothing`.

### #6 decoder metadata as observation only

- `src/decoder/streaming.rs`: `info` is private on both `StreamingDecoder` and
  `StreamingDecoderBuilder`, each with a `pub fn info(&self) -> &AudioInfo`.
  `#[cfg(test)] pub(crate) set_gapless_counters_for_test` names the only two
  fields a test may override.
- Call sites updated: `automix_analysis.rs`, `decoder/tests.rs`,
  `benches/audio_decoder_perf.rs`, `benches/audio_gapless_comparison_perf.rs`,
  and the `README.md` loudness example.

### #7 loudness cache freshness

- `src/processor/loudness_db.rs`: shared case-insensitive `is_remote_path`
  replaces two case-sensitive prefix checks; `needs_scan` uses exact version
  matching, returns "needs scan" for an unreadable local file, and documents
  remote records as version-only; `get_outdated_tracks` propagates a
  row-decoding failure and matches `scan_version != ?1`.
- The module doc records the three remaining identity limitations.
- `test_database_basic_operations` asserted the old behavior for a path that
  never existed on disk; it now asserts the corrected contract.
- Tests `an_unreadable_local_file_needs_a_rescan`,
  `a_newer_scanner_version_also_needs_a_rescan`,
  `remote_identities_are_recognized_case_insensitively`.

### #2 and #5 residuals

- `src/pipeline.rs`: `PlaybackController::set_volume`, `set_muted`, and
  `dynamic_loudness_telemetry` deleted; the type doc now states that the
  controller owns only the convolver lease and the lifecycle channel.
- `src/decoder/error.rs`: `DecodeCancelToken::new()` takes no argument,
  `cancel()` added, previous constructor available as `from_flag`. Test
  `cancel_token_owns_its_own_flag`.

### #12 and the spec corrections

- `logging-guidelines.md`: `pipeline.rs` moved from the allowed list to the
  forbidden list, with the reason recorded; `processor/resampler.rs` corrected
  to `processor/resampler/mod.rs`; `decoder/source/http.rs` and
  `processor/loudness_db.rs` added to the allowed list.
- `directory-structure.md`: the `src/` tree now matches the real layout —
  `pipeline.rs` described as the playback facade, `resampler/` and its six
  backends, `output_chain.rs` + `output_chain/`, `dsp_chain/`, `traits/`,
  `saturation/`, `dynamic_loudness/`, `convolver/`, `fir_design.rs`, and the
  limiter's selectable detection mode.
- `streaming-lifecycle.md`: new `## Scenario: Callback Playback Facade
  Ownership` covering realtime prohibitions on `process`, the split control
  authority, the packed coalescing lifecycle channel, build-time drain-policy
  validation, strict-build vs clamping-runtime validation, the idle-silence
  exception, and IR adoption.
- `realtime-safety.md`: new `### One Validation Policy For Both Parameter
  Layers`, including the published-range vs core-range distinction.
- `analysis-fir-correctness.md`: new `### Declared duration is untrusted input`.
- `decoder-correctness.md`: `AudioInfo` is observation, not control.
- `database-guidelines.md`: new `## Cache Freshness Contract` plus exact version
  matching and the recorded limitations.
- `CHANGELOG.md` and `README.md` updated for the breaking changes.

## Part 5 — validation performed

All commands were run to completion in this session on the current dirty working
tree.

| Command | Outcome |
|---|---|
| `cargo fmt --all -- --check` | passed (after one `cargo fmt --all` pass) |
| `cargo clippy --all-targets --all-features -- -D warnings` | passed |
| `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings` | passed |
| `cargo test --all-features` | passed: 462 library, 20 benchmark-support, 25 resampler-support, 3 Windows deployment, 6 doctests; 1 native-shim support test ignored because its separately built shim was absent |
| `cargo test --no-default-features --features rubato` | passed: 493 library, 20 benchmark-support, 25 resampler-support, 3 Windows deployment, 6 doctests; same 1 ignored |
| `git diff --check` | passed; only the pre-existing LF-to-CRLF working-copy warnings |

Feature matrix:

| Command | Outcome |
|---|---|
| `cargo build --no-default-features --features rubato` | built |
| `cargo build --no-default-features --features soxr` | built |
| `cargo build --no-default-features --features rubato,http` | built |
| `cargo build --no-default-features --features rubato,loudness-db` | built |
| `cargo build --no-default-features` | failed on the missing-backend guard, as the spec requires |

Library test count moved from 458 to 462 under all-features during this session
as tests were added; the 439 figure in `07-` was the pre-session baseline.

No benchmark binary was executed. This session makes no timing, regression,
device, driver, DAC, or end-to-end latency claim.

### Limits of this validation

- The energy-profile defect is demonstrated by a unit test asserting the bound,
  not by executing the pre-fix abort. Reproducing the abort would have required
  provoking an ~80 TB allocation; the arithmetic (`1e12 s × 10 slots/s × 8 B`)
  is stated instead.
- No adversarial container fixture with a forged duration header was built. The
  fix is applied at the two points where declared duration enters and at the
  allocation site, all of which are covered by unit tests.
- The loudness-cache tests use real temporary files, but no test covers a
  same-size replacement within one second of the recorded mtime; that gap is
  recorded as a known limitation rather than closed.

## Part 6 — deliberately not changed

Recorded so a later session does not read silence as completion.

- **P2 #1 residual.** `DspChain::new(0)` / `with_capacity(_, 0)` still accept a
  zero rate. It is inert: `add` rejects a zero-rate chain, so such a chain can
  never hold a processor, `latency()` returns `ZERO`, and `tail()` returns
  `None` for an empty chain. Making the constructors fallible would ripple
  through every call site for no reachable defect. Separately, `add` still
  cannot verify that a processor was *configured* at the chain rate — the trait
  exposes no rate getter, only `output_sample_rate_hz`.
- **P2 #2 residual.** `OutputChainParams` still carries `source_sample_rate`
  through the callback build that ignores it. Splitting it into callback and
  render parameter types is a genuine improvement but touches every builder call
  site in `pipeline.rs` and the benches; it is not justified by any defect, only
  by clarity, and the misleading order is now documented at the definition.
- **P2 #4 residual.** `analyze_automix` still returns `Result<_, String>`,
  erasing cancellation, decoder, seek, and I/O classes. This is a real API
  weakness and a contained one (one module plus the error type), but it is an
  additive design task rather than a fix, and it was not started so that it is
  not left half-done.
- **P2 #8.** The resampler facade still has magic `64` margins, unchecked
  capacity arithmetic, an error-swallowing `map_or(0, ...)`, and `Standard`/
  `High` mapping to one SoXR recipe. Tightening it needs a resolved-recipe and
  exact-units model across the public sizing helpers, their callers, and the
  benchmark quality labels.
- **P2 #9.** The legacy public surface still has no lifecycle policy. Which
  items are supported, deprecated, or internal is a product decision, not one
  this session can make from source alone.
- **P2 #10 residual.** Saturation configuration still crosses a facade config,
  snapshot, adapter cache, setter list, and core copy through hand-written
  fields. The compiler-enforced enum conversions removed the highest-risk half.
- **P2 #11 residual.** Gapless still accepts and ignores shared baseline flags,
  and the per-probe baseline/enforcement branches still lack synthetic tests.
- **P3 themes** from `99-final-report.md` are untouched.
- The four untracked root Markdown files, `.pi-subagents/`, and `.tmp/` remain
  untouched, as in the original audit.
