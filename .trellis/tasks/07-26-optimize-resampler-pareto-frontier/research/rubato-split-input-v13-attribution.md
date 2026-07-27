# Rubato Split-Input v13 Attribution

Date: 2026-07-27

## Candidate

The v13 candidate maps an already-staged FIFO prefix and the caller suffix into
one stack `Adapter<f64>`. Rubato copies each channel from the two interleaved
segments directly into its existing planar input scratch. This avoids copying
the suffix into `SampleRing` when a 128, 256, or 512-frame caller completes the
production 1024-frame FFT input chunk.

The production algorithm identifiers are:

* `streaming_native_interleaved_halfband2x_fft1024_sub2_split_input_direct_native_v13`
* `audio_engine_core_rubato_fft1024_subchunk2_split_input_direct_native_v13`

## Correctness

* Focused Rubato backend tests: 21 passed before the temporary attribution
  probe was added.
* Strict project/raw Rubato 1024/2 complete streams: bit-exact in both canonical
  directions.
* Explicit 128/256/512 caller controls match the forced FIFO completion route
  bit-for-bit with constrained 257-frame output.
* Split completion, reset/fresh, and complete drain are bit-exact.
* Split adapter channel-copy and split completion execute inside
  `assert_no_alloc` after setup.

## Timing Evidence

The ordinary pinned comparison remains frequency-contaminated on the balanced
Windows power plan. Even 40,000-call (about 400 ms) trials retained paired
project/raw spreads wider than 20 percentage points. Those reports are retained
for transparency but are not used to attribute the candidate:

* `rubato-strict-v13-split-input-vs-raw1024-pinned-heavy.json`
* `rubato-strict-v13-split-input-vs-raw1024-pinned-longtrial.json`
* `rubato-strict-v12-disabled-split-input-vs-raw1024-pinned-longtrial.json`

The direct attribution used one optimized test binary with debug assertions
enabled because `assert_no_alloc` intentionally removes its test allocator in
plain release builds. Two otherwise identical production backends differed
only by the test-only `split_input_enabled` flag. Each trial contained 200
batches of 200 512-frame calls. Batch order alternated v13/v12, then v12/v13,
to put both variants inside the same short frequency interval. There were 15
trials after 4,000 warm-up calls.

| Direction | v13 median | v12 FIFO median | Paired delta median | Improving trials |
| --- | ---: | ---: | ---: | ---: |
| 44.1 -> 48 kHz | 32.183538 ns/sample | 33.120200 ns/sample | -1.719% | 13 / 15 |
| 48 -> 44.1 kHz | 31.075220 ns/sample | 31.778164 ns/sample | -1.961% | 14 / 15 |

Paired delta vectors, where negative is faster:

```text
44.1 -> 48:
-1.7503, +0.0227, -1.2092, -1.7187, -2.8281,
-2.4024, -1.7662, -1.9911, -0.2770, +0.4996,
-2.5360, -1.0344, -0.6384, -0.2347, -2.8031

48 -> 44.1:
-2.6621, -0.9959, +1.3763, -2.2120, -1.4408,
-3.6926, -4.3796, -1.2352, -1.9607, -0.4305,
-2.3389, -3.1379, -1.6440, -2.2157, -0.5644
```

## Decision

The candidate has a repeatable, bit-exact benefit in both directions, but its
isolated forward gain is below the task's 5% retained-improvement target. Keep
it only as an intermediate candidate while pursuing additional input-copy and
adapter reductions; do not claim the Rubato acceptance criterion is complete.

## Bulk-output v14 follow-up

The v14 candidate replaces Rubato's default per-sample interleaved output
adapter writes with a channel-wise bulk `AdapterMut` implementation. A direct
ABBA attribution against v13 changed only that test-only output adapter:

| Direction | Paired delta median | Improving trials |
| --- | ---: | ---: |
| 44.1 -> 48 kHz | -1.637% | 12 / 15 |
| 48 -> 44.1 kHz | -1.935% | 15 / 15 |

The retained v14 production identifiers were:

* `streaming_native_interleaved_halfband2x_fft1024_sub2_bulk_io_split_input_v14`
* `audio_engine_core_rubato_fft1024_subchunk2_bulk_io_split_input_v14`

Focused backend tests passed, and strict project/raw Rubato 1024/2 complete
streams remained bit-exact in both canonical directions. The temporary ABBA
probe, its default-output helper, and the output-adapter switch were removed
after attribution. The permanent `split_input_enabled` test switch remains only
because it drives the FIFO-completion bit-exact oracle.

Strict v14 versus raw Rubato 1024/2 evidence is retained in:

