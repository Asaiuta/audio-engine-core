# Quality Guidelines

> Code-quality and evidence standards for this crate. The evidence policy below
> is derived from the algorithm audit
> (`.trellis/tasks/06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`).

---

## Evidence Policy (the core rule)

This crate makes audio-quality and performance claims. Every such claim must be
backed by one of:

1. a passing unit/integration test,
2. current benchmark output from `benches/`, or
3. an explicit, honest limitation note.

Concretely:

- **Do not strengthen README / doc claims without current measured evidence.**
  Regenerate the number, or label it as machine/config-specific, or keep the
  limitation note. The limiter now defaults to 4x-oversampled true-peak
  detection (measured: an intersample-stress input at +0.10 dBTP is pulled to
  -1.00 dBTP); the remaining visible limitation is that the limiter runs at
  source rate, so the full output-chain true-peak probe stays report-only
  because downstream resampling/quantization can re-introduce intersample
  peaks.
- **No marketing absolutes.** "Industry-leading", "all algorithms are optimal",
  etc. are forbidden unless a measurement backs the specific claim. The audit
  classifies IIR/FIR EQ, crossfeed, saturation, and FFT convolution as classic,
  useful DSP — not automatically best-in-class.
- **Missing external corpora are skipped, not silently passed.** The EBU Tech
  3341/3342 corpus check is skipped when reference vectors are absent rather
  than reported as a pass. A report-only benchmark is not a conformance gate;
  do not present it as one.

## Forbidden Patterns

- Allocation / locks / logging / I/O / panics on the hot path — see
  `realtime-safety.md`. This is the highest-priority quality rule.
- `unwrap()` / `expect()` / `panic!` in DSP/callback code.
- Resizing channel/sample buffers during processing instead of presizing during
  setup.
- Per-sample coefficient recomputation when it can be done once on a parameter
  change.

## Required Patterns

- New tunable parameters go through the lock-free atomic snapshots in
  `lockfree_params.rs`.
- Stateful DSP adapters must distinguish scalar/control updates from
  coefficient-geometry updates. Mix/strength/enabled changes should preserve
  filter history unless the processor contract explicitly requires a reset;
  sample-rate, cutoff, latency-window, or topology changes may reset state when
  documented and tested. Crossfade branch ownership, config publication,
  coefficient-oracle, and sample-rate state boundaries additionally follow
  `dsp-state-correctness.md`.
- Streaming processor implementations and drivers must follow
  `streaming-lifecycle.md`: validated zero-copy blocks, complete 1:1 in-place
  progress, explicit backpressure, idempotent finish, native-state reset, and
  rate-tagged latency/tail timing. Use `process_checked` / `finish_checked` at
  chain/direct-driver boundaries.
- New DSP processors ship with: unit tests (mono + stereo at minimum), a
  no-steady-state-allocation test (`assert_no_alloc`) for the processing path,
  and a benchmark entry if they touch the callback budget.
- Public names must not conflate distinct guarantees (e.g. "sample peak" vs
  "true peak").

## Saturation Quality Modes

`SaturationQuality::Direct` is the legacy source-rate waveshaper. Higher-quality
antialiasing modes are explicit public modes, currently
`SaturationQuality::Oversampled2x` and `SaturationQuality::Oversampled4x`.

Contracts to preserve when changing or extending this path:

- Public control must flow through both `Saturation::set_quality(...)` and
  `AtomicSaturationParams::set_quality(SaturationQualityValue::...)` so direct
  processor use and callback-chain use expose the same modes.
- Per-channel oversampling/FIR history must be sized by
  `Saturation::set_channel_count(...)` during setup. Do not resize, allocate,
  log, or lock inside `process_with_channels`, `process_fullband_oversampled`,
  or high-pass processing.
- Quality-mode claims need objective evidence in
  `audio_quality_measurements.rs`. The current gate is
  `saturation_oversampled4x_alias_reduction`, which compares folded harmonic
  alias energy from the Direct and Oversampled4x Tube paths at equivalent
  drive/mix settings.
- Callback-budget changes need `audio_callback_chain_perf`; the active DSP
  scenario intentionally enables `SaturationQualityValue::Oversampled4x` so the
  measured 512-frame callback cost includes the upgraded path.

Tests required for this contract:

