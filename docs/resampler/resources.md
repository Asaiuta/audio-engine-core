# Resampler CPU, memory and FFT results

The project-owned Rust backend is named **CoreSinc**. Its stable machine name
remains `rust`, so existing feature selection and historical measurements stay
compatible.

Measured on 2026-10-10, Intel Core i5-12500H (16 logical processors), Windows/x86_64, Rust 1.93.1. The latest owned implementation also shares frequency storage by preparing inverse conjugate pairs in place. It follows the time-buffer and fused-inverse optimizations documented below.

## Latest targeted optimization

The owned FFT now compacts setup-only coefficient storage and can emit a block
straight to the caller after an internally fragmented input block is completed.
The fixed three-case comparison was complete-work valid but inconclusive for
throughput; it is retained as a copy-path observation, not a speed claim. The
compact coefficient tail removes 848 bytes at 48→44.1 and 5,552 bytes at
96→44.1 per instance. See the [raw targeted observations](../data/resampler-targeted-optimization-2026-10-10.json).

The next direct-polyphase idea was screened separately against the current
quality floor. A short kernel is not sufficient: the 44.1↔48 cases need about
96 taps per phase and 88–105 estimated multiply-accumulates per input frame,
while 96→44.1 needs about 192 taps per phase. This would retain roughly
110–220 KiB of coefficients before streaming state, so it is not an obvious
CPU or memory win over the current FFT. The screen is research-only and did not
change the production route; see the [machine-readable polyphase screen](../data/resampler-polyphase-screen-2026-10-10.json).
The supporting research note is local to the development machine and is
deliberately not distributed with this repository.

## Latest frequency storage and bounded retention

Retained Rust heap falls another **5.9–9.5%**, to **88.16 / 88.17 / 143.03 /
72.01 / 146.53 KiB** for stereo 44.1→48 / 48→44.1 / 44.1→96 / 96→44.1 and
six-channel 44.1→48. The existing inverse and complete-block RealFFT oracles
pass with maximum error 0 across 556,212 outputs; allocation-free streaming
still passes. Coefficients and scheduling are unchanged.

