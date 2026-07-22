# Long IR Convolver Evidence

## Selected implementation

The partitioned tail now uses `realfft` half-spectra and stores each channel's
IR spectra and input-history ring in contiguous buffers. The overlap-save head
and the public convolver routing remain unchanged. The final partition policy
is 1024 frames, because it is the best measured compromise for the required
long-IR workload.

All callback storage, FFT plans, spectra, history, and scratch buffers are
constructed before processing. Real FFT backend failures use the static,
allocation-free `ProcessError::Backend` variant.

## Benchmark protocol

The maintained `audio_convolver_perf` report covers 54 cases:

- throughput: 256, 2048, 4097, 8192, 16384, 32768, and 65536 taps;
- channels: 2 and 6;
- callback blocks: 64, 128, 256, and 512 frames;
- seven trials, four warmup calls, and a quick throughput window of at least
  roughly 15 ms for the short cases.

The complex baseline was generated in a detached old-HEAD worktree with the
same harness and conditions. Reports use the same Windows MSVC release
environment and CPU. The canonical files are:

- `convolver-complex-stable.json`
- `convolver-realfft-stable.json`
- `convolver-realfft-stable-repeat.json`
- `convolver-layout-control-clean-2.json`
- `convolver-layout-direct-index-repeat.json`

## Throughput comparison

`process_into` median, ns/sample, complex baseline -> final RealFFT/layout
candidate. The candidate values below come from the direct-index repeat report;
the short/medium overlap-save path is unchanged by the layout work:

| IR taps | 2 channels | 6 channels |
| ---: | ---: | ---: |
| 256 | 9.553 -> 9.104 (-4.7%) | 9.873 -> 10.514 (+6.5%) |
| 2048 | 13.258 -> 13.730 (+3.6%) | 13.257 -> 13.311 (+0.4%) |
| 4097 | 32.996 -> 25.239 (-23.5%) | 49.427 -> 30.065 (-39.2%) |
| 8192 | 38.852 -> 28.757 (-26.0%) | 44.792 -> 29.378 (-34.4%) |
| 16384 | 61.613 -> 37.120 (-39.8%) | 63.825 -> 35.696 (-44.1%) |
| 32768 | 92.396 -> 51.827 (-43.9%) | 131.413 -> 62.837 (-52.2%) |
| 65536 | 187.115 -> 89.696 (-52.1%) | 294.506 -> 127.657 (-56.7%) |

## Layout A/B decision

The nested-vector RealFFT implementation was measured immediately before the
layout change in `convolver-layout-control-clean-2.json`. The direct-index
candidate was measured with the same harness and environment in
`convolver-layout-direct-index-repeat.json`. The table reports candidate versus
control medians and the candidate delta:

| IR taps | 2 channels | 6 channels |
| ---: | ---: | ---: |
| 4097 | 25.239 vs 29.065 (-13.2%) | 30.065 vs 33.987 (-11.5%) |
| 8192 | 28.757 vs 33.368 (-13.8%) | 29.378 vs 38.100 (-22.9%) |
| 16384 | 37.120 vs 50.193 (-26.0%) | 35.696 vs 48.142 (-25.9%) |
| 32768 | 51.827 vs 87.495 (-40.8%) | 62.837 vs 112.526 (-44.2%) |
| 65536 | 89.696 vs 139.395 (-35.7%) | 127.657 vs 195.467 (-34.7%) |

The first iterator-based flattened prototype was rejected because it regressed
small tail rings. Direct row-offset indexing removes that iterator overhead and
preserves the partition accumulation order. The result is retained; a later
run with the same source still showed Windows scheduler variance, so the
numbers are evidence rather than a hard per-run timing guarantee.

Discarded diagnostic reports remain in the task directory for auditability.
`convolver-layout-control.json` and `convolver-layout-candidate.json` were run
while a stale `audio_fir_eq_perf` process consumed one CPU core and must not be
used for comparison. `convolver-layout-candidate-clean-1.json` is the rejected
iterator prototype. `convolver-layout-direct-index.json` and
`convolver-realfft-flat-final.json` show additional scheduler/frequency noise;
the interleaved control and direct-index repeat named above are the retained
layout decision pair.

## Partition sweep

The following values are from the same quick workload and candidate code. The
callback columns are the 65536-tap, 6-channel, 64-frame case.

| Partition | 8192 taps ns/sample | 65536 taps ns/sample | callback p99 | callback max |
| ---: | ---: | ---: | ---: | ---: |
| 512 | 47.35 | 304.19 | 125.56% | 133.06% |
| 1024 | 28.99 | 130.08 | 72.48% | 79.47% |
| 2048 | 31.54 | 179.49 | 114.17% | 157.55% |

512 increases the number of tail FFTs; 2048 makes each callback boundary too
expensive, especially for six channels. No partition-size change is retained.

## Callback burst evidence

For the final flattened-layout repeat, the worst case remained 65536 taps,
6 channels, and 64 frames: p99 58.67% and raw maximum 93.92% of the 1.333 ms
callback deadline. The other block sizes were:

| Frames | p99 | max |
| ---: | ---: | ---: |
| 64 | 58.67% | 93.92% |
| 128 | 39.22% | 41.83% |
| 256 | 22.89% | 22.89% |
| 512 | 14.84% | 14.84% |

Another final-source run recorded p99 values of 75.21%, 44.32%, 28.51%, and
11.65%; its raw 64-frame maximum was 96.31%. These samples are individual
Windows wall-clock callback timings, so scheduler preemption can add hundreds
of microseconds to an otherwise bounded DSP call. The raw maximum is retained
in every JSON report and is not hidden or replaced by a best-of-N value. A
future hard max gate should use an isolated or affinity-pinned callback probe.

## Correctness and safety

- 50 focused convolver/lifecycle/no-allocation tests pass.
- All-features matrix: 350 tests, benchmark support, Windows runtime tests,
  and doctests pass.
- Pure-Rust Rubato matrix: 346 tests, benchmark support, Windows runtime tests,
  and doctests pass.
- `audio_fir_eq_perf --quick --enforce` passes; apply remains overlap-save.
- `audio_callback_chain_perf --quick --enforce` passes, including the active
  256-tap convolver chain and isolated Saturation 4x cases.
- `cargo fmt --all -- --check`, `cargo check --lib --benches`, both strict
  Clippy matrices, and `cargo rustc --lib -- -D unused-crate-dependencies`
  pass.
- Partitioned output is checked against the overlap-save/direct convolution
  oracle for mono, stereo, surround, irregular chunks, reset, finish, and
  in-place processing.
