# Representative resampler matrix evidence

Date: 2026-07-26

Status: corrected formal evidence, code-spec synchronization, and the final
post-documentation quality rerun are complete. The Trellis task remains
`in_progress` until its task-only commit boundary is executed.

## Coverage result

The primary all-features full report contains the exact 11-project inventory,
22 measured rate cases, zero unavailable engines, and no invalid quality or
work rows. Its machine-readable coverage summary is:

```text
entries=11
all_terminal=true
measured=11
unavailable=0
cases=22
quality_valid=22/22
work_valid=22/22
```

Every engine has both `music_44k1_to_48k` and `music_48k_to_44k1` case keys.
The benchmark generated the coverage table from actual measured/unavailable
rows and validated it before printing or writing JSON. The formal run also
used `--require-complete-matrix`, which writes JSON before rejecting any
non-terminal row.

## Primary throughput result

Medians are nanoseconds per input sample from full mode: 32 warm-up buffers,
1,000 timed buffers per trial, and 11 alternating-order trials.

| Engine | 44.1 -> 48 | 48 -> 44.1 | Lane |
| --- | ---: | ---: | --- |
| audio-engine-core | 12.638 | 9.294 | f64 |
| raw libsoxr | 12.155 | 9.528 | f64 |
| raw Rubato | 14.858 | 9.145 | f64 |
| libsamplerate | 406.935 | 346.797 | f32 |
| FFmpeg libswresample | 8.585 | 6.910 | f64 |
| SpeexDSP | 343.331 | 330.236 | f32 |
| r8brain | 21.721 | 23.165 | f64 |
| zita-resampler | 37.614 | 37.171 | f32 |
| WebRTC | 9.511 | 9.330 | f32 |
| WDL | 47.834 | 39.985 | f64 |
| libresample | 39.931 | 48.644 | f32 |

Cross-engine values remain report-only because lane, quality recipe, transition
band, phase, and latency policy are not identical. The closest wrapper control
is the f64 project-SoXR/raw-libsoxr pair: the project wrapper was 4.0% slower
for 44.1-to-48 and 2.5% faster for 48-to-44.1 in this run.

Complete setup/reset/drain distributions, p95 throughput, latency, impulse,
997 Hz THD+N, 18 kHz gain, alias, engine identity, and linked binary hashes are
retained in the JSON. The public interpretation is in
`docs/resampler-comparison.md`.

## Formal artifacts

Primary representative matrix:

- Path:
  `research/resampler-comparison-representative-11-full-20260726.json`
- SHA-256:
  `43F0A6F0DCD6F4443854CC6598904F63C20E1B44A8B505FDDE358FB7CB6D485F`
- Build: all Cargo features; project backend SoXR; raw SoXR and raw Rubato both
  compiled; all independent DLLs loaded through explicit paths.
- Environment: revision
  `342fd447c4c92025c86497b3cfb0d729559046ab`, dirty=true, rustc 1.93.1,
  Windows x86-64 release, Intel Family 6 Model 154.

Rubato-backend supplementary matrix:

- Path:
  `research/resampler-comparison-rubato-backend-supplementary-full-20260726.json`
- SHA-256:
  `2862137A5FC95CFF3B9EC8E54E80E3603F242420DF4BBC05821C193DBF91EBE8`
- Result: 10 engines and 20 valid cases; raw libsoxr is explicitly unavailable
  because this build intentionally compiled only the Rubato feature.
- Coverage: 11 rows, `all_terminal=false`; this artifact is not the
  representative matrix completion evidence.

The earlier four-engine quick reports are retained as historical phase-1
evidence. The old `resampler-comparison-all-11-full-20260726.json` report is
invalidated history and is not a substitute for the corrected v4 artifact.

## Native shim hashes

The reproducible build script completed after all lifecycle fixes and produced:

