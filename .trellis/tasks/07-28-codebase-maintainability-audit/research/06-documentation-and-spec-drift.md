# Public documentation and Trellis specification drift

## Snapshot and scope

- Review window ended at 2026-07-28 19:32:58 +08:00.
- HEAD remained `0c62febd2b6afdd1800da1591b68f7a600a3835e`.
- The concurrently edited files were re-read in their current working-tree
  state: `README.md` (2026-07-28 15:26:30), `CHANGELOG.md` (15:39:13),
  `src/lib.rs` (15:24:03), `src/pipeline.rs` (15:41:20),
  `src/processor/lockfree_params.rs` (15:41:20),
  `src/processor/mod.rs` (15:41:20), and `src/processor/traits.rs`
  (13:46:15). This audit did not modify them.
- Public/release documentation reviewed: `README.md`, `CONTRIBUTING.md`,
  `CHANGELOG.md`, `Cargo.toml`, crate-level rustdoc, both examples,
  `docs/installation.md`, `docs/quality.md`,
  `docs/resampler-comparison.md`, and `.github/workflows/ci.yml`.
- Trellis specifications reviewed against current source: the backend index,
  realtime safety, streaming lifecycle, error handling, logging, directory
  structure, database, and quality guidelines.
- This area evaluates whether a maintainer or user receives the current
  contract. It does not treat an old historical benchmark number as a runtime
  defect unless the document still presents it as the current architecture or
  current gate.

## Verdict

The documentation is strong in breadth but is no longer a coherent source of
truth. The current README, changelog, pipeline rustdoc, and several newer
Trellis scenarios accurately describe the new playback facade and hybrid
resampler. Older release checklists and module/lifecycle specs were not updated
with those changes. The result is not merely cosmetic: one executable Trellis
checklist requires feature combinations the crate intentionally rejects, the
public quality page overstates what `--enforce` proves, and a current-tense
resampler section describes a retired algorithm and baseline identity.

No broken Rust code block was found. README code is deliberately included as
doctests from `src/lib.rs`, and all six doctests passed under both all-features
and Rubato-only builds. The problems are semantic drift and missing ownership,
not examples that happen not to parse.

## Confirmed findings

### P1 - the Trellis release feature matrix requires impossible builds

**Category**: executable-spec defect / contradictory source of truth.

- The current source intentionally requires at least one resampler backend:
  `src/processor/resampler/mod.rs:16-20` emits `compile_error!` unless `soxr`
  or `rubato` is enabled. `Cargo.toml:35-53`, `README.md:285-303`, and the
  primary testing contract at
  `.trellis/spec/backend/quality-guidelines.md:187-210` all describe the
  supported matrices correctly.
- The later release checklist in the same quality spec instead requires
  `cargo build --no-default-features`, plus `http`-only and
  `loudness-db`-only variants, at `quality-guidelines.md:1121-1125`. It then
  says SoXR remains required when default features are disabled
  (`:1162-1166`) and again requires bare no-default builds/tests at
  `:1170-1171`. These statements describe the pre-pluggable-backend design and
  contradict both the source and the earlier section of the same file.
- The generic review checklist also says every feature must build individually
  (`quality-guidelines.md:1097-1105`), which is impossible for `http` or
  `loudness-db` without a backend.
- The same stale shorthand appears in
  `.trellis/spec/backend/database-guidelines.md:47-48`,
  `.trellis/spec/backend/error-handling.md:35-36`, and the contributor
  submission checklist at `CONTRIBUTING.md:90-93`.
- Direct validation confirmed the boundary. `cargo check
  --no-default-features` exited 1 at the intended resampler `compile_error!`
  (followed by cfg-cascade compiler errors), while `cargo check
  --no-default-features --features rubato --examples` exited 0.

Consequence: a future agent following the mandatory Trellis checklist cannot
make the prescribed gate green without weakening the intentional backend
invariant. It may misclassify the correct compile failure as a regression or
reintroduce an unconditional SoXR dependency. Feature checks must be expressed
as service-feature-plus-backend matrices, never as every optional feature in
isolation.

### P1 - public performance documentation overstates `--enforce`