* `rubato-strict-v14-bulk-io-vs-raw1024-pinned-quick.json`
* `rubato-strict-v14-bulk-io-vs-raw1024-pinned-heavy.json`
* `rubato-strict-v14-bulk-io-vs-raw1024-pinned-heavy-a2.json`

The quick steady medians favored production by 3.86% forward and 2.01%
reverse. The two heavy confirmations favored production by 7.94% / 1.26% and
2.80% / 1.94% in their aggregate medians; paired trial medians favored
production by 4.11% / 2.17% and 2.92% / 2.21%. Host frequency drift on the
balanced Windows power plan remains visible, so the reports do not support a
portable absolute-performance claim.

## Rejected four-frame unroll v15

The v15 experiment manually unrolled four stereo output frames in the bulk
adapter. Against the same default Rubato output adapter it improved only
1.593% forward and 0.817% reverse, both weaker than v14's iterator-based bulk
implementation. The unroll was reverted and no production identifier was
retained. Do not repeat it without a materially different code-generation or
SIMD hypothesis.

## Partial-zero drain v16 attribution

The v16 intermediate replaces explicit all-zero FIFO staging during FFT-only
drain rounds with Rubato's stack-only `Indexing::partial_len(0)` contract. The
old explicit-zero route remains available only under `cfg(test)` as a permanent
complete-stream oracle. Both canonical directions and 128/256/512-frame caller
schedules were bit-exact with constrained 257-frame output; the new drain path
also passed `assert_no_alloc` and stable terminal-finish checks.

An optimized same-process ABBA attribution primed four complete 1024-frame
chunks before each timed terminal drain, alternated candidate/control order for
1,000 drains per trial, and retained 15 trials. Relative to explicit-zero v14,
the paired median changed by -0.533% forward and -1.191% reverse. This is a
repeatable but small intermediate benefit; by itself it does not close the raw
Rubato drain gap. The next candidate must remove the terminal spill that is
currently written into staging/ring storage and immediately discarded at the
exact-duration boundary.

## Terminal-truncating drain v17 attribution

The v17 candidate adds a bulk terminal output adapter for the common case where
one FFT zero-input step can produce every frame still authorized by exact
duration and caller capacity can hold that remainder. It writes only the
caller-visible interval and discards native delay/suffix frames in the adapter,
instead of copying the suffix into `out_stage`, pushing it through `out_fifo`,
and clearing it immediately at terminal. Constrained output that cannot retain
all still-required frames continues to use the established split-spill path.

The complete-stream oracle compared v17 with forced v16 split-spill in both
canonical directions and remained bit-exact. Focused tests also cover the
adapter mapping, constrained fallback, idempotent terminal state, and
`assert_no_alloc` execution.

The same optimized ABBA method measured v17 against v16 over 15 trials:

| Direction | Paired delta median | Improving trials |
| --- | ---: | ---: |
| 44.1 -> 48 kHz | -1.872% | 15 / 15 |
| 48 -> 44.1 kHz | -2.499% | 15 / 15 |

Combined multiplicatively with v16's partial-zero attribution, terminal drain
is about 2.4% faster forward and 3.7% faster reverse than the explicit-zero v14
route. The temporary timing probe was removed after recording these results.

## v17 streaming matrix and release-gate evidence

The retained v17 production route was exercised through the public
`StreamingResampler` paths with 128, 256, 512, and 1024-frame callers. The
reports are pinned to logical core 2 and retain both `process_checked` and
direct-trait cases:

* `rubato-streaming-v17-terminal-drain-pinned-quick.json`
* `rubato-streaming-v17-terminal-drain-pinned-heavy-a1.json`
* `rubato-streaming-v17-terminal-drain-pinned-heavy-a2.json`

The algorithm identifier intentionally differs from v12, so these reports are
not accepted by the automatic same-algorithm baseline gate. The table below is
a manual same-workload row match against
`rubato-streaming-v12-direct-native-pinned-heavy-a2.json`. For each caller size,
the candidate range covers the two public API paths and the delta is the less
favorable of those two paths. Negative deltas are improvements.

| Direction | Caller frames | v17 median range (ns/input sample) | Worst median delta vs v12 A2 | Worst p95 delta vs v12 A2 |
| --- | ---: | ---: | ---: | ---: |
| 44.1 -> 48 kHz | 128 | 8.035-8.089 | -25.11% | -19.75% |
| 44.1 -> 48 kHz | 256 | 8.011-8.013 | -25.11% | -6.25% |
| 44.1 -> 48 kHz | 512 | 7.720-7.870 | -16.26% | -2.27% |
| 44.1 -> 48 kHz | 1024 | 7.368-7.781 | -34.29% | -28.75% |
| 48 -> 44.1 kHz | 128 | 6.828-7.141 | -29.32% | -20.02% |
| 48 -> 44.1 kHz | 256 | 6.683-6.847 | -39.62% | -23.14% |
| 48 -> 44.1 kHz | 512 | 6.949-7.104 | -23.91% | -22.33% |
| 48 -> 44.1 kHz | 1024 | 6.875-6.880 | -33.41% | -24.83% |

