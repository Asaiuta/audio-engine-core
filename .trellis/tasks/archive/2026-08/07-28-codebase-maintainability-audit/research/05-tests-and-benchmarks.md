# Tests and benchmark maintainability

## Snapshot and scope

- Local audit window ended at 2026-07-28 18:55:13 +08:00.
- HEAD remained `0c62febd2b6afdd1800da1591b68f7a600a3835e`.
- The working tree remained concurrently dirty in `CHANGELOG.md`, `README.md`,
  `src/lib.rs`, `src/pipeline.rs`, `src/processor/lockfree_params.rs`,
  `src/processor/mod.rs`, and `src/processor/traits.rs`. None of those files
  was modified by this audit.
- The benchmark/test files central to this area did not move during the audit:
  `audio_gapless_comparison_perf.rs` (2026-07-19 15:08:49),
  `audio_lockfree_params_perf.rs` (2026-07-19 00:16:18), shared
  `benches/support/mod.rs` (2026-07-26 15:21:02),
  `tests/benchmark_support.rs` (2026-07-26 15:26:18), and CI
  (2026-07-25 23:55:28).
- Inventory: 14 custom-harness benchmark entry points, 8 Rust support/adaptor
  files under `benches/`, 3 integration-test entry points, and the unit tests
  embedded in or split from production modules. The benchmark tree is about
  20,300 logical lines, but size alone was not treated as a finding.

## Verdict

The test and benchmark code is not generally a pile of ad-hoc scripts. It has
a strong shared core for argument parsing, environment identity, case-key
validation, trial distributions, baseline compatibility, JSON artifacts,
deterministic fixtures, callback scenarios, and allocation accounting. CI
exercises three operating systems, both backend feature paths, and nine
default-feature quick report gates. Large DSP test suites mostly have focused,
descriptive cases and several have already been split into owned `tests.rs`
modules.

There is nevertheless one confirmed false-green evidence defect: the gapless
`--enforce` path can discard a failed fixture as "skipped" and still pass when
another fixture validates. The lock-free parameter benchmark is also a clear
legacy island outside the shared evidence model. Finally, most probe-specific
baseline/enforcement code has no automated synthetic test; routine CI only
executes the no-baseline branch.

## Confirmed findings

### P1 - gapless `--enforce` can pass after a fixture correctness failure

**Category**: benchmark correctness / false-green evidence.

- `benches/audio_gapless_comparison_perf.rs:216-225` calls
  `validate_fixture`; an `Err` is appended to `skipped` and the loop continues.
  This is not an optional-corpus absence: the message explicitly says
  `correctness probe failed`.
- The failed fixture is therefore absent from `report.validations` and
  `report.cases` (`:293-303`).
- `enforce_report` (`:979-996`) fails only when *no* validation exists or when
  an included validation has non-`pass` status. If fixture A passes and fixture
  B returns an error, A makes `validations` non-empty, B exists only in
  `skipped`, and `--enforce` returns success.
- The same file correctly treats an entirely empty fixture set as an enforce
  failure, so the defect is specifically the partial-failure path.

This undermines the command advertised at `docs/quality.md:40`: a mixed corpus
can produce a green exit code while omitting the failed input. Preserve a
structured failed validation row, or retain a separate `run_failures` list and
make enforcement reject it. Missing optional fixture classes may remain
explicit skips; an attempted fixture that fails correctness must not.

### P2 - the gapless probe silently accepts unsupported baseline options

**Category**: inaccurate CLI contract / evidence trap.

- The probe uses shared `PerfArgs::parse` at
  `audio_gapless_comparison_perf.rs:158-160`. Shared parsing accepts
  `--baseline` and `--max-median-regression-pct`
  (`benches/support/mod.rs:281-352`).
- The gapless file never reads `args.baseline` or
  `args.max_median_regression_pct`; it has no baseline comparison function or
  baseline reference in the report.
- Its help text (`audio_gapless_comparison_perf.rs:369-375`) does not advertise
  either option, but an explicitly supplied option is accepted rather than
  rejected. A caller can therefore believe a regression threshold was applied
  when it was ignored.

