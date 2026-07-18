# Verification

Date: 2026-07-18

Environment:

- Windows x86_64 MSVC
- rustc 1.93.1 (01f6ddf75 2026-02-11)
- Intel64 Family 6 Model 154 Stepping 3
- benchmark profile: release

## Static and API checks

- `cargo check --all-targets --all-features`: passed.
- `cargo fmt --all -- --check`: passed after the final code change.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy --all-targets --no-default-features -- -D warnings`: passed.
- `cargo rustdoc --all-features --lib -- -D warnings`: passed.
- Repository-wide migration search found no live use of the removed
  `offline_stage_names`, `offline_stage_order_csv`, `.offline_stage`, or
  `status().is_quiescent()` APIs.

## Tests

- `cargo test --lib output_chain`: 17 passed.
- `cargo test --lib convolver`: 42 passed after the final quiescence race
  regression was added.
- `cargo test --all-features`: 293 library tests, 8 benchmark-support tests,
  3 Windows runtime tests, and 2 doctests passed.
- `cargo test --no-default-features`: 285 library tests, 8 benchmark-support
  tests, 3 Windows runtime tests, and 2 doctests passed.

The added tests cover new-thread first-use allocation behavior, concurrent
retirement/reclamation and destructor thread identity, single-consumer lease
conflicts across all three construction entries, lease cleanup, partial finish
plus disable, stale generation acknowledgement, wrapping generation, and the
retirement-slot TOCTOU discovered during final review.

## Quality and performance gates

- `cargo bench --bench audio_quality_measurements -- --quick --enforce`:
  25/25 gates passed, 4 report metrics, 0 failures, 0 skipped. The local EBU
  corpus passed 55 loudness files and 9 true-peak files.
- `cargo bench --bench audio_callback_chain_perf -- --quick --enforce`:
  passed. The final 512-frame active DSP + IR256 case measured a median
  122.112 ns/sample and 1.1723% callback-deadline utilization. The reported
  callback plan contains the eight executed stages and excludes Resampler,
  Quantize, and Meter.
- `cargo bench --bench audio_convolver_perf -- --quick --enforce`: passed.
  Short/medium IRs remained overlap-save and long IRs remained partitioned.
- `cargo bench --bench audio_fir_eq_perf -- --quick --enforce`: passed. All
  measured FIR tap counts remained on the overlap-save route.

No convolution math, IR routing threshold, saturation/crossfeed tuning, Meter
CPU path, or output sound policy changed in this task.

## Miri

The exact concurrent ownership test was run with nightly Miri under both
Stacked Borrows and Tree Borrows:

```text
processor::adapters::convolver::tests::
concurrent_reclaim_and_audio_retirement_drop_every_kernel_off_audio ... ok
```

In both modes the target test body completed successfully. The Miri process
then exited non-zero while the Rust test harness was being destroyed because
the crate-wide `assert_no_alloc::AllocDisabler` wraps the Windows `System`
allocator. Both backtraces end in `std::sys::alloc::windows` during harness
channel deallocation and contain no project handoff frame. This is recorded as
a Miri/test-allocator compatibility limitation, not as a clean Miri pass and
not as evidence of handoff undefined behavior.

## Review finding fixed during verification

The first `is_quiescent()` implementation checked both pointer slots before
loading the drained generation. Audio could store the last retirement and
acknowledge that generation between those reads. The final implementation
performs an initial slot rejection, reads the versioned acknowledgement, and
then rechecks both slots. A deterministic hook test reproduces that exact
interleaving and requires teardown to remain blocked until control reclaims the
retired value.