- Unit tests cover all saturation types across Direct/Oversampled2x/Oversampled4x.
- High-pass mode, multichannel setup, reset, and sample-rate changes remain
  finite and bounded.
- A no-steady-state-allocation assertion covers the oversampled processing path
  after setup.

## FFT Convolution Routing

`FFTConvolver::new(...)` owns both convolution strategies: the overlap-save
engine for short/medium FIRs and the uniform partitioned engine for long impulse
responses. Keep the public constructor as the routing point so callers,
adapters, FIR EQ, and benches do not need to duplicate IR-length logic.

Contracts to preserve when changing this path:

- `PARTITIONED_CONVOLUTION_IR_THRESHOLD` and
  `PARTITIONED_CONVOLUTION_PARTITION_SIZE` are public evidence-backed routing
  constants. Changing either requires fresh `audio_convolver_perf --quick` and
  `audio_fir_eq_perf --quick` output plus README/doc updates for any cited
  values.
- FIR EQ tap counts must stay on `ConvolutionStrategy::OverlapSave` unless a
  benchmark proves the partitioned path does not regress the FIR apply budget.
- Partitioned processing must precompute IR spectra, history buffers, FFT plans,
  and in-place scratch buffers during construction. `process_into` and
  `process_inplace` must not allocate, lock, log, perform I/O, or do unbounded
  work after setup.
- Correctness tests must compare the partitioned output against the overlap-save
  reference within a fixed tolerance for stereo and at least one mono/surround
  channel count, and cover cross-buffer continuity, reset, and in-place paths.

## Testing Requirements

- `cargo test --lib` must pass. The crate already carries ~150 unit tests; new
  behavior must add tests, not rely on existing ones.
- Cover continuity across buffers, reset behavior, silence, and edge inputs
  (non-finite samples, sample-rate changes) where the processor is stateful.
- Offline finalize changes must cover last-frame impulse survival,
  raw-vs-compensated content equivalence, finite tail propagation through every
  downstream rate domain, and timing metadata. Unknown-tail tests must prove
  both block-size-independent retained output and early energy termination; a
  test that generates the full safety maximum and only checks post-trim samples
  is insufficient performance evidence.
- When fixing adapter-level parameter continuity, compare the adapter against a
  direct processor reference that preserves history, and include a reset
  reference when possible so the test proves it would catch the old click/glitch
  behavior.
- Run `cargo clippy --all-targets -- -D warnings` clean.

## Benchmark Gate Convention

The quality benches (`benches/`, custom-main `harness = false`) follow a
report-vs-gate contract established by `audio_quality_measurements.rs`. New or
extended benches must keep it:

- **Classify every metric** as `gate` (fails the run), `report` (evidence only,
  never fails), or `skipped` (a gate whose reference inputs are absent — e.g.
  the EBU corpus). A missing corpus is reported as `skipped` with the
  missing-file count, **never a silent pass**.
- **`--enforce`** turns gate failures into a non-zero exit; the diagnostic must
  name the metric and print measured-vs-threshold. Without `--enforce` the bench
  only reports.
- **`--out <path>`** emits machine-readable JSON (classified metric table +
  conditions) so README/doc values are traceable to a specific run.
- **Conservative thresholds.** Gate margins must survive debug/release and
  cross-CPU/compiler variance. Tight gates are only for deterministic
  bit-parity metrics (e.g. `LoudnessMeter` vs `ebur128` at `1e-6 LU`);
  float/FFT/timing-dependent metrics get wide margins or stay `report`. Record
  the observed value and margin rationale in the task's benchmark inventory.
- **No network**, and `--quick` must stay fast for local dev.

## Scenario: Versioned Benchmark Evidence And Compatible Baselines

### 1. Scope / Trigger

- Trigger: changing `audio_quality_measurements`,
  `audio_callback_chain_perf`, `audio_resampler_streaming_perf`, shared
  `audio_fir_eq_perf`, `benches/support/` code, benchmark CI wiring, or a
  documented timing claim.
- These are custom-main benches (`harness = false`). Benchmark plumbing stays
  bench-local; do not expose report helpers as crate public API.

### 2. Signatures

```bash
cargo bench --bench audio_quality_measurements -- \
  --quick --enforce --out <quality.json>

cargo bench --bench audio_callback_chain_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]

cargo bench --bench audio_resampler_streaming_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]

cargo bench --bench audio_fir_eq_perf -- \
  [--quick|--heavy] [--enforce] [--out <candidate.json>] \
  [--baseline <baseline.json>] \
  [--max-median-regression-pct <non-negative-finite-pct>]
```