| Shim | SHA-256 | Bytes |
| --- | --- | ---: |
| FFmpeg libswresample | `955FF2955EA42DAD4E774BCF07E1AFB585778FA0ED03D5F3942FB64F67B6C82C` | 677931 |
| SpeexDSP | `5E29228A9A55A0E05097D8C9BA380AA6BA3D7079551167E2EA246A53A1042060` | 677571 |
| r8brain | `8AAC05F8FA830DD7A38760A9A902E2C0C34C3DA4A74E60EA8344FB49B52BB453` | 891260 |
| zita-resampler | `6299790821FB38517D1A3C6450DEEBC0AB04BBC79C76F57E93FE02FA2C604CC5` | 698762 |
| WebRTC | `7DA0744339FB944E60348AE3F015272CA0C79D8313A06FC564853791AE5B8E4C` | 726302 |
| WDL | `CBF5ADF903FED511BD2EF321830A90B397E6D022691E337C52A188B024B4B359` | 704111 |
| libresample | `23CABA6DA1423B2AE8D09D3F46F6992AAFF5A11830F893F7392198FE1E2E6C91` | 695625 |

These DLLs and their dependencies remain in ignored
`target/benchmark-deps/build/resampler-shims/` storage. The report records the
canonical loaded path and hashes of linked runtime DLLs as well.

## Defects exposed by evidence testing

1. The first r8brain shim build crashed with `STATUS_ACCESS_VIOLATION`. The
   build and adapter ownership were corrected, then isolated and combined
   smoke runs passed.
2. FFmpeg produced one extra frame for a 4,097-frame irregular stream. The
   shim now tracks cumulative input/output, caps process and drain to the exact
   rational target, and does not declare drain complete after a short native
   flush.
3. SpeexDSP 1.2.1 `speex_resampler_reset_mem` left stale right-channel history
   in the tested interleaved build. The first reset/fresh difference occurred
   at frame 0, channel 1 (`0.0` vs `-0.019434956833720207`). Reset now creates
   an equivalent replacement native state first, swaps only after successful
   construction, and preserves the old state on failure.
4. Reset failures originally printed entire sample vectors. The evidence test
   now reports length or the first differing sample/frame/channel and values.
5. Review found that exact-length silence could pass the old quality floor,
   timed output allowed one native buffer of slack, latency fields conflated
   buffering with signal alignment, pre-write baseline errors lost partial
   evidence, and arbitrary native bytes could carry formal provenance claims.
   Schema v4 adds signal-energy validity, exact warm-up/process/drain totals,
   separate API/observed/impulse latency fields, deferred run failures with JSON
   read-back, canonical create probes, and a formal provenance gate.
6. The all-engine quick rerun then exposed libsamplerate's complete-stream
   endpoint rule: 2,560 frames at 44.1-to-48 kHz produce 2,787 frames, while
   nearest rounding predicts 2,786. Its adapter now declares the upstream
   `ceil(input_frames * ratio)` contract and uses algorithm ID v3; the global
   exact-work gate remains unchanged.

After these fixes, both all-features and Rubato-only ignored native evidence
tests loaded all seven ABI-v2 shims and passed ABI, metadata, path/hash/dependency,
irregular progress, exact output length, finite output, complete drain,
idempotent terminal drain, reset, and fresh-stream equivalence checks in both
rate directions.

## Final verification

The post-documentation quality gate passed on 2026-07-26:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`
- `cargo test --all-features`: 366 unit, 20 benchmark-support, 23 comparison,
  3 Windows-runtime, and 6 doctests passed; the native evidence test was the
  only ignored comparison test in this command.
- `cargo test --no-default-features --features rubato`: 399 unit, 20
  benchmark-support, 22 comparison, 3 Windows-runtime, and 6 doctests passed;
  the native evidence test was the only ignored comparison test in this
  command.
- The ignored native evidence test then passed explicitly under both feature
  matrices (1/1 each) against all seven ABI-v2 shims.

The primary and supplementary JSON SHA-256 values remained
`43F0A6F0DCD6F4443854CC6598904F63C20E1B44A8B505FDDE358FB7CB6D485F`
and `2862137A5FC95CFF3B9EC8E54E80E3603F242420DF4BBC05821C193DBF91EBE8`.
The remaining workflow action is task-only commit reconciliation; unrelated
playback and benchmark-coverage worktree changes must stay out of that commit.