The one 80-trial timing schedule is **inconclusive for speed**. The apparent
18.6% median reduction at 48→44.1 is smaller than the drift between the two
unchanged baseline children (9.195–10.196 versus 5.599–5.849 ns/sample). The
six-channel observed median increased 2.9%; other cases were faster or flat.
All observations are retained without further retries. See the
[current table](comparison.md#latest-spectrum-storage-and-lifecycle-results)
and [raw paired data](../data/resampler-spectrum-reuse-2026-10-10.json).

Optional `--resource-retention-rounds=32` collects 32 one-instance reuse/reset
and 32 recreate/drop streams after initial pool warmup. Fourteen fresh children
cover 12 recipes and 896 complete-duration streams. Busy bytes are flat within
each lifecycle sequence; recreate/drop returns to the warmed baseline.
SpeexDSP's private commit varies by one 4 KiB page despite flat busy bytes.
r8brain retains 1431.42 KiB from pool warmup, with no subsequent accumulation in
these 64 streams; this is not proof of a cache or long-term leak freedom.

SoXR capacity comparisons are now filled: project SoXR uses 195.05 KiB/instance
at both 33,326 and 622 advertised caller frames; raw libsoxr uses 195.03 KiB at
33,326 and 1,070 frames. See [lifecycle and capacity observations](../data/resampler-retention-2026-10-10.json).
Native exact allocation-call counts, temporal peaks and long-duration growth
remain outside these snapshots.

## Earlier time-buffer storage and capacity measurements

Retained Rust heap decreased **5.6–8.7%** across five common configurations:
stereo 44.1→48 and 48→44.1 use 97.38 KiB instead of 106.62; 44.1→96 uses
152.26 instead of 161.49; 96→44.1 uses 76.63 instead of 81.27; six-channel
44.1→48 uses 155.76 instead of 164.99. The final paired schedule measured
**0.3% / 5.5% / 4.7% / 3.1% / 2.6%** lower wall medians in that order; the
first case is effectively flat. See the [current table and validation](comparison.md#earlier-time-buffer-optimization-and-supplemental-metrics)
and [80 paired trials](../data/resampler-fft-layout-2026-10-10.json). These are
local observations, not universal speed claims or FFmpeg-speed parity.

The new `--resource-output=adapter` option permits a second capacity policy.
Twenty fresh memory-only children measured ten recipes at stereo 44.1→48.
FFmpeg's warmed process-heap increment changes from **2241.22 KiB** with 33,326
caller frames to **705.22 KiB** with the adapter's advertised 8,749 frames.
The other nine recipes had unchanged busy-byte increments between the policies.
See the [capacity table](comparison.md#output-capacity-memory-comparison)
and [raw observations](../data/resampler-capacity-memory-2026-10-10.json).
Advertised capacity is not minimum native capacity. SoXR capacity sensitivity
was not measured in that earlier round; the latest observations above fill it.

Lazy progress-label formatting removes benchmark-wrapper allocations on
successful native calls. All ten measured recipes now have zero Rust-side
process/finish/reset allocation operations; native malloc/free is not counted.
Earlier native timing tables include the old wrapper overhead and have not
been remeasured solely to update those numbers.

`--diagnostics` separately adds selected multi-level response, phase-slope delay,
impulse ringing, dense finite transition/stopband samples, silence and
64/128/512-frame call tails. Its [report](../data/resampler-diagnostics-2026-10-10.json)
has 447 complete renders per owned arm with identical quality metrics, plus
192 FFmpeg control renders. These supplements do not close the full parameter,
native allocation-peak, multistream, platform or device-latency gaps.

## Earlier fused-inverse implementation improvement

The coefficients, FFT sizes, retained frequency bins, normalization and stream scheduling are unchanged. The fused pass uses disjoint contiguous slices and skips known-zero bins. An independent RealFFT comparison exercised 15 odd/even and up/down geometries, including all High/UltraHigh common-rate sizes: 151,812 scalar outputs, maximum absolute error 0. The existing resampler suite passed 59 tests. Rubato sample equality is **not** a requirement for future algorithms; objective response, alias/image rejection, phase and transient quality must not regress.

Values are median ns/input scalar sample. Each arm has eight >=250 ms trials in one ABBA (or BAAB) schedule, 512-frame caller blocks, core 2. Cycles show the same direction. These are bounded machine-specific observations.

| Conversion | Before | After | Wall-time reduction | Cycle reduction |
| --- | ---: | ---: | ---: | ---: |
| music_44k1_to_48k | 7.503 | 6.615 | 11.8% | 10.8% |
| music_48k_to_44k1 | 7.110 | 6.151 | 13.5% | 14.5% |
| music_44k1_to_96k | 10.858 | 10.511 | 3.2% | 3.2% |
| music_96k_to_44k1 | 4.684 | 4.447 | 5.1% | 5.2% |
| music_44k1_to_48k_6ch | 7.923 | 7.493 | 5.4% | 5.9% |

The first indexed-loop candidate had two regressions and was retained as a failed generic implementation. One implementation revision removed those indexed loops; its results are the table above. No favorable-run selection or further timing retries were used. The retained Rust heap falls by only 48 bytes per instance, so this is a CPU improvement, not a material memory optimization.

## Added resource coverage

57 current recipe/rate/channel configurations, 248 steady trials. The [machine-readable report](../data/resampler-resources-2026-10-10.json) includes every rate, raw trials, thread user/kernel time, cycles, single-core audio-time budget, Rust setup/peak heap and hot allocation calls, process working-set/private-commit snapshots, and endpoint failures.

The table below selects stereo 44.1->48 kHz. CPU budget is the fraction of **one logical CPU needed to keep up with real-time audio**, not whole-machine Task Manager utilization. CPU-time accounting is quantized; use the recorded trials and cycle results rather than treating the last decimal as exact.

| Recipe | ns/sample | cycles/sample | One-core CPU budget | Rust retained / peak KiB | Private-commit increment KiB/instance |
| --- | ---: | ---: | ---: | ---: | ---: |
| Owned Rust | 6.615 | 20.38 | 0.0584% | 106.62 / 106.62 | 117.81 |
| raw Rubato FFT | 7.079 | 21.83 | 0.0623% | 250.96 / 250.96 | 280.12 |
| libsamplerate SINC_BEST_QUALITY | 281.325 | 865.56 | 2.4971% | not captured (native) | 838.75 |
| FFmpeg libswresample | 4.640 | 14.13 | 0.0405% | not captured (native) | 2245.38 |
| SpeexDSP quality 10 | 242.441 | 746.91 | 2.1029% | not captured (native) | 46.25 |
| r8brain-free-src | 17.279 | 53.28 | 0.1530% | not captured (native) | 245.12 |
| zita-resampler | 24.232 | 74.55 | 0.2103% | not captured (native) | 17.75 |
| WebRTC PushResampler | 6.255 | 19.29 | 0.0553% | not captured (native) | 52.25 |
| WDL Resampler | 35.764 | 110.33 | 0.3213% | not captured (native) | 53.25 |
| libresample high quality | 25.493 | 78.52 | 0.2352% | not captured (native) | 1291.00 |
| Project SoXR | 6.892 | 21.27 | 0.0614% | 0.02 / 0.02 | 284.75 |
| raw libsoxr stereo | 7.045 | 21.72 | 0.0619% | not captured (native) | 284.75 |

Memory increments are measured across 32 simultaneously live, warmed instances and divided by 32. They are page-granular process increments, not exact native heap payload. The common output capacity is ceil(512 * output/input rate) + 32,768 frames. Native libraries may size their internal workspaces from that capacity; the numbers therefore do not rank each library's minimum possible memory usage. The report retains absolute working set, peak working set, private commit, peak pagefile usage and page faults before/after construction, warmup and drop. Allocator caching means a process need not release pages immediately after dropping instances.

In this earlier run, the owned Rust backend had zero measured process/finish/reset heap operations. The seven native-shim adapters showed Rust-side temporary allocations during those calls; the raw counts are retained and include the old benchmark wrapper overhead. These counters do not intercept C/C++ malloc/free and must not be described as zero native allocations.

One of the four longer FFmpeg 48->44.1 resource trials hit the existing shim's `FFmpeg drain ended before the exact target duration` check. Its steady CPU measurements are retained, complete output is explicitly unknown and the trial is not a successful complete-stream check. The initial failed child is retained separately. Other current resource rows completed their duration checks.

## Coverage verdict and FFmpeg comparison

The subsequent native-memory addition uses Windows `HeapWalk` to record busy
heap-block bytes/counts at the same four lifecycle points. One memory-only run
at stereo 44.1→48 kHz measured 11 recipes, including every native library in the
comparison, without rerunning quality grids or steady timing. Examples are
FFmpeg **2241.22 KiB/instance**, libsamplerate **859.58**, project SoXR **195.05**,
SpeexDSP **39.53** and zita **15.89**. See the
[full table and scope](comparison.md#native-inclusive-heap-snapshots-2026-10-10)
and [raw heap snapshots](../data/resampler-native-heap-2026-10-10.json).

These process heap increments include adapter/runtime allocations and allocator
slabs. They do not establish library-specific requested bytes, native allocation
call counts or temporal peaks, and omit direct `VirtualAlloc`/mapped payloads.
They overlap with Rust heap data and must not be added to it. `null` means the
walk is unavailable or incomplete. Pool handle storage is retained through the
release snapshot so its deallocation does not look like a native memory decrease.
The native-memory addition did not rerun the owned Rust recipe or update the
earlier CPU table.

CPU and per-instance/process memory reporting are now implemented and measured. The existing [quality and latency report](comparison.md) supplies the finite response, alias/image, distortion, channel-isolation, DC/step, startup and call-tail observations; unchanged engines were not rerendered for this resource addition.

Coverage is still incomplete for exact native allocator accounting/peak payload, hours-long memory trends, other platforms/CPUs, the complete parameter/level matrix and consuming-application device latency. Finite tone grids are not continuous-band guarantees.

FFmpeg's selected default uses short SIMD polyphase filters (32/36/32/72 taps in the four stereo directions), while the owned High response uses longer FFT filtering and much stronger measured near-stopband rejection. The new fusion closes part of the CPU gap without shortening the filter. Matching FFmpeg's default throughput at the owned response remains unproven; dropping Rubato sample equality allows other algorithms, but does not make the two quality targets equivalent.

## Reproduction

```text
cargo bench -p audio-engine-core --bench audio_resampler_comparison_perf --features raw-rubato -- --resources --pinned --pin-core 2 --out target/bench-reports/resources.json
```

Add the existing explicit native-library arguments to include external engines, or `--all-features` to select project SoXR and raw libsoxr. `--resource-engines=audio_engine_core,ffmpeg_libswresample` limits engines. `--resource-ms=250` selects the minimum of four timing windows per case. Each case runs in a separate child process; allocation instrumentation is disabled during timing. This mode does not render quality grids and is report-only; `--enforce`/`--baseline` belong to the normal comparison report.

Add `--resource-memory-only` to skip steady timing and collect lifecycle memory
plus the short allocation/lifecycle pass. New reports use
`resampler_resources_v2` with `process_heap_*` snapshots and
`pooled_process_heap_busy_{bytes,blocks}_increment_per_instance`. Exact native
allocation profiling still requires separate instrumented libraries or heap
traces that cover dependency DLLs, aligned allocations and realloc/free pairs.

To compare output capacities, run memory-only mode once with
`--resource-output=common` (default) and once with `--resource-output=adapter`.
Both reports retain `output_capacity_policy` and `output_capacity_frames`.

Add `--resource-retention-rounds=32` (accepted range 1–128) for bounded lifecycle
observations. They run after throughput and report `retention.valid`, completed
stream counts, frame counts and process heap/commit snapshots. A lifecycle
error stays separate from existing CPU/pool observations. Use memory-only mode
when collecting just these observations.

For supplemental metrics without rerunning the full quality/throughput matrix:

```text
cargo bench -p audio-engine-core --bench audio_resampler_comparison_perf --features raw-rubato -- --diagnostics --diagnostic-engines=audio_engine_core --diagnostic-rates=music_44k1_to_48k,music_96k_to_44k1 --pinned --pin-core 2 --out target/bench-reports/diagnostics.json
```
