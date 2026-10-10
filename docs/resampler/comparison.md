# Resampler quality and performance comparison

The project-owned Rust implementation is named **CoreSinc**. `rust` remains
its stable machine/backend identifier for feature selection, diagnostics and
historical benchmark records.

This document has three parts. [Current comparison](#current-comparison) and
[Coverage verdict](#coverage-verdict) give the present picture and state what is
still unmeasured. [Evidence rounds](#evidence-rounds) records how the
implementation got there, newest first, each round carrying the verdict that
round actually established. Metric definitions and the reproduction commands
close the document.

Every number here belongs to the implementation state that produced it, so cite
the round when quoting one. The rounds are kept precisely because their verdicts
differ: several did **not** establish a speedup, and one established a memory
reduction with no timing claim at all. Timings from different rounds must not be
combined into a cumulative percentage.

The current default `rubato` feature selects the owned `rust` backend. Upstream
Rubato 4.0.0 is an optional control enabled by `raw-rubato`; production code does
not use it. Enabling `soxr` selects SoXR instead. The runner has **11 engine IDs**;
`audio_engine_core` reports the backend selected by each compiled build.

The document combines the 2026-10-10 storage, retention, diagnostic and
fusion/resource rounds with the repaired **v7 quality, latency and throughput
measurements**.
The earlier October v6 summaries mixed trial records between engines: 96 of 100 rows and
672 of 1,500 trials were affected. Those reports and the prior documents remain
retained as historical evidence and must not serve as current timing baselines.
The independent R5 complete-waveform and `l5_retest_v1` experiments used different
drivers and retain their original scope; see [replacement evidence](replacement.md).

## Current comparison

The CPU column is the cross-engine baseline in [Cross-engine CPU and memory
baseline](#cross-engine-cpu-and-memory-baseline) (57 configurations, 248 steady
trials). The memory columns use the 2026-10-10 observations: current owned Rust
retention after the spectrum storage change, and native-inclusive process-heap
increments for the third-party recipes. These are different memory domains and
must not be added together. `—` means that the corresponding domain was not
captured in that run.

| Engine | Established ns/input sample | One-core CPU budget | Rust retained KiB (2026-10-10) | Process-heap increment KiB/instance (2026-10-10) |
| --- | ---: | ---: | ---: | ---: |
| **CoreSinc (owned Rust)** | 6.615 | 0.0584% | **88.16** | — |
| Raw Rubato FFT 1024/2 | 7.079 | 0.0623% | 250.96 | 251.21 |
| Project SoXR High/Linear | 6.892 | 0.0614% | 0.02 (Rust only) | 195.05 |
| Raw libsoxr HQ/Bits20 | 7.045 | 0.0619% | — | 195.03 |
| FFmpeg SWR default | **4.640** | **0.0405%** | — | **2241.22** |
| libsamplerate SINC_BEST | 281.325 | 2.4971% | — | 859.58 |
| SpeexDSP quality 10 | 242.441 | 2.1029% | — | 39.53 |
| r8brain 7.1 | 17.279 | 0.1530% | — | 230.56 |
| zita-resampler 1.11.2 | 24.232 | 0.2103% | — | 15.89 |
| WebRTC v1.3 shim | 6.255 | 0.0553% | — | 43.89 |
| WDL 96b770f shim | 35.764 | 0.3213% | — | 52.05 |
| libresample 0.1.5 shim | 25.493 | 0.2352% | — | 1257.55 |

This gives the current practical ranking: FFmpeg remains fastest at the tested
default quality, while Owned Rust is about **42.6% slower** in wall time
(6.615 versus 4.640 ns/input sample) and remains close to SoXR and WebRTC.
The spectrum-storage timing schedule itself was inconclusive, so 6.615 is
retained as the last established CPU baseline rather than claimed as a new speed
result.
Owned Rust's retained Rust heap is now **88.16 KiB**, down from 97.38 KiB in
the previous implementation; this is the confirmed resource improvement of the
spectrum-storage round.

Private-commit increments are measured across 32 simultaneously live, warmed
instances and divided by 32. Output capacity is
`ceil(512 * output_rate / input_rate) + 32,768` frames. Native workspaces may
scale with this capacity, so these page-granular process increments do not rank
the libraries' minimum possible memory use. The report also retains absolute
working set, peak working set, private commit, peak pagefile usage and page
faults before/after construction, warmup and drop. Allocator caching can retain
pages after instances are dropped.

## Coverage verdict

CPU and per-instance/process memory reporting are now implemented and measured.
Quality and latency combine the Pre-fusion v7 grids with the supplemental
metrics in [Evidence rounds](#evidence-rounds). Remaining gaps include exact
native allocator accounting and peak payload, hours-long memory trends, other
CPUs/platforms, the complete rate/phase/channel/buffer/amplitude matrix and
device latency. Finite tone grids do not establish continuous-band rejection or
perceptual transparency.

The most useful remaining additions to the cross-library comparison are:

| Area | Present evidence | Remaining gap / next useful scope |
| --- | --- | --- |
| Frequency response and rejection | v7 response/alias/image grids plus 49-point transition and 33-point quadrature stopband samples for selected recipes | Dense image coverage and band-edge/extrema certification for a changed filter; finite grids are not continuous-band bounds |
| Levels and numerical accuracy | Four amplitudes at two tones and silence for selected recipes; native-format analyzer self-controls | Broader level/frequency/recipe matrix, subnormal signal behavior and an independent analyzer accuracy bound |
| Phase and transients | Phase error, two local group-delay estimates, centered pre/post-ring energy/extent, DC and full-stream step peaks | Broadband unwrapped group-delay variation and other phase modes; residual timeline delay is not API buffering |
| Streaming | Exact-duration checks, reset/fresh comparison, separate scoped irregular-buffer tests | Resolve the observed FFmpeg drain failure; compare short streams and block-boundary invariance for changed algorithms |
| Performance | Steady wall/CPU/cycles, setup/reset/drain and current 64/128/512-frame call percentiles/deadline counts for selected recipes | Irregular buffers, multistream/cache pressure and realistic callback scheduling; refresh only when the proposed change needs these claims |
| Memory | Rust allocation accounting, process commit/residency, native-inclusive heap snapshots, capacity comparisons including SoXR, and 32-round reuse/recreate observations for 12 recipes | Exact native allocation attribution/calls/peaks, persistent-block attribution, and growth over long lifetimes |
| Recipe/platform coverage | Common High/Linear comparisons on Windows x86_64; broader project-only historical matrices | Selected other tiers, phases, levels, channel counts and CPUs; device/driver latency belongs to application integration |

Some broader probes already exist in the project-only replacement research;
their coverage is not a complete current cross-library matrix. These gaps are
not a requirement to repeat all prior measurements before each local change.
For a new filter, prioritize response/phase/transient nonregression. For a
memory-reporting change, measure memory independently and reuse unchanged
quality/timing evidence.

## Evidence rounds

Each round below is one implementation change or measurement addition, newest
first, with the verdict that round established. They are kept because the
verdicts differ: several rounds did **not** establish a speedup, and one
established a memory reduction with no timing claim at all. A number is only
valid for the implementation state that produced it, so cite the round when
quoting one. The committed JSON in `docs/data/` is the primary record; each
section below summarises it.

| Round | Change | Verdict | Data |
| --- | --- | --- | --- |
| Setup-storage compaction | Keeps only the coefficient bins and inverse rotation factors `prepare_inverse` can reach; emits directly into a sufficiently large caller buffer | **Inconclusive for speed**; retained for deterministic setup-memory savings and a bounded copy reduction | [`resampler-targeted-optimization`](../data/resampler-targeted-optimization-2026-10-10.json) |
| Spectrum storage sharing | Prepares conjugate pairs inside the forward spectrum's allocation, removing the second spectrum allocation | Wall medians retained, **not confirmed speedups**; confirmed Rust heap reduction of **5.9–9.5%** | [`resampler-spectrum-reuse`](../data/resampler-spectrum-reuse-2026-10-10.json) |
| Time-buffer sharing | Forward input and inverse output share one allocation, and their scratch storage is shared too | 0.3–5.5% wall reduction on one ABBA/BAAB schedule; retained Rust KiB **106.62 → 97.38** | [`resampler-fft-layout`](../data/resampler-fft-layout-2026-10-10.json) |
| Native process-heap measurement | Independent native-inclusive process-heap walk over 11 recipes | Report-only; **not additive** with the Rust heap column | [`resampler-native-heap`](../data/resampler-native-heap-2026-10-10.json) |
| Fused inverse transform | Disjoint contiguous slices, skipping known-zero bins | **3.2–13.5%** wall reduction, all five paired cases positive | [`resampler-resources`](../data/resampler-resources-2026-10-10.json) |
| Pre-fusion v7 measurement | The v7 quality, latency and throughput runs, before any change above | Established the cross-engine CPU baseline and the quality grids | [`resampler-diagnostics`](../data/resampler-diagnostics-2026-10-10.json) |

All six rounds date to 2026-10-10 and are ordered by commit. Verdicts are the
observations recorded in that round, not cumulative claims.

### Setup-storage compaction and direct full-block emit

The current targeted change keeps only the coefficient bins and inverse rotation
factors that `prepare_inverse` can access. The FFT library still receives the
full temporary filter spectrum during setup, then the unused tail is released.
For the selected geometries this removes 53 complex values (848 bytes) at
48→44.1 kHz and 347 complex values (5,552 bytes) at 96→44.1 kHz per instance.
The selected upsampling cases do not remove inverse factors; their active range
already covers the allocated rotation table.

When a fragmented caller input fills the internal block, the stream now emits
directly into a sufficiently large caller buffer when no output has been
skipped. This avoids a `pending` copy under that condition; existing output
budget, duration and backpressure checks remain in force.

One fixed ABBA/BAAB schedule compared the v6 executable with v7 over three
representative cases (four trials per arm and case, core 2). Every trial passed
the complete-work check. The stereo 44.1→48 result showed a small apparent
improvement, 48→44.1 varied with construction/order, and six-channel 44.1→48
was effectively flat. The schedule is therefore **inconclusive for speed** and
does not justify a stable throughput claim. The direct path is retained for its
bounded copy reduction; the compact vectors are retained for deterministic setup
memory savings. Raw observations and binary hashes are in
[targeted optimization data](../data/resampler-targeted-optimization-2026-10-10.json).

The current path passed the two independent FFT oracles (maximum absolute error 0),
the 60-test resampler module set, allocation-free streaming validation and
strict scoped Clippy. The full quality grid was not rerun because the filter and
FFT arithmetic are unchanged.

### Spectrum storage sharing and lifecycle retention

The FFT now prepares conjugate pairs in the forward spectrum's allocation.
Each pair reads both source bins before overwriting them; the unpaired middle
bin is saved before clearing inactive bins. This removes the second spectrum
allocation without changing the filter, arithmetic, geometry or stream timing.

| Conversion | Previous retained Rust KiB | Current retained Rust KiB | Reduction |
| --- | ---: | ---: | ---: |
| 44.1→48 kHz, stereo | 97.38 | 88.16 | 9.5% |
| 48→44.1 kHz, stereo | 97.38 | 88.17 | 9.5% |
| 44.1→96 kHz, stereo | 152.26 | 143.03 | 6.1% |
| 96→44.1 kHz, stereo | 76.63 | 72.01 | 6.0% |
| 44.1→48 kHz, 6ch | 155.76 | 146.53 | 5.9% |

Both existing independent FFT oracles passed: **151,812 inverse outputs and
404,400 complete-block outputs, maximum absolute error 0**. These cover odd/even
geometries, up/downsampling, channel layouts, overlap and reset. The existing
streaming allocation test also passed. The previous 447-render-per-arm quality
supplement was not rerun for this same-filter storage change; Rubato samplewise
identity remains optional for future algorithms.

One fixed ABBA/BAAB schedule collected 80 valid >=250 ms trials on logical
core 2. Wall-time medians in ns/input scalar sample were:

| Conversion | Previous | Current | Observed change |
| --- | ---: | ---: | ---: |
| 44.1→48 kHz, stereo | 6.796 | 6.528 | −3.9% |
| 48→44.1 kHz, stereo | 7.522 | 6.125 | −18.6% |
| 44.1→96 kHz, stereo | 10.263 | 10.255 | −0.1% |
| 96→44.1 kHz, stereo | 4.229 | 4.203 | −0.6% |
| 44.1→48 kHz, 6ch | 7.134 | 7.344 | **+2.9%** |

These medians are retained observations, **not confirmed speed improvements**.
For example, the unchanged 48→44.1 baseline's two child processes measured
9.195–10.196 and 5.599–5.849 ns/sample. That drift is larger than the apparent
gain. The six-channel slowdown and CPU-time differences are retained as well.
No further timing retries were made. The production change is justified by
its deterministic memory reduction and unchanged filtering; its speed effect
needs a quieter measurement environment before a stronger claim. The complete
[paired data](../data/resampler-spectrum-reuse-2026-10-10.json) contains raw order,
cycles, thread CPU time, work checks, heap observations and executable hashes.

#### Bounded native memory retention and SoXR capacity

`--resource-retention-rounds=32` adds 32 process/finish/reset cycles on one
instance, followed by 32 create/process/finish/drop cycles. Each stream has
32,768 input frames and must produce its exact duration. Snapshots follow the
initial 32-instance pool warmup; observation storage is allocated before the
baseline and remains alive until all snapshots finish. This mode runs outside
steady timing.

**14 fresh child runs cover 12 recipes and 896 complete streams**, all valid,
at stereo 44.1→48 kHz. The recipes are owned Rust, raw Rubato, libsamplerate,
FFmpeg, SpeexDSP, r8brain, zita, WebRTC, WDL, libresample, project SoXR and raw
libsoxr. The two SoXR recipes each use both capacity policies.

Every recipe's busy-byte snapshots remained constant across its 32 reset
observations and across its 32 recreate/drop observations. After dropping an
instance, all returned to their **already-warmed** heap baseline. SpeexDSP's
private commit varied by one 4 KiB page during reset observations despite flat
busy bytes; heap blocks and process pages are different measurements.

r8brain still retains **1431.42 KiB for the process** after the initial pool is
released. It did not grow during the subsequent 64 streams. Its earlier
230.56 KiB pooled per-instance increment includes 44.73 KiB/instance from
that persistent process total; subtracting it leaves 185.83 KiB/instance.
This distinguishes releasable from persistent observed heap blocks; it does
not identify the blocks as a cache or prove leak freedom.

| Recipe | Common / advertised caller frames | Common / advertised warmed heap increment KiB/instance |
| --- | ---: | ---: |
| Project SoXR, High/Linear | 33,326 / 622 | 195.05 / 195.05 |
| Raw libsoxr, HQ/Bits20 | 33,326 / 1,070 | 195.03 / 195.03 |

SoXR heap increments are unchanged between these capacities. This closes the
previously unmeasured SoXR capacity comparison; neither capacity is asserted
to be minimum native workspace. [Raw lifecycle/capacity data](../data/resampler-retention-2026-10-10.json)
retain all snapshots, identities and exact stream counts. A bounded warmed
observation cannot establish long-duration leak freedom, exact native
allocation-call counts or temporal peaks.

### Time-buffer sharing and supplemental metrics

The forward FFT consumes the time-domain input before the inverse writes its
output, so both now use one allocation sized to the larger transform. Their
scratch storage is shared too. Mono/stereo/6-channel transfers have constant
strides; other layouts retain a generic path. Filter coefficients, transforms,
normalization, scheduling and public interfaces are unchanged.

One final ABBA/BAAB schedule contains **80 valid trials**, eight per arm per
configuration, each at least 250 ms, with 512-frame input and logical core 2 on
the i5-12500H Windows host. Baseline and candidate use identical measurement
tooling. Medians below are ns/input scalar sample; memory is exact retained
Rust allocator payload after construction, including the owned adapter.

| Conversion | Before | After | Wall reduction | Retained KiB before → after |
| --- | ---: | ---: | ---: | ---: |
| 44.1→48 kHz, stereo | 6.570 | 6.549 | 0.3% (essentially flat) | 106.62 → 97.38 |
| 48→44.1 kHz, stereo | 6.130 | 5.790 | 5.5% | 106.62 → 97.38 |
| 44.1→96 kHz, stereo | 10.360 | 9.871 | 4.7% | 161.49 → 152.26 |
| 96→44.1 kHz, stereo | 4.341 | 4.207 | 3.1% | 81.27 → 76.63 |
| 44.1→48 kHz, 6ch | 7.241 | 7.050 | 2.6% | 164.99 → 155.76 |

Cycle reductions are 0.05%, 5.83%, 4.51%, 3.08% and 2.39%, respectively.
These are observations from one machine/schedule, not significance estimates
or universal gains. Do not combine these timings with [the fused-inverse
table](#fused-inverse-transform-and-resource-results) to infer a cumulative
percentage. [Raw paired trials and binary hashes](../data/resampler-fft-layout-2026-10-10.json)
include CPU time, allocation counts, lifecycle snapshots and work validity.

The initial scratch-only storage revision saved just 24 bytes and had a 2.6%
44.1→96 slowdown. It was superseded by actual time-buffer reuse, with a new
implementation identity and paired schedule; its results are retained in the
task's `research/metrics-fft-layout-v1/initial-*` files. No further timing retries
were made. The full resampler suite passed 60 tests before this storage extension;
the final extension passed the two affected FFT oracles. The full-block oracle
covers 12 geometries × 4 channel layouts × 4 blocks, including overlap/reset:
**404,400 scalar outputs, maximum absolute error 0**. The inverse oracle also
retains its 151,812-output zero-error result. These are local same-filter checks;
Rubato samplewise identity is still not a requirement for new algorithms.

#### Added quality and small-block observations

The standalone `--diagnostics` mode collected **447 complete finite renders per
owned arm** across the five configurations above. The four-level tone results,
local group delay, centered-impulse metrics, dense response/alias points and
silence output are identical before/after. Two FFmpeg cases add 192 renders and
are retained controls. The report contains 1,086 renders total and 36 separate
call-tail passes; [raw diagnostics](../data/resampler-diagnostics-2026-10-10.json)
retain the exact frequencies, amplitudes, windows, frame counts and samples.

| Added metric | Measured scope / interpretation |
| --- | --- |
| Multi-level response | 997 Hz and 18 kHz at 0.999, 0.5, 0.001 and 0.000001 amplitude; gain, residual RMS/dBFS, THD+N and full-output peak |
| Local group delay | 1000/1001 and 18000/18001 Hz phase differences; seconds and output frames, principal-phase ambiguity ±0.5 s |
| Centered impulse | Peak/energy-centroid timing, pre/post-peak energy fractions, -100 dB relative-peak extents and record-edge flag |
| Dense finite bands | 49 transition frequencies from 18 kHz to shared Nyquist−1 Hz; downsampling additionally uses 33 stopband frequencies with two quadrature phases |
| Silence | Complete zero-input output peak; all measured cases produced zero |
| Small-block call tails | 64/128/512-frame callers, 2,048 measured calls per pass, p50/p95/p99/p99.9/max, raw durations and deadline counts |

These signal metrics excite all channels identically and analyze channel 0;
finite/complete checks cover all channels. Existing v7 channel-isolation and
image grids remain separate. Group delay here is the **residual output timeline
delay after adapter compensation**, approximately zero for the owned linear
recipes; it does not mean zero buffering or device latency. Fractional impulse
positions also make pre/post-peak energy asymmetric even for a linear-phase
filter. Analytic delay and known-FIR tests check the metric calculations.

For the final owned implementation, 64-frame p99 call times are **7.4 / 7.2 /
11.3 / 5.5 / 23.6 µs** in table order. None of its 30,720 measured calls exceeded
the corresponding source-buffer duration. Some buffered calls read **0 ns** at
the host timer's resolution; the report keeps those zeros instead of rejecting
them or replacing them with 1 ns. This is not device callback evidence.

The added data again shows why default FFmpeg is a different quality target:
at 96→44.1 kHz, worst output among the 33 quadrature stopband points is
**−168.31 dB** for owned Rust versus **−10.36 dB** for default SWR (the grid
starts only 1 Hz above output Nyquist). At 44.1→48 kHz, the first sampled
transition point below −0.1 dB is 21,289 Hz versus 19,265 Hz. These are finite
samples, not exact cutoff frequencies or continuous-band extrema. FFmpeg also
has much shorter impulse ringing: about 16/17 frames before/after the peak
versus 205/206 here at the stated threshold. Short SIMD polyphase filtering
does less work, with a different response/transient tradeoff. This update
preserves the owned response; it does not establish FFmpeg-speed parity.

#### Output-capacity memory comparison

`--resources --resource-memory-only --resource-output=adapter` now uses each
adapter's advertised output capacity. The default remains `common`. Twenty
fresh child runs cover **10 recipes × 2 capacities**, stereo 44.1→48 kHz,
512-frame input and 32 warmed instances. All completed. Values below are
Windows process heap busy-byte increments per instance, not native malloc
request payloads or peaks. Caller buffers themselves are excluded.

| Recipe | Common / advertised capacity, frames | Common / advertised heap increment, KiB |
| --- | ---: | ---: |
| Owned Rust | 33,326 / 622 | 97.63 / 97.63 |
| Raw Rubato | 33,326 / 1,280 | 251.21 / 251.21 |
| libsamplerate | 33,326 / 1,070 | 859.58 / 859.58 |
| FFmpeg SWR | 33,326 / 8,749 | **2,241.22 / 705.22** |
| SpeexDSP | 33,326 / 1,720 | 39.53 / 39.53 |
| r8brain | 33,326 / 1,024 | 230.56 / 230.56 |
| zita | 33,326 / 1,685 | 15.89 / 15.89 |
| WebRTC | 33,326 / 960 | 43.89 / 43.89 |
| WDL | 33,326 / 1,615 | 52.05 / 52.05 |
| libresample | 33,326 / 4,681 | 1,257.55 / 1,257.55 |

FFmpeg's heap increment falls **68.5%** solely from changing caller capacity;
this does not measure its minimum possible memory use. Other measured recipes
do not change at these two capacities. SoXR capacity sensitivity was not
collected in that round; [Bounded native memory retention and SoXR
capacity](#bounded-native-memory-retention-and-soxr-capacity) completes it.
[Raw capacity observations](../data/resampler-capacity-memory-2026-10-10.json)
also include commit/residency, lifecycle points and native identities.

The native shim wrappers now format progress-validation labels only on error.
All ten recipes recorded zero **Rust-side** process/finish/reset allocation
operations in these short runs. That removes measurement-adapter overhead;
it does not establish zero C/C++ allocations or production DSP acceleration.

### Native-inclusive heap snapshots (2026-10-10)

The missing native entries in the **Rust heap** column do not mean that native
engines have no measurable memory use. In addition to process commit/residency,
the resource runner now walks Windows process heaps outside processing/timing.
It records live busy-block bytes and block counts before construction, after
construction, after warmup and after releasing the instances. A failed or
unsupported walk returns `null`, never a fabricated zero or partial total.

One memory-only run covers **11 recipes at stereo 44.1→48 kHz**, with 512-frame
input, 32 warmed instances and the same caller capacity described above.
Each recipe runs in a fresh child process. All 11 completed; no throughput or
quality grid was rerun. The values below are warmed-minus-baseline busy bytes
divided by 32, in KiB. Raw lifecycle snapshots, block counts, engine identities,
commands and executable hash are in the
[native-inclusive heap data](../data/resampler-native-heap-2026-10-10.json).

| Recipe | Process heap increment KiB/instance |
| --- | ---: |
| Raw Rubato, FFT 1024/2 | 251.21 |
| Project SoXR, High/Linear | 195.05 |
| Raw libsoxr, HQ/Bits20 | 195.03 |
| FFmpeg SWR, default filter | 2241.22 |
| libsamplerate, SINC_BEST | 859.58 |
| SpeexDSP, quality 10 | 39.53 |
| r8brain 7.1 | 230.56 |
| zita-resampler 1.11.2 | 15.89 |
| WebRTC v1.3 shim | 43.89 |
| WDL 96b770f shim | 52.05 |
| libresample 0.1.5 shim | 1257.55 |

These are **process heap block increments**, including adapter/runtime effects
and allocator slabs. They are not library-attributed `malloc` request payloads,
allocation-call counts, or peaks between snapshots. Direct `VirtualAlloc`,
mapped data and allocations not represented by Windows heap entries remain
outside this column; process commit/residency remains the complementary view.
The large output capacity can still inflate native workspace requirements.
Do not add this column to the Rust heap column: the populations overlap.
The owned Rust recipe was not rerun in this native-memory addition.

After releasing the 32 instances, ten recipes returned to the baseline busy
byte count. r8brain retained about **1.40 MiB** above baseline for the entire
process. This one lifecycle observation does not distinguish shared cache
retention from a leak or establish a growth trend.

Exact native requested-byte retention, temporal peaks and allocation/free counts
would require a separate instrumented native build or heap-allocation trace.
Such instrumentation must cover the dependency DLLs and aligned allocations,
not just the C++ shim's `new`/`delete`. Allocation pointers need to be tracked
through `realloc` and free; the instrumented run must be separate from timing.
This additional instrumentation has not been implemented.

Owned Rust had **zero measured process/finish/reset heap operations** in the
[output-capacity run above](#output-capacity-memory-comparison). The seven
native-shim adapters then had temporary Rust-side allocations, which the
lazy-label change in that section removed. Rust counters do not intercept native
C/C++ `malloc`/`free` and cannot establish zero native allocations.

One of four longer FFmpeg 48→44.1 resource trials hit
`FFmpeg drain ended before the exact target duration`. Its steady CPU results
are retained, but complete output is unknown and it is not a successful
complete-stream check. The initial failed child is also retained. Other current
resource rows completed their duration checks.

### Fused inverse transform and resource results

Measured on Intel Core i5-12500H (16 logical processors), Windows/x86_64,
Rust 1.93.1. The fused pass uses disjoint contiguous slices and skips known-zero
bins. Coefficients, FFT sizes, retained frequency bins, normalization and stream
scheduling are unchanged.

Values below are median **nanoseconds per input scalar sample**; stereo has two
samples per frame. Each arm has eight trials of at least 250 ms in one ABBA or
BAAB schedule, using 512-frame caller blocks on logical processor 2. The gains
come from these paired measurements, not from comparison with the older v7 table.

| Conversion | Before fusion | After fusion | Wall-time reduction | Cycle reduction |
| --- | ---: | ---: | ---: | ---: |
| 44.1→48 kHz, stereo | 7.503 | 6.615 | 11.8% | 10.8% |
| 48→44.1 kHz, stereo | 7.110 | 6.151 | 13.5% | 14.5% |
| 44.1→96 kHz, stereo | 10.858 | 10.511 | 3.2% | 3.2% |
| 96→44.1 kHz, stereo | 4.684 | 4.447 | 5.1% | 5.2% |
| 44.1→48 kHz, 6ch | 7.923 | 7.493 | 5.4% | 5.9% |

An independent RealFFT comparison covered 15 odd/even and up/down geometries,
including all High/UltraHigh common-rate sizes: **151,812 scalar outputs,
maximum absolute error 0**. The existing resampler suite passed 59 tests. The
v7 quality grids below were not rerendered; this numerical comparison supports
the local fusion change. Future algorithms may differ sample by sample from
Rubato, provided objective quality does not regress.

The first indexed-loop candidate regressed in two cases and remains recorded.
The table reports the subsequent implementation revision, which removed those
indexed loops. These are machine-specific observations. Retained Rust heap
decreased by only 48 bytes per instance; the measured benefit is CPU time.

#### Cross-engine CPU and memory baseline

The resource report contains 57 current recipe/rate/channel configurations and
248 steady trials. It records thread user/kernel CPU time, cycles, wall time,
Rust retained/peak heap and allocation calls, and process memory snapshots.
The following table selects **stereo 44.1→48 kHz**. Full rate/channel results,
raw trials and executable identities are in the
[resource report](resources.md) and
[committed JSON](../data/resampler-resources-2026-10-10.json).

CPU budget is the fraction of **one logical CPU needed to keep up with real-time
audio**. It is not whole-machine utilization. CPU-time counters are quantized;
the final decimal should not be treated as exact. The heap and private-commit
columns in this baseline table belong to that same historical measurement set;
the current retention value is shown in [Current comparison](#current-comparison).

| Recipe | ns/sample | cycles/sample | One-core CPU budget | Rust retained / peak KiB | Private-commit increment KiB/instance |
| --- | ---: | ---: | ---: | ---: | ---: |
| Owned Rust | 6.615 | 20.38 | 0.0584% | 106.62 / 106.62 | 117.81 |
| Raw Rubato, FFT 1024/2 | 7.079 | 21.83 | 0.0623% | 250.96 / 250.96 | 280.12 |
| Project SoXR, High/Linear | 6.892 | 21.27 | 0.0614% | 0.02 / 0.02 (Rust only) | 284.75 |
| Raw libsoxr, HQ/Bits20 | 7.045 | 21.72 | 0.0619% | not captured (native) | 284.75 |
| FFmpeg SWR, default filter | 4.640 | 14.13 | 0.0405% | not captured (native) | 2245.38 |
| libsamplerate, SINC_BEST | 281.325 | 865.56 | 2.4971% | not captured (native) | 838.75 |
| SpeexDSP, quality 10 | 242.441 | 746.91 | 2.1029% | not captured (native) | 46.25 |
| r8brain 7.1 | 17.279 | 53.28 | 0.1530% | not captured (native) | 245.12 |
| zita-resampler 1.11.2 | 24.232 | 74.55 | 0.2103% | not captured (native) | 17.75 |
| WebRTC v1.3 shim | 6.255 | 19.29 | 0.0553% | not captured (native) | 52.25 |
| WDL 96b770f shim | 35.764 | 110.33 | 0.3213% | not captured (native) | 53.25 |
| libresample 0.1.5 shim | 25.493 | 78.52 | 0.2352% | not captured (native) | 1291.00 |

### Pre-fusion v7 quality and latency evidence

The two sequential v7 runs exited 0 and retained source, executable, native-library,
command and scheduling identities. An independent reconstruction made 30,803
integrity checks against the raw records and found no errors.

| Evidence | Owned-Rust build | SoXR build |
| --- | ---: | ---: |
| Measured recipes / rate-channel cases | 10 / 48 | 11 / 52 |
| Identified throughput trials | 720 | 780 |
| Individually timed process calls | 192,000 | 208,000 |
| Complete quality signal renders | 2,384 | 2,584 |
| Quality output scalar samples checked | 111,609,704 | 118,651,448 |
| Frequency-response points | 3,328 | 3,536 |
| Alias / image points | 500 / 304 | 550 / 320 |
| Independently excited channel pairs | 448 | 464 |
| Reset/render repetition rounds | 576 | 624 |

Raw libsoxr requires the `soxr` feature and is explicitly unavailable in the
Rust build. Raw libsoxr, FFmpeg and WebRTC adapters support the four stereo cases;
their current adapters do not support the six-channel case. This is adapter
coverage, not a general upstream-library limitation. Every measured v7 case passes
complete-work and finite-output checks. Cross-engine quality and speed are
**report-only**, not an assertion that different filters have equal quality.

The comparison covers High/Linear, 512-frame caller blocks and the five listed
rate/channel cases. It does not cover every rate, quality, phase, amplitude,
channel count or buffer size. A finite frequency grid does not establish a
continuous-band envelope, perceptual transparency or independent analyzer
accuracy. Other package probes and R5 research provide separately scoped
coverage; device/driver/DAC latency belongs to the consuming application.

### Pre-fusion v7 throughput

This table predates the fusion above. Cells are median **nanoseconds per input
scalar sample**. Stereo has two samples per frame. Each trial uses 32 warmup
buffers followed by 4,000 timed
process calls; each case has 15 alternating forward/reverse trials. All factories
use the same maximum advertised output capacity within a rate/channel case.
Input preparation, setup, reset, quality analysis and drain are outside steady
timing. Complete input and every drain output still participate in work checks.

| Recipe | 44.1→48 | 48→44.1 | 44.1→96 | 96→44.1 | 44.1→48, 6ch |
| --- | ---: | ---: | ---: | ---: | ---: |
| Owned Rust, High/Linear | 7.559 | 7.029 | 11.438 | 4.919 | 7.945 |
| Raw Rubato, FFT 1024/2 | 7.542 | 7.098 | 11.352 | 5.131 | 8.303 |
| Project SoXR, High/Linear | 6.897 | 6.563 | 9.572 | 3.878 | 8.023 |
| Raw libsoxr, HQ/Bits20 | 6.878 | 6.560 | 9.584 | 3.875 | — |
| FFmpeg SWR, default filter | 4.521 | 4.401 | 9.586 | 4.316 | — |
| libsamplerate, SINC_BEST | 288.589 | 265.508 | 572.578 | 260.944 | 134.844 |
| SpeexDSP, quality 10 | 240.667 | 221.354 | 481.231 | 216.193 | 240.696 |
| r8brain 7.1 | 17.377 | 17.192 | 22.838 | 10.712 | 17.359 |
| zita-resampler 1.11.2 | 24.147 | 22.808 | 44.359 | 21.816 | 20.179 |
| WebRTC v1.3 shim | 6.201 | 5.303 | 11.983 | 2.940 | — |
| WDL 96b770f shim | 35.779 | 30.229 | 71.436 | 15.219 | 32.198 |
| libresample 0.1.5 shim | 25.160 | 31.686 | 49.160 | 27.809 | 28.985 |

The project SoXR and raw-libsoxr rows come from one SoXR build; their median
differences are below 0.3% in all four stereo cases. The remaining rows come from
one Rust build. Comparing absolute Rust and SoXR rows across these two builds is
descriptive, not a paired optimization acceptance. Native f32 lanes
(libsamplerate, SpeexDSP, zita, WebRTC and libresample) and f64 lanes remain
explicit in the raw report and must not be treated as the same precision recipe.

The owned Rust and same-geometry raw Rubato quality grids are exactly equal in
all five v7 cases. Their pre-fusion common-rate throughput was close; the historical
R5 population speedup is a different measurement over 828 configurations.

The runs pin logical processor 2 and record affinity mask 4, process priority
class 128 and thread priority 2. `QueryThreadCycleTime` is retained alongside
wall time. Its counts are machine/scheduling-specific and are **not** a portable,
frequency-invariant performance unit.

## Why FFmpeg is fast

This measures in-process `libswresample`, not an FFmpeg command line, decoder,
file transfer or process startup. The pinned n8.0.1 source is
`894da5ca7d742e4429ffb2af534fcda0103ef593`. The shim uses packed f64 I/O and SWR's
internal planar f64 path. The build enables x86 assembly, with AVX/FMA3 available
on this host. SWR prepares a rational polyphase coefficient bank during setup
and performs short, vectorized dot products during conversion.

The selected SWR defaults are filter size 32, phase shift 10, exact rational
phases, cutoff 0.97 and Kaiser beta 9. Across the four stereo conversions, the
actual tap counts are **32 / 36 / 32 / 72**, and phase counts are
**160 / 147 / 320 / 147**. Coefficients are reused, the hot work is bounded and
regular, and no large transform is needed for each output block.

The owned High path instead uses real FFT pairs of lengths
**1176/1280, 1280/1176, 1176/2560 and 1280/588**. RustFFT already selects SIMD;
this is not an interpreted-code or missing-vectorization comparison. The long
filter and narrow transition require substantially different work.

The quality difference is visible in the v7 measurements:

| 48→44.1 kHz metric | Owned Rust High | FFmpeg selected default |
| --- | ---: | ---: |
| Gain at 18 kHz | approximately 0 dB | −0.00961 dB |
| Gain at 19,845 Hz | approximately 0 dB | −1.02350 dB |
| Gain at 20,947 Hz | −0.000000094 dB | −3.95940 dB |
| Output level from 23 kHz input, quadrature pair | −185.34 dB | **−19.93 dB** |
| 997 Hz tone-fit residual / signal | −209.51 dB | −108.68 dB |

The 96→44.1 kHz 23 kHz probe is −179.89 dB for owned Rust and −19.93 dB for
FFmpeg. These are particular measured points, not whole-band rejection bounds.
The old −121.7 dB FFmpeg reverse-alias entry was a miscopied distortion metric.
Increasing SWR filter quality changes its cost; default-filter speed does not
establish speed at the project's current response.

## Can the owned backend reach that speed without losing quality?

The fusion improves all five measured common configurations while retaining
the owned filter. Matching the selected FFmpeg default across all four stereo
conversions at that response remains unproven. For example, the fused-inverse resource
rows measured 6.615 ns/sample for owned Rust and 4.640 for FFmpeg at 44.1→48 kHz;
they are not a fresh speed ratio for the current implementation.
Cross-engine timings describe the selected recipes, not equal-quality acceptance.

### Direct linear polyphase screen

The next proposed large change was a direct linear polyphase FIR built around
the current cutoff. A bounded design screen rejects the short-kernel version
before any production rewrite: preserving a conservative −180 dB near-stopband
floor needs 96 taps/phase at 44.1→48 and 48→44.1, and 192 taps/phase at
96→44.1. Those candidates require about 88–105 multiply-accumulate pairs per
input frame and retain 110–220 KiB of coefficients. A 32-tap 44.1→48 kernel
reaches only −50.7 dB in the same screen. These are response and cost
estimates, not streaming acceptance or speed claims; the reproducible rows are
in [the machine-readable screen](../data/resampler-polyphase-screen-2026-10-10.json).
The supporting research note is local to the development machine and is
deliberately not distributed with this repository.

The existing R3 study independently found 0/72 passing multistage development
candidates and retained streaming/pointwise tradeoffs for direct and
equiripple designs. Consequently the current `ContiguousPolyphaseResampler`
must not be routed to `Linear`, and the FFT filter must not be shortened. A
future multistage replacement remains possible, but it needs a new complete
response and lifecycle identity before it can be considered a production
optimization.

The following gaps are **historical, calculated from pre-fusion v7**. Use the
paired table above for the fusion's actual gains; these percentages are not the
remaining reductions needed by the optimized implementation.

| Stereo conversion | Owned Rust / FFmpeg time | Reduction needed to match FFmpeg |
| --- | ---: | ---: |
| 44k1 → 48k | 1.672× | 40.2% |
| 48k → 44k1 | 1.597× | 37.4% |
| 44k1 → 96k | 1.193× | 16.2% |
| 96k → 44k1 | 1.140× | 12.3% |

In the v7 table, SoXR High approaches or beats the selected FFmpeg recipe in two
cases, but its bandwidth and residuals differ from owned Rust High;
selecting it is not proof of quality nonregression against the owned backend.

Before the fusion, Windows CPU sampling was rejected by local profiling policy
(`0xc5585011`). A separate, sealed source-instrumented copy therefore measured
the pre-fusion FFT path for four seconds per stereo case, outside all throughput
trials:

| Work inside process | Fraction of instrumented process time |
| --- | ---: |
| Forward and inverse FFT together | 67.4–70.8% |
| Spectrum multiplication and zero fill | 7.4–10.5% |
| Input deinterleave/zero and output overlap/interleave | 12.6–13.0% |
| Other work, including instrumentation overhead | 7.9–11.0% |

The timings include timer overhead; 10,000 timer-read observations per case are
retained. These are pre-fusion diagnostics, not a profile of the optimized
implementation. The fused pass now combines spectrum multiplication and inverse
FFT preparation, reducing work while retaining the production filter and
geometry. No post-fusion stage profile was collected. Faster FFT work or a
different block/filter architecture remain possible ways to close the remaining
gap; the data does not establish their achievable speed at the owned quality.

Further optimization should distinguish three targets. The production FFT still
zero-pads half of each forward input and gathers/scatters individual channels;
specialized transforms or layouts are hypotheses requiring a useful measured
gain. Prior decimation experiments already tried selective spectrum rebuilding,
so repeating that work unchanged is not a new optimization. A fixed-ratio
polyphase or multistage design may change the filter and reduce buffering, but
must satisfy the quality criteria below. Separately, native benchmark adapters
currently allocate formatted validation labels on successful calls; removing
that harness overhead would improve measurement fidelity, not the owned DSP.

Future replacements **do not require sample-by-sample identity with Rubato**.
Objective quality must not regress: comparisons must cover passband error,
usable bandwidth, alias/image response, phase/timeline, full-stream transients
and numerical resolution against the current implementation. A quality win at
one frequency, a broad audibility threshold, or a faster default preset cannot replace nonregression
across those criteria. Streaming duration, startup, allocations and process
tails remain separate acceptance requirements.

## v7 metric definitions

`steady_ns_per_input_sample.p95` is the p95 of 15 trial averages; it is not a callback tail.
`process_call_latency` comes from a separate instrumented pass and retains every
duration and output count, p50/p95/p99/p99.9/max, timer observations and exact
complete output. It includes timer overhead and excludes setup/reset/drain and
device scheduling. For example, FFmpeg's 48→44.1 kHz median throughput barely
changes between the two runs, while observed individual-call maxima differ
from 52.2 to 778.2 microseconds. Stable throughput is not a hard deadline guarantee.

Startup distinguishes the API's compensated buffering report, the cursor before
the producing call, input consumed through that call, and whether first output
occurred during drain. At 512-frame calls, owned Rust first produces output by
the call that brings total consumed input to 1,024 frames; FFmpeg does so by
512. SoXR reaches 1,024 or 2,048 depending on the conversion. These are caller-
block observations, not exact internal lookahead or device/group-delay values.

The quality grid measures 13 response frequencies for each input quadrature
phase and output channel: 20, 100, 997, 5k, 10k, 15k, 18k Hz plus 85/90/95/98/99%
of common Nyquist and Nyquist−1 Hz. Both downsampling directions include a
stopband grid, exact output/input Nyquist, and legal exact/near-DC folds.
Upsampling image fits jointly model the fundamental and image. Channel
isolation excites each source channel independently. DC and both step polarities
are checked, including complete-stream peaks and ending transients.

Tone generation uses bounded integer-Hz rational phase, and fitting uses
compensated sums. The analysis guard is capped at one quarter of usable output;
the old 96→44.1 kHz window could shrink to just 32 frames. Each point retains its
window and residual metadata. Analytic/quantized analyzer self-controls are not
an external accuracy bound. The −400 dB display floor is not analyzer resolution.

The optional `--soak-rounds 12` check uses a persistent adapter per case, reset
between rounds. Every round processes 32 warmup plus 64 regular input calls and
all drain chunks. Actual sample-bit vectors are compared; digests accompany,
rather than replace, the comparison. The two repetition phases lasted only
7.19 and 7.34 seconds in total. Working set is report-only; this is not a
long-duration leak test or complete accounting of native C/C++ allocations.

## Integrity and reproduction

Every v7 trial carries engine, algorithm, sample format, rate pair, channel count,
input chunk, output capacity and trial index. Accumulators use stable factory
IDs even when execution order reverses. Missing, duplicate, wrong-identity and
inconsistent records fail. Published statistics and work counters are rebuilt
from raw records; `valid: true` cannot override corruption. JSON reload uses
`serde_json/float_roundtrip` and exact floating-bit validation. This subsequent
reader fix changes no DSP arithmetic or timed conversion work in the retained
v7 runs; its own regression and integration checks are recorded separately.
Later feature-matrix checks also repaired missing `raw-rubato` guards in adapter
dispatch arms; both heavy builds already enabled that feature.

The ignored seven-shim fixture in the SoXR-linked test harness completed its
assertions but stalled during Windows process teardown. Its incomplete exits
remain recorded separately from the normally exiting heavy main executables
and ordinary tests. The specific DLL/runtime cause has not been established.

libsamplerate receives end-of-input on the final call containing real samples.
The v4 adapter uses the pinned 0.2.2 source's floating clock and native endpoint
rule, restricted to the supported 2/6-channel matrix. Ceil, rounding and a
one-frame tolerance do not reproduce that rule. Native observations cover 5,416
stereo/six-channel streams; the tested mono model disagreed once and was not
generalized. Nothing is padded or trimmed to manufacture matching lengths.

Build native controls with `benches/native/build_resampler_shims.ps1`. Pass the
libsamplerate DLL through `--libsamplerate <absolute-path>` and each shim through
`--engine-library <engine-id>=<absolute-path>`. Libraries are not discovered by
searching PATH. Record source/build/runtime artifacts and hashes from each run.

The quality/latency comparison mode used for v7 is:

```text
cargo bench --bench audio_resampler_comparison_perf --features raw-rubato --   --heavy --enforce --pinned --pin-core 2 --soak-rounds 12   --out target/bench-reports/rust-new.json <explicit-native-arguments>

cargo bench --bench audio_resampler_comparison_perf --all-features --   --heavy --enforce --pinned --pin-core 2 --soak-rounds 12   --require-complete-matrix   --out target/bench-reports/soxr-new.json <explicit-native-arguments>
```

Run feature builds and timing sequentially. Use unique evidence paths and retain
failures. The all-feature command requires every declared engine; the Rust build
can require each of its ten expected engines explicitly. Optional
`--raw-rubato-geometry 512/1` is a different control recipe, not the High/1024/2
reference used above. There is no claim of native ARM/Linux runtime performance.

For CPU/memory reporting without quality renders:

```text
cargo bench -p audio-engine-core --bench audio_resampler_comparison_perf --features raw-rubato -- --resources --pinned --pin-core 2 --out target/bench-reports/resources.json
```

Add `--resource-memory-only` to skip steady timing entirely while retaining
construction/warmup/release snapshots and the short allocation/lifecycle pass.
The extended resource schema is `resampler_resources_v2`; historical v1 reports
are retained as measured and have no newly inferred heap fields.

Add the same explicit native-library arguments for external engines, or use
`--all-features` to select project SoXR and raw libsoxr. Filter recipes with
`--resource-engines=audio_engine_core,ffmpeg_libswresample` and set the minimum
timing window with `--resource-ms=250`. Cases run in separate child processes;
allocation instrumentation is disabled during timing. Resource mode is
report-only and rejects `--enforce` and `--baseline`.

The spectrum-storage and retention evidence is retained in the local research
workspace (deliberately not distributed), alongside committed
[paired timing](../data/resampler-spectrum-reuse-2026-10-10.json) and
[lifecycle snapshots](../data/resampler-retention-2026-10-10.json). Use
`--resource-memory-only --resource-retention-rounds=32` for bounded lifecycle
observations, and `--resource-output=common|adapter` to select caller capacity.
The time-buffer and supplemental quality evidence was captured in the same local
workspace.

The fused-inverse and resource evidence was captured in the same local research
workspace. The [committed resource data](../data/resampler-resources-2026-10-10.json)
includes raw trials, executable identities and endpoint failures.

Pre-fusion v7 evidence was captured in the same local research workspace:

- `results/{rust,soxr}-heavy-v7.json`: all measurements and raw records.
- `results/integrity-audit-v1.json`: reconstructed statistics and denominators.
- `binaries/{rust,soxr}/freeze.json`: exact commands, source/native/binary hashes.
- `profile/instrumented-v2/measurement/`: diagnostic stages, raw counters and scope.
- `runs/`: successful and failed command requests, logs and actual exit codes.

Rust executable SHA-256:
`8812e1ab8e58e15a66f32a393ff3a8c348bc2722eedf4789589d7d748a3ef858`.
SoXR executable SHA-256:
`037eb6c51de08b58102f82570c6a9b36a80c156265b65addd1038c0c4eeea088`.