The shared parser needs capability-aware options, or this probe needs a
smaller parser that rejects unsupported flags.

### P2 - `audio_lockfree_params_perf` is outside the benchmark evidence system

**Category**: legacy benchmark surface + unreliable gate.

- It manually scans raw argv for only `--quick` and `--enforce`
  (`benches/audio_lockfree_params_perf.rs:16-20`), silently ignoring every
  unknown flag. It cannot emit JSON and records no revision, dirty state,
  compiler, target, CPU, profile, feature set, case key, or schema version.
- Every comparison is a single wall-clock measurement (`:542-569`,
  `:643-676`), not the shared multi-trial distribution. The sole gate is a
  fixed `assert!(improvement_percent >= 3.0)` at `:45-51`, which is vulnerable
  to scheduling noise and reports no compatible baseline identity.
- The benchmark manually mirrors seven parameter groups and hardcodes the
  loudness-band count (`:14`, `:60-108`). A newly added parameter can be
  omitted from both the current and legacy sums without a compile failure, so
  the benchmark's claimed scope can silently shrink.
- `docs/quality.md:19` presents it beside standardized probes, but
  `.github/workflows/ci.yml` never runs it and the standardized JSON command
  list deliberately omits it.

This probe should either migrate onto the shared harness with multiple trials,
work validation, a named case set, and a machine-compatible baseline, or be
clearly classified as an exploratory microbenchmark with no gate authority.

### P2 - most probe-specific baseline and enforcement branches are compile-only

**Category**: test gap / failure-localization risk.

- All 14 top-level `benches/*.rs` entry files contain zero `#[test]` cases.
- `tests/benchmark_support.rs` directly tests the real shared support module,
  but not each report's mapping into `compare_case_medians` or its
  probe-specific `enforce_report` function.
- Ten ordinary probes implement baseline-capable report flows across callback,
  tail, component, convolver, decoder, FIR EQ, lifecycle, output render,
  resampler matrix, and streaming resampler. The cross-project resampler probe
  is the positive exception: its support module is imported by an integration
  test and includes synthetic baseline/incomplete-coverage tests.
- CI invokes no benchmark with `--baseline`; its quick gates exercise work and
  report integrity, but not report deserialization, per-probe case mapping, or
  regression failure messages.

The shared primitives are well tested, so this is not a claim that all gates
are broken. It is a change-risk problem: a field or case-key edit in a probe's
private report logic can compile and pass routine CI yet fail only during a
manual same-machine comparison. Move the small pure comparison/enforcement
parts into importable modules and test synthetic pass, threshold-boundary,
missing-case, and incompatible-report inputs.

### P3 - repeated report metadata and uneven artifact validation invite drift

**Category**: duplicated benchmark infrastructure.

- Nine probes repeat the same five-field `BaselineReference` struct; callback
  tail repeats it with two additional percentile thresholds. The copies begin
  at callback `:64`, component `:68`, convolver `:170`, decoder `:100`, FIR EQ
  `:164`, lifecycle `:136`, output render `:251`, resampler matrix `:435`, and
  streaming resampler `:148`.
- The same files repeat report header fields and baseline-reference assembly.
  Shared helpers own compatibility and median comparison, but not the repeated
  metadata model.
- Newer decoder/component/lifecycle probes use
  `write_json_round_trip`; the cross-project comparison has its own written
  report validation. Callback, tail, convolver, FIR EQ, gapless, output render,
  quality, matrix, and streaming probes use only `write_json`, so artifact
  round-trip guarantees depend on which probe produced the file.
- A single global `REPORT_SCHEMA_VERSION` (`benches/support/mod.rs:18`) versions
  several structurally unrelated report types. A schema change either bumps
  unrelated probes or relies on maintainers remembering that one probe's shape
  changed.

The exact common baseline metadata should be shared. Per-probe report bodies
should remain separate; a generic mega-report abstraction would add more
complexity than it removes. Prefer per-probe schema constants or an explicitly
documented global-version policy, and use one artifact-validation policy.