Omitting `--quick` / `--heavy` selects full mode. Quality supports quick/full;
the three performance probes additionally support heavy. Environment overrides
are `AUDIO_BENCH_REVISION`, `AUDIO_BENCH_DIRTY`, `AUDIO_BENCH_RUSTC`,
`AUDIO_BENCH_RUSTC_VERBOSE`, `AUDIO_BENCH_TARGET`, `AUDIO_BENCH_CPU`, and
`AUDIO_BENCH_PROFILE`; `GITHUB_SHA` is a revision fallback.

### 3. Contracts

- Every report has `schema_version`, stable `probe`, `generated_unix_ms`,
  `mode`, `environment`, and explicit measurement `conditions`.
- Environment contains revision, nullable dirty state, rustc, target, OS,
  architecture, CPU, Cargo profile, and compiled feature names. Failed probes
  produce `"unknown"` / `null`; they do not abort a report without a baseline.
- Performance cases have unique stable `case_key` values, declared iterations
  and trials, raw trial samples, and min/median/nearest-rank-p95/max. Callback
  utilization uses the device-buffer deadline. Resampler utilization is only a
  source-buffer realtime reference and must be named as such. FIR regeneration
  compares ns/regeneration while FIR apply compares ns/sample; the case key and
  payload must state that primary unit explicitly.
- Quality keeps `gate` / `report` / `skipped` distinct. Full-output points copy
  `RenderedOutput` rendered frames, algorithmic latency, semantic tail, and
  truncation fields directly. Missing external corpus counts remain visible.
- A requested baseline must match schema, probe, mode, conditions, complete
  case set, rustc, target, OS/architecture, CPU, profile, and features. Unknown
  required environment fields are not comparable. Revision and dirty state may
  differ and are retained for traceability.
- The default median regression limit is 10%: exactly +10% passes and any
  greater regression fails under `--enforce`. Without `--baseline`, absolute
  timing is report-only; `--enforce` still validates work and report integrity.
- Shared CI runners run all four quick reports and upload JSON, but never use
  a cross-run absolute nanosecond gate without an explicitly supplied
  compatible baseline.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Unknown CLI option, missing path, negative/non-finite threshold | named argument error |
| Empty, non-finite, or non-positive trial sample | report construction error |
| Duplicate/missing case key | baseline comparison rejected with both case sets named |
| Corrupt JSON | deserialization error naming the file and report type |
| Schema/probe/mode/conditions mismatch | comparison rejected before percentages are computed |
| Required environment mismatch or `unknown` | comparison rejected with each incompatible field |
| Candidate median exactly 10% slower | comparison passes |
| Candidate median more than 10% slower | enforced failure names case, baseline, candidate, regression, and threshold |
| No baseline on a shared runner | timing remains report-only; work/report gates still run |
| EBU vectors absent | `skipped` with missing-file count, never pass/conformance |

### 5. Good / Base / Bad Cases

- Good: compare two reports from the same compiler, target, CPU, profile,
  features, mode, conditions, and case set; allow revisions to differ.
- Base: generate a quick CI artifact with `--enforce --out` and no baseline;
  deterministic quality/work checks are enforced while timing is evidence.
- Bad: compare two `cpu = "unknown"` reports, compare debug with release, or
  call a source-buffer resampler percentage a device callback utilization.
- Bad: cite min/best-of-N as representative performance or turn a missing EBU
  corpus into a successful conformance claim.

### 6. Tests Required

- Shared support tests assert odd/even median, nearest-rank p95, raw sample
  retention, invalid samples, CLI modes/paths/thresholds, JSON round trip, and
  environment compatibility including unknown-field rejection.
- Regression tests assert exactly +10% passes, greater than +10% fails, and the
  diagnostic contains case, baseline, candidate, measured regression, and
  threshold.
- Each performance quick run asserts unique case keys, trial-vector lengths,
  finite timing, and complete work. Callback/resampler additionally assert
  consumed/produced work and output bounds; FIR asserts IR length/finite
  samples, finite changed apply output, and overlap-save routing.
- Quality quick `--enforce --out` must deserialize and expose environment,
  skipped count, rendered frames, latency, semantic tail, and truncation.