**Category**: evidence correctness / false assurance.

- `docs/quality.md:46-50` says performance `--enforce` *always* validates
  finite timing, complete work, stable case keys, and report integrity.
- Area 05 established a concrete counterexample in
  `audio_gapless_comparison_perf`: a fixture whose correctness validation
  returns `Err` is moved into `skipped`, omitted from `validations`, and does
  not fail `--enforce` when another fixture passes. The command advertised at
  `docs/quality.md:40` can therefore exit green with an attempted fixture's
  correctness failure omitted from the enforced set.
- Area 05 also established that `audio_lockfree_params_perf`, listed beside
  the other performance commands at `docs/quality.md:19`, has no JSON report,
  environment identity, case set, work-integrity model, or multi-trial
  distribution. Its `--enforce` path is only one fixed 3% `assert!` against a
  same-process legacy measurement.
- `docs/quality.md:110-114` correctly says CI runs nine default-feature JSON
  probes and leaves gapless outside CI, but the lock-free probe is neither in
  those nine nor explicitly classified as an exploratory/nonstandard probe.

Consequence: readers can treat a green exit as stronger evidence than the
command supplies. Until the two legacy probes join the shared evidence model,
the public statement must enumerate exceptions and distinguish attempted
correctness failures from genuinely absent optional corpora.

### P2 - the playback facade has no Trellis-owned lifecycle or realtime contract

**Category**: boundary/specification debt.

- `src/pipeline.rs:1-6` now owns a caller-driven callback DSP facade.
  `PlaybackPipeline::process` is explicitly the callback hot path
  (`:1288-1327`), while `PlaybackController` owns lifecycle and convolver
  authority (`:954-1058`). The facade, its parameter ranges, and its two
  terminal behaviors account for most of the current `pipeline.rs`.
- No backend spec mentions `PlaybackPipeline`, `PlaybackController`,
  `PlaybackLifecycleState`, `ProcessError::InvalidParameter`, or
  `ProcessError::UnsupportedOperation`.
- `directory-structure.md:25` still describes `pipeline.rs` solely as a
  `RingBuffer` primitive. The realtime hot-path inventory at
  `realtime-safety.md:8-23` lists `DspChain`, adapters, and parameter readers
  but not `PlaybackPipeline::process`, callback drain, or lifecycle command
  consumption.
- `logging-guidelines.md:18-27` classifies `pipeline.rs` generically with
  setup/runtime paths where logging is allowed. That file now mixes the
  callback facade with a legacy `RingBuffer::write` warning at
  `src/pipeline.rs:1508-1518`; a file-wide classification is no longer safe.
- The generic `StreamingProcessor` contract correctly says process-after-
  terminal-finish returns `AlreadyFinished`
  (`streaming-lifecycle.md:134-144`). The facade deliberately has two paths:
  callback-side drain enters `Idle`, writes silence, and returns success
  (`pipeline.rs:1301-1326`), while explicit `finish_into_with_policy` retains
  `AlreadyFinished` (`:1429-1449`). This is not a violation of the scoped
  `StreamingProcessor` rule, but the exception has no Trellis contract of its
  own.

Consequence: the highest-level recommended callback API is governed only by
inline rustdoc and changelog text. A maintainer applying the generic terminal
rule or the old file-wide logging classification can regress the device-
callback behavior while believing they are following Trellis. The facade
needs an owned scenario covering thread authority, command coalescing,
prepared capacity, idle silence, explicit-finish behavior, ranges, and its
realtime prohibitions.

### P2 - public scope wording contradicts the newly recommended API

**Category**: inaccurate ownership boundary / naming drift.

- `README.md:111-190` calls `PlaybackPipeline` the recommended canonical
  callback DSP path and describes `PlaybackController` as its high-level
  control authority.
- The same README says the crate leaves "playback" to the application at
  `:7-11` and says it may not fit callers needing "a high-level playback API"
  at `:328-329`.
- Crate rustdoc likewise says the application/server layers playback control
  on top (`src/lib.rs:3-5`) immediately before documenting the callback
  playback facade (`:26-62`).