### P3 - a canonical callback fixture is copied into output-render code

**Category**: low-severity fixture duplication.

`benches/support/callback_fixture.rs:274-311` and
`benches/audio_output_render_perf.rs:907-939` independently implement the same
220/330 Hz modulated stereo program and the same impulse/early-reflection/tail
IR formula. The callback helper clamps samples while the output-render copy
does not, although current amplitudes keep that difference inactive. If the
corpus is meant to make callback and render results comparable, one shared
parameterized fixture should own it. Other signals in quality, FIR, lifecycle,
and resampler probes have distinct measurement purposes and should not be
collapsed merely because they use sine waves.

### P3 - a few test helpers and ownership boundaries remain duplicated

**Category**: test maintenance smell.

- `src/processor/resampler/mod.rs:1774-1805` and `:1859-1890` duplicate local
  `sine` and `fitted_tone` implementations in adjacent quality tests.
- `src/pipeline.rs` keeps legacy `RingBuffer` tests and the newer playback
  facade/lifecycle tests in one inline module starting at `:1614`. This mirrors
  the production ownership problem already recorded in area 04: unrelated
  legacy and facade changes touch one large file and one test namespace.

These are cleanup candidates, not evidence of incorrect behavior. Keep
independent numerical oracles separate when sharing would make the test repeat
the implementation under test.

## Strong quality signals and justified complexity

- `benches/support/mod.rs` centralizes argument validation, feature/backend
  identity, dirty-state capture, baseline compatibility, finite positive trial
  samples, percentile definitions, case-set validation, regression diagnostics,
  JSON I/O, and optional round-trip checking. The focused integration test
  covers these contracts directly.
- Callback chain and callback tail are separate by design: aggregated
  throughput and retained per-callback p99/p99.9 evidence answer different
  questions. Raw tail samples, explicit timer limitations, pinned scheduling
  state, and baseline incompatibility are appropriate realtime rigor, not
  over-design.
- The 3,000-line cross-project adapter file is large because it independently
  drives upstream Rust engines and explicit native ABIs, validates progress,
  reset, exact duration, native library provenance, hashes, and quality. That
  independence is necessary for a comparison control. Reusing production
  adapter internals would weaken the oracle.
- The resampler comparison's tiny entry point plus importable support module is
  a good testability boundary. Its all-features and Rubato-only tests both
  preserve missing native engines as visible non-terminal coverage.
- Optional EBU corpora and unavailable native shims are explicitly classified,
  not silently treated as measured. The ignored native-shim ABI test names the
  prerequisite in its ignore reason.
- Large DSP unit-test files generally use descriptive case names, bounded
  lifecycle loops, allocation assertions on isolated threads, independent
  legacy/numerical oracles, and feature-specific matrices. File length by
  itself is not a reason to merge unrelated helpers or weaken those oracles.
- Custom `harness = false` binaries are justified here: Criterion alone would
  not provide the repository's versioned reports, raw callback distributions,
  exact-work validation, optional native discovery, or compatible-baseline
  semantics.

## Validation performed for this area

All commands reached explicit successful exit status:

| Command | Result |
|---|---|
| `cargo test --all-features --test benchmark_support` | 20 passed |
| `cargo test --all-features --test resampler_comparison_support` | 25 passed, 1 explicitly ignored native-shim test |
| `cargo test --no-default-features --features rubato --test resampler_comparison_support` | 25 passed, 1 explicitly ignored native-shim test; includes the Rubato-vs-production bit-exact complete-stream check |

No performance benchmark was executed in this area, so this document makes no
new timing or regression claim. The full-suite results in area 00 predate the
latest concurrent playback-facade edits and must be refreshed before final
synthesis if those moving files remain in scope.

## Handoff to documentation/spec review

- Check `docs/quality.md:46-50` against the gapless false-green behavior and
  against the lock-free benchmark's nonstandard `--enforce` semantics.
- Check whether the CI-excluded probe list and manual-only native requirements
  are stated completely and consistently.
- Check examples and public docs against backend-neutral APIs rather than the
  currently selected SoXR default.