- Before release, run callback/resampler/FIR quick/full/heavy, quality
  quick/full, both feature test matrices, both strict Clippy matrices, rustfmt,
  docs, and package verification.

### 7. Wrong vs Correct

#### Wrong

```text
Candidate is 8 ns/sample, therefore this is the fastest implementation.
GitHub timing regressed, so fail against last run regardless of runner CPU.
```

#### Correct

```text
On the recorded compiler/target/CPU/profile/features, the seven-trial quick
median was 8 ns/input-sample; see the JSON for p95 and raw samples. Enforce a
timing regression only against an explicitly compatible same-environment
baseline; shared-runner absolute timing remains report-only.
```

## Code Review Checklist

- [ ] Hot path: no alloc/lock/log/IO/panic/unbounded work.
- [ ] Claims in docs/README backed by a test, current bench output, or a
      limitation note.
- [ ] Feature-gated code builds under `--no-default-features` and each feature
      toggled individually.
- [ ] New tunables use the lock-free snapshot mechanism.
- [ ] Tests cover continuity/reset/edge cases, plus a no-alloc assertion.

## Release Documentation Checklist

Use this when changing `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`,
`NOTICE`, Cargo features, or public crate metadata.

### 1. Scope / Trigger

- Trigger: release-facing documentation, public API wording, package metadata,
  feature flags, examples, or native dependency claims.
- Target files: `Cargo.toml`, `src/lib.rs`, `README.md`, `CHANGELOG.md`,
  `CONTRIBUTING.md`, `NOTICE`, and `examples/`.

### 2. Signatures

- Feature checks:
  - `cargo build`
  - `cargo build --no-default-features`
  - `cargo build --no-default-features --features http`
  - `cargo build --no-default-features --features loudness-db`
- Docs/package checks:
  - `cargo doc --no-deps`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`
  - `cargo package --allow-dirty`

### 3. Contracts

- `http` controls the optional `reqwest` dependency and network error surface.
- `loudness-db` controls the optional `rusqlite` dependency and SQLite cache
  types.
- SoXR is a required native dependency today because `src/processor/resampler.rs`
  is part of the core crate and `soxr` is not optional in `Cargo.toml`.
  Therefore `default-features = false` removes HTTP and SQLite, but it does not
  remove libsoxr or the resampler API.
- README quality/performance numbers must name the benchmark or test family that
  produced them and must preserve explicit limitation notes for report-only
  probes.

### 4. Validation & Error Matrix

- A feature combination fails to compile -> fix the `#[cfg(feature = "...")]`
  boundary or update the feature contract before release.
- `cargo doc` emits a public warning -> treat it as a release blocker, even if
  the command exits successfully.
- `cargo package` fails while verifying the package -> fix package contents or
  metadata.
- `cargo package` fails only while updating the registry/index in a sandboxed
  environment -> rerun in a normal network/credential environment before
  classifying it as a package-content failure.

### 5. Good/Base/Bad Cases

- Good: "`default-features = false` removes HTTP and SQLite dependencies; SoXR
  remains required because resampling is core."
- Base: "Both Cargo features are default-on and can be disabled independently."
- Bad: "`default-features = false` creates a dependency-free DSP-only build" or
  "building without resampling avoids libsoxr" while `soxr` is still required.

### 6. Tests Required

- Build default, no-default, and each optional feature independently.
- Run all-features tests and no-default-features tests before publishing.
- Run examples listed in the README.
- Run `cargo package --allow-dirty` or `cargo publish --dry-run`.

### 7. Wrong vs Correct

#### Wrong

```text
If you build without the resampling functionality, libsoxr is not linked.
```

#### Correct

```text
SoXR-backed resampling is part of the core crate today. No Cargo feature
currently disables the `soxr` dependency, so building the crate links libsoxr
even when default features are disabled.
```

## Scenario: Windows MSVC Runtime Deployment for MSYS2 SoXR

### 1. Scope / Trigger

- Trigger: changing `build.rs`, `build/windows_runtime.rs`, Windows native
  dependency setup, `PKG_CONFIG_PATH` handling, or any test/example/benchmark
  that must start with the MSYS2 SoXR DLL under the MSVC Rust target.
- This scenario does not apply when `vcpkg::find_package("soxr")` selects a
  static vcpkg build or on non-Windows targets.

### 2. Signatures