The first v17 heavy run also beat both v9 heavy reports in all 16 canonical
rate/API/caller cases. Cross-run comparisons against the two v12 reports had
three p95 outliers and one median outlier per baseline, but the affected rows
changed between runs and disappeared in v17 A2 versus v12 A2. This is
consistent with the already documented balanced-power frequency drift, not a
repeatable caller-size regression. The retained adjacent-feature ABBA probes
remain the attribution evidence for v13, v14, v16, and v17.

The broader verification reports are:

* `rubato-v17-quality-quick.json`: 27/27 gates passed, zero failed; 44.1 to
  48 kHz THD+N was -204.95 dB and the 96 to 48 kHz worst alias attenuation was
  -290.48 dB.
* `rubato-v17-output-render-quick.json`: all 18 complete-render cases passed,
  including 64/4096-frame execution and one/five-second inputs; fixed-scenario
  temporary memory remained duration-bounded.
* `rubato-v17-lifecycle-memory-quick.json`: all 13 timing cases were valid;
  active resampler reset and finish each performed zero Rust allocations,
  deallocations, and reallocations; the 5 x 128 lifecycle soak retained zero
  Rust bytes.

Full feature validation after collecting those reports passed as follows:

* Pure Rubato: 410 library tests, 20 benchmark-support tests, 25 comparison
  tests (one native-shim evidence test ignored), three Windows-runtime tests,
  and six doctests; zero failures.
* All features / SoXR precedence: 368 library tests, 20 benchmark-support
  tests, 25 comparison tests (the same native-shim evidence test ignored),
  three Windows-runtime tests, and six doctests; zero failures.

## Final-source verification reruns

The final lint-only source was revalidated rather than relying on the earlier
full matrices:

* Pure Rubato: 410 library tests, 20 benchmark-support tests, 25 comparison
  tests (one native-shim evidence test ignored), three Windows-runtime tests,
  and six doctests; zero failures.
* All features / SoXR precedence: 368 library tests, 20 benchmark-support
  tests, 25 comparison tests (the same ignored evidence test), three
  Windows-runtime tests, and six doctests; zero failures.
* Both strict `cargo clippy --all-targets ... -- -D warnings` matrices and
  `cargo fmt --all -- --check` passed.

The first final-source public heavy report,
`rubato-streaming-v17-terminal-drain-pinned-heavy-final-source.json`, is a
retained failed timing sample. It ran immediately after release compilation on
the Balanced Windows power plan and regressed several v12 A2 rows, including
reverse medians by 11.58% to 37.50%. It is not deleted or used as acceptance
evidence.

The immediate final-source confirmation,
`rubato-streaming-v17-terminal-drain-pinned-heavy-final-source-a2.json`, passed
the matched v12 A2 gate across both public APIs:

| Direction | Caller frames | Worst median delta | Worst p95 delta |
| --- | ---: | ---: | ---: |
| 44.1 -> 48 kHz | 128 | -13.66% | -5.63% |
| 44.1 -> 48 kHz | 256 | -16.68% | -1.02% |
| 44.1 -> 48 kHz | 512 | -12.35% | +2.67% |
| 44.1 -> 48 kHz | 1024 | -32.77% | -27.90% |
| 48 -> 44.1 kHz | 128 | -28.87% | -30.40% |
| 48 -> 44.1 kHz | 256 | -36.83% | -21.36% |
| 48 -> 44.1 kHz | 512 | -22.77% | -21.00% |
| 48 -> 44.1 kHz | 1024 | -27.79% | -11.91% |

Two final-source strict controls preserve the fourth geometry combinations:

* `rubato-strict-v17-terminal-drain-vs-raw512-sub1-pinned-heavy-final-source.json`
* `rubato-strict-v17-terminal-drain-vs-raw1024-pinned-heavy-final-source.json`

The same-geometry 1024/2 report measured project/raw steady medians of
7.794/8.059 ns/input sample forward (-3.29%) and 7.242/7.510 reverse (-3.57%).
The project forward p95 was 11.272 versus raw 9.810, so the result supports a
median win only. Together with the earlier project-512/raw-512 and
project-512/raw-1024 reports, the raw/project x 512/1024 attribution matrix is
complete. Every final strict case retained 15 raw trials and passed exact-work
and quality validity.
