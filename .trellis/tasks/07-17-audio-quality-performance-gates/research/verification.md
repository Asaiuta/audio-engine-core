# Quality And Performance Gate Verification

## Scope

This verification covers only the chosen three-entry MVP:

1. `audio_quality_measurements`;
2. `audio_callback_chain_perf`;
3. `audio_resampler_streaming_perf`.

The convolver, FIR, lock-free, and future listening/nonlinear benchmark
migrations remain owned by their corresponding P1 tasks.

## P0 regression evidence

The mechanism-to-test matrix is recorded in
[`current-gate-gap-audit.md`](current-gate-gap-audit.md). The final all-features
and no-default-features suites cover the SoXR consumption/drain/reset, final
impulse and downstream tail propagation, EQ target-state adoption, loudness
config publication, RBJ shelf coefficients, sample-rate state preservation,
and unknown-tail early-stop/safety-cap regressions. No duplicate mechanism
tests were added merely to increase test count.

## Report and gate verification

### Quality

`audio_quality_measurements --quick --enforce --out` passed with:

- schema version 1 and explicit revision/dirty/rustc/target/OS/arch/CPU/profile/features;
- 16 deterministic gates passed, 4 report-only metrics, and 2 skipped corpus gates;
- 53 missing EBU loudness files and 9 missing EBU true-peak files shown in text;
- a synthetic full-output point with 71,332 rendered frames, 494 algorithmic
  latency frames, 0 finite semantic-tail frames, and `tail_truncated = false`;
- the known full-output true-peak result still report-only at about -0.610 dBTP
  (0.390 dB above the -1 dBTP limiter target).

The full mode also passed and emitted the same schema/classification semantics.
PowerShell JSON round-trip checks verified the environment, skipped count, and
all four timing/tail fields, including equality between serialized output frame
count and authoritative `RenderedOutput::rendered_frames`.

### Callback chain

Quick/full/heavy all passed report validity under `--enforce` and emitted 12
unique cases with respectively 7/9/15 raw trials. The final quick 512-frame
evidence on the recorded Windows x86_64 release environment was:

| Scenario | Median | P95 callback utilization |
| --- | ---: | ---: |
| Active DSP, no convolver | 116.8 ns/sample | 1.41% |
| Active DSP, 256-tap convolver | 126.2 ns/sample | 1.24% |

Every case validated finite output, expected changed/bypass behavior, and full
512-frame consumed/produced progress outside the timed region.

### Streaming resampler

Quick/full/heavy all passed report validity under `--enforce` and emitted 18
unique cases with respectively 7/9/15 raw trials. The final quick 44.1->48 kHz,
512-frame, `process_checked` evidence was 7.90 ns/input-sample median and 0.084%
p95 source-buffer realtime-reference utilization.

Every case retained complete per-trial consumed/output totals. A fixed 8-buffer
validation window required complete input consumption, finite output, and
non-zero output within the window, without incorrectly requiring every native
SoXR call to produce immediately.

## Baseline policy verification

- A real compatible callback quick report comparison passed all cases.
- Shared tests prove exactly +10% median regression passes and +10.01% fails.
- Failure formatting includes case key, baseline median, candidate median,
  measured regression, and threshold.
- Shared identity tests reject schema, probe, mode, conditions, profile, CPU,
  feature, unknown required environment, duplicate case, and missing case-set
  mismatches before computing misleading percentages.
- Revision and dirty state are intentionally ignored for compatibility but
  retained in reports and baseline references.

Shared-runner CI supplies `AUDIO_BENCH_REVISION=${{ github.sha }}`, runs the
three quick `--enforce --out` entries, and uploads all JSON reports. It supplies
no timing baseline and therefore enforces no cross-machine absolute timing.

## Build and release checks

The final verification matrix includes:

- `cargo fmt --all -- --check`;
- `cargo test --all-features`;
- `cargo test --no-default-features`;
- `cargo clippy --all-targets --all-features -- -D warnings`;
- `cargo clippy --all-targets --no-default-features -- -D warnings`;
- `DOCS_RS=1 RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`;
- `cargo package --all-features --allow-dirty` (210 files, 1.6 MiB,
  isolated package build passed).

The first package attempt was blocked only while updating crates.io inside the
sandbox (`SEC_E_NO_CREDENTIALS`). Per the release spec it was rerun with normal
network authority and passed, so this was not a package-content failure.

## Local Windows runtime note

The locally copied `libsoxr.dll` depends on `libgomp-1.dll`. Cargo benchmark
processes initially returned `STATUS_DLL_NOT_FOUND`; one unrelated local
`libgomp-1.dll` also hung during DLL initialization. Using the ABI-compatible
installed runtime made quality and resampler reports complete in roughly two
seconds. Ubuntu CI installs `libsoxr-dev` and does not rely on this local
Windows runtime arrangement.

## Honest limitations retained

- Missing EBU Tech 3341/3342 vectors remain `skipped`, not conformance passes.
- Full-output true peak remains report-only because downstream resampling and
  final quantization can reintroduce intersample peaks.
- Timing evidence is machine/configuration specific. It is not a claim of
  globally highest performance or best audio quality.