The intended boundary is recoverable from the detailed text: the crate owns
DSP playback control and callback lifecycle, but not decoding orchestration,
queues, device negotiation/output, or a complete player. The unqualified term
"playback" now names both sides of that boundary. This makes the advertised
scope self-contradictory and obscures where future lifecycle behavior belongs.

### P2 - the decoder/error spec records resolved work as an active gap

**Category**: stale contract / duplicated planning state.

- `error-handling.md:37-41` says `DecoderError::UnsupportedFormat` is never
  constructed and assigns the work to an older task.
- Current `map_probe_error` constructs it for Symphonia `Unsupported` and
  short/garbage `UnexpectedEof` cases (`src/decoder/streaming.rs:594-611`).
  Focused tests assert garbage and empty input map to the typed variant
  (`src/decoder/tests.rs:269-290`), and `README.md:340-342` documents the
  behavior correctly.
- The process-error section of `error-handling.md` also omits the newer
  `InvalidParameter` and `UnsupportedOperation` variants now defined at
  `src/processor/traits.rs:605-630` and used by the facade.

Consequence: a maintainer may duplicate completed decoder work, preserve a
generic error path unnecessarily, or miss the intended control-boundary error
categories. Resolved task state should not remain embedded as a current
"known gap" in an executable spec.

### P2 - `docs/quality.md` presents the retired nonlinear resampler route as current

**Category**: algorithm and benchmark-identity drift.

- The current implementation routes reduced nonlinear interpolation factors
  `up <= 16` through the spectral engine and larger factors such as 147:160
  through the contiguous polyphase engine
  (`src/processor/resampler/rubato_backend.rs:28-41`, `:84-101`, and
  `:776-788`). The current matrix identity is
  `matrix_process_checked_v4_nonlinear_polyphase_up16`
  (`benches/audio_resampler_matrix_perf.rs:38-39`).
- README's feature section (`README.md:288-298`) and the newer Trellis
  resampler scenarios describe that hybrid route accurately.
- `docs/quality.md:423-443` instead says Minimum/Maximum always use the exact
  spectral engine and cites the retired
  `matrix_process_checked_v2_spectral_nonlinear` result, including
  44.1-to-48 kHz as a spectral case. It is written in present tense after the
  same document's newer v17 tables.

Consequence: readers receive the wrong complexity/cost model for the dominant
147:160 conversion and can select or compare an incompatible baseline ID. The
long rolling evidence page needs an explicit current-contract section and
dated historical sections, rather than leaving superseded architecture in
present tense.

### P2 - backend-neutral examples are described as feature-free SoXR examples

**Category**: example/feature contract drift.

- `README.md:80-83`, both example module comments, and
  `directory-structure.md:68-71` say the examples need no optional features.
  The crate cannot compile with no backend feature, even when the example uses
  only EQ.
- `README.md:82-83` and `examples/resample_sine.rs:3-6` say the resampling
  example runs through SoX VHQ. The code constructs backend-neutral
  `StreamingResampler` (`resample_sine.rs:39`); in a Rubato-only build it runs
  the pure-Rust backend instead.
- `cargo check --no-default-features --features rubato --examples` passed,
  proving the example itself is backend-neutral. The problem is the prose, not
  the implementation.

Consequence: "no optional features" suggests that bare
`--no-default-features` is supported, while the hard-coded SoXR description
hides a valid Rubato-only workflow. The accurate promise is "no input files
and no extra flags with the default backend; either supported backend works."

### P2 - contributor CI documentation misstates both count and platform coverage

**Category**: release-process drift.

- `CONTRIBUTING.md:66-72` lists five quick evidence commands but `:84-86`
  calls them "all four".
- Default-feature CI currently runs nine report commands at
  `.github/workflows/ci.yml:181-241`; the pure-Rust job runs three additional
  Rubato reports at `:129-150`. Neither job supplies a baseline.
- `CONTRIBUTING.md:98-100` says CI runs all preceding checks on Linux, macOS,
  and Windows. Only the build/test job is a three-OS matrix (`ci.yml:53-107`).
  Format, both Clippy matrices, and docs run on Ubuntu (`:18-51`), as do the
  pure-Rust, benchmark-report, and package jobs (`:112-268`).