```text
PKG_CONFIG_PATH=<msys2-prefix>/mingw64/lib/pkgconfig
VCPKG_ROOT=<vcpkg-root>  # alternative provider

soxr_dll_candidates_from_pkg_config_dir(pkg_config_dir: &Path) -> Vec<PathBuf>
deploy_runtime_dlls(soxr_dll: &Path, out_dir: &Path) -> Result<(), String>

cargo test
cargo run --example <name>
cargo bench --bench <name> -- <bench arguments>
```

### 3. Contracts

- A pkg-config directory at `<prefix>/lib/pkgconfig` maps to
  `<prefix>/bin/libsoxr.dll` (or `soxr.dll`). `<prefix>/lib/bin` is wrong.
- Explicit pkg-config and the known Scoop/MSYS2 installation are searched
  before generic `PATH`, so a stale DLL already copied under `target/` cannot
  become the source for the next deployment.
- The MSYS2 source directory supplies one ABI-matched set: `libsoxr.dll`,
  `libgomp-1.dll`, `libgcc_s_seh-1.dll`, and `libwinpthread-1.dll`. Never mix a
  `libgomp` or GCC runtime from another MinGW installation merely because it is
  earlier on `PATH`.
- Runtime DLLs are content-checked and deployed beside every ordinary Cargo
  executable location for the active profile: profile root, `deps`, and
  `examples`. This covers binaries, tests/doctests, custom-harness benches, and
  examples without command-specific PATH wrappers.
- Unrelated DLLs from the MSYS2 `bin` directory are not copied. Runtime sources
  and `build/windows_runtime.rs` are Cargo rerun inputs, so an updated installed
  runtime or deployment rule refreshes stale destinations.
- Deployment is build-time filesystem work only; it never enters an audio or
  DSP path.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| `PKG_CONFIG_PATH=<prefix>/lib/pkgconfig` | Probe `<prefix>/bin`, not `<prefix>/lib/bin` |
| Cargo target root already has an isolated `libsoxr.dll` | Prefer the configured MSYS2 source; do not self-copy the isolated target DLL |
| Destination DLL bytes equal source | Leave the file unchanged |
| Destination DLL is absent or stale | Copy/refresh it in root, `deps`, and `examples` |
| Unrelated sibling DLL exists | Do not deploy it |
| Destination directory is absent | Create it during the build script |
| Source/destination read, directory creation, or copy fails | Return a diagnostic containing both relevant paths |
| Supported MSYS2 deployment is complete | Direct Cargo test/example/bench commands start without `STATUS_DLL_NOT_FOUND` or runtime PATH injection |

### 5. Good / Base / Bad Cases

- Good: derive one source `bin` directory from pkg-config, copy its SoXR and
  matching MinGW runtime set into all three Cargo executable directories, then
  run the quality bench directly.
- Base: a vcpkg static triplet links without runtime DLL deployment.
- Bad: copy only `libsoxr.dll` to `target/release`; benchmark executables live
  in `target/release/deps` and the DLL itself imports `libgomp-1.dll`.
- Bad: make every developer prepend an arbitrary MinGW directory to runtime
  `PATH`; another ABI-compatible-looking `libgomp-1.dll` can fail or hang during
  DLL initialization.

### 6. Tests Required

- Path resolution asserts `<prefix>/lib/pkgconfig -> <prefix>/bin` for both
  accepted SoXR DLL names.
- A filesystem fixture asserts exact deployment of the four supported runtime
  DLLs to profile root, `deps`, and `examples`, while an unrelated DLL remains
  absent.
- A stale-destination fixture asserts every executable directory is refreshed
  from the configured source.
- On Windows MSVC with MSYS2 SoXR, run at least one SoXR-using custom bench
  directly with no temporary PATH injection; `--quick --enforce` must reach the
  benchmark report and exit zero.

### 7. Wrong vs Correct

#### Wrong

```rust
let dll = pkg_config_dir.join("..").join("bin").join("libsoxr.dll");
fs::copy(dll, profile_dir.join("libsoxr.dll"))?;
```

#### Correct

```rust
let prefix = pkg_config_dir.ancestors().nth(2).ok_or("missing prefix")?;
let dll = prefix.join("bin").join("libsoxr.dll");
deploy_runtime_dlls(&dll, out_dir)?; // root + deps + examples, same-source closure
```
