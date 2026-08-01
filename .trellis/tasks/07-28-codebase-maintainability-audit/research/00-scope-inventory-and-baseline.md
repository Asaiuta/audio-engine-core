# Scope, inventory, and validation baseline

## Snapshot

- Local audit window: 2026-07-28 14:43:59 to 14:49:23 +08:00.
- Branch: `main`, tracking `origin/main`.
- HEAD: `0c62febd2b6afdd1800da1591b68f7a600a3835e`
  (`docs(audio): record benchmark, playback, and resampler evidence`).
- The working tree was already dirty and was being edited concurrently.
- The five tracked source changes at the end of the snapshot were:
  `src/lib.rs`, `src/pipeline.rs`, `src/processor/lockfree_params.rs`,
  `src/processor/mod.rs`, and `src/processor/traits.rs`.
- Their tracked diff at this snapshot was 1,418 insertions and 80 deletions.
- The audit task itself and a separate playback-facade task were untracked, as
  were unrelated `.pi-subagents/`, `.tmp/`, and root Markdown files. None of
  those pre-existing paths were modified by this audit.

The relevant source mtimes did not change while the validation commands below
ran:

| File | Size | Last write (+08:00) |
|---|---:|---|
| `src/lib.rs` | 5,796 bytes | 2026-07-28 13:10:50 |
| `src/pipeline.rs` | 95,785 bytes | 2026-07-28 14:29:10 |
| `src/processor/lockfree_params.rs` | 47,639 bytes | 2026-07-28 14:42:20 |
| `src/processor/traits.rs` | 25,047 bytes | 2026-07-28 13:46:15 |
| `src/processor/mod.rs` | 5,773 bytes | 2026-07-28 13:42:06 |

## Review inventory

The bounded Rust inventory covers `src/`, `tests/`, `benches/`, and
`examples/`. It contains 82 `.rs` files and 55,258 logical lines as counted by
PowerShell `Get-Content | Measure-Object -Line`:

| Root | Files | Lines |
|---|---:|---:|
| `src/` | 55 | 34,125 |
| `tests/` | 3 | 681 |
| `benches/` | 22 | 20,341 |
| `examples/` | 2 | 111 |

Largest files are routing hints, not findings by themselves:

| Lines | File |
|---:|---|
| 3,322 | `benches/audio_quality_measurements.rs` |
| 3,025 | `benches/resampler_comparison_support/adapters.rs` |
| 2,811 | `src/processor/resampler/rubato_backend.rs` |
| 2,245 | `src/pipeline.rs` |
| 2,041 | `benches/resampler_comparison_support/mod.rs` |
| 1,847 | `src/processor/adapters/tests.rs` |
| 1,798 | `src/processor/resampler/mod.rs` |
| 1,721 | `src/processor/output_chain.rs` |
| 1,694 | `src/processor/adapters.rs` |
| 1,417 | `benches/audio_lifecycle_memory_perf.rs` |
| 1,359 | `src/processor/lockfree_params.rs` |

## Validation results

All commands reached an explicit exit status in this snapshot.

| Command | Result | Evidence summary |
|---|---|---|
| `cargo fmt --all -- --check` | **failed (1)** | Formatting-only diffs in `src/pipeline.rs` and `src/processor/lockfree_params.rs`. |
| `git diff --check` | passed (0) | No whitespace errors; Git emitted LF-to-CRLF warnings for three modified processor files. |
| `cargo clippy --all-targets --all-features -- -D warnings` | passed (0) | Completed dev checks with warnings denied. |
| `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings` | passed (0) | Rubato-only feature matrix completed with warnings denied. |
| `cargo test --all-features` | passed (0) | 383 library tests, 20 benchmark-support tests, 25 resampler-support tests, 3 Windows deployment tests, and 6 doctests passed; one native-shim test was explicitly ignored. |
| `cargo test --no-default-features --features rubato` | passed (0) | 425 library tests plus the same 20/25/3/6 support and doc groups passed; one native-shim test was explicitly ignored. |

The Rubato-only library group took 81.95 seconds because a numerical oracle
test ran for over 60 seconds; it nevertheless completed successfully.

## Baseline conclusions

1. **The current snapshot is behaviorally green under the two tested feature
   matrices, but not formatting-clean.** This is a quality-gate defect in the
   dirty playback-facade work, not evidence that the underlying architecture is
   generally broken.
2. **Earlier observed failures are superseded.** A prior snapshot failed the
   lock-free concurrent-publication test after new parameter clamping was
   introduced. The current source now passes that test in both matrices, so the
   old failure must not appear in the final report as current.
3. **Passing Clippy and tests does not settle the maintainability audit.** The
   next review areas must inspect whether public contracts, ownership, and
   duplicated representations remain understandable and coherent.
4. **Benchmark and performance claims remain outside this baseline.** These
   commands validate test behavior, not device/driver/DAC latency or every
   benchmark enforcement mode.

## Revalidation rule

Before the final synthesis, re-read Git status and the mtimes of every file
cited by a finding. If any have moved, re-run the smallest relevant inspection
or test and mark this baseline as historical for that file.