Consequence: contributors cannot tell which gates have cross-platform proof,
which probes CI actually runs, or which baseline branches remain manual-only.
This also masks the probe-private baseline test gap recorded in area 05.

### P3 - the directory spec no longer describes the live ownership structure

**Category**: maintenance locator drift.

- `directory-structure.md` explicitly claims to reflect the live tree
  (`:1-4`) but lists the nonexistent single file
  `processor/resampler.rs` (`:43`) instead of the seven-file
  `processor/resampler/` module.
- It omits `fir_design.rs`, `output_chain.rs`, their split test modules, and
  several other current test ownership boundaries. It also labels
  `loudness/limiter.rs` as sample-peak at `:62`, although the public limiter
  defaults to true-peak detection.
- Most importantly, `pipeline.rs` is still labeled only `RingBuffer`, even
  though the legacy ring is now effectively test/benchmark-only and the file's
  main responsibility is the public playback facade (area 04 records the
  production-use evidence).

Consequence: this spec sends new work to the wrong file shape and hides the
largest current ownership boundary. This is a navigation/maintenance defect,
not an argument that every test file must be exhaustively listed.

### P3 - lock-free timing prose has two unversioned current values

**Category**: evidence traceability drift.

- README and `docs/quality.md:220-227` say the full cached parameter set costs
  about 7 ns; `realtime-safety.md:101-109` says the measured hazard read is
  about 13 ns.
- Neither prose location carries the exact report, revision, date, iteration
  mode, or host identity. Area 05 established that the underlying lock-free
  probe cannot emit versioned JSON and uses one wall-clock sample.

The numbers may come from different revisions or host conditions, so this is
not evidence of a performance regression. It is evidence that an
unversioned exploratory microbenchmark cannot safely support a single current
number copied into multiple living documents.

## Strong documentation signals and justified detail

- `src/lib.rs:98-102` includes README Rust blocks as doctests. This is a useful
  anti-drift mechanism: all six crate/README doctests passed under both backend
  matrices in this review.
- The current `CHANGELOG.md:14-104` accurately records the playback facade,
  callback-idle exception, lifecycle command channel, range constants, and new
  typed errors. It supplied a reliable map for finding the spec gaps; its
  detail is appropriate for a pre-1.0 public API change.
- `docs/installation.md` accurately distinguishes default SoXR setup from the
  Rubato-only no-native build. `docs/resampler-comparison.md` preserves lane,
  recipe, provenance, unavailable-engine, and complete-matrix limitations
  rather than claiming a universal winner.
- The detailed streaming, convolver, and resampler Trellis scenarios encode
  realtime, numerical, lifecycle, and evidence constraints that are difficult
  to reconstruct from code alone. Their size is justified. The defect is that
  older sections were not retired or updated when ownership changed, not that
  the project has too much specification.
- The generic `StreamingProcessor` terminal contract remains correct for its
  stated scope. The facade's idle-silence behavior is an intentional wrapper
  exception, not proof that the lower-level rule is wrong.

## Validation performed for this area

| Command | Exact outcome |
|---|---|
| `cargo check --no-default-features` | exited 1; intended missing-backend `compile_error!` plus cfg-cascade compiler errors |
| `cargo check --no-default-features --features rubato --examples` | exited 0 |
| `cargo test --doc --all-features` | 6 passed |
| `cargo test --doc --no-default-features --features rubato` | 6 passed |

No benchmark was run, so this area makes no new timing claim. The 7 ns versus
13 ns observation is only a cross-document consistency finding.

## Handoff to final synthesis

- Treat the unsupported release matrix and overclaimed benchmark enforcement
  as executable-contract defects, not copy-editing nits.
- Keep the playback lifecycle exception and backend-neutral example behavior
  as intentional design; the finding is that their ownership is undocumented
  or ambiguously named.
- Re-read every concurrently moving source/document file before ranking final
  findings. If a newer edit fixes a statement, mark this snapshot superseded
  rather than silently deleting its evidence.
