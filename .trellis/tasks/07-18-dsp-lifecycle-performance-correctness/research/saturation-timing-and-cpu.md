# Oversampled Saturation Timing and CPU Options

## Confirmed behavior

* The 2x FIR has 17 symmetric taps: group delay `(17 - 1) / 2 / 2 = 4`
  source frames.
* The 4x FIR has 33 symmetric taps: group delay `(33 - 1) / 2 / 4 = 4`
  source frames.
* The current wet path is FIR-filtered and mixed with the current dry sample.
  This creates phase cancellation at partial mix.
* Below threshold, the nonlinear transfer returns its input, but the
  oversampled path still filters that input. Therefore the documented identity
  transfer does not hold after the oversampling state has warmed up.
* Initialization fills the complete FIR history with the first wet sample and
  bypasses filtering for that sample. This suppresses startup delay but makes
  the path time-varying and prevents a consistent impulse/timing contract.

## Current FIR cost

`process_oversampled_value` evaluates the complete FIR at every interpolated
phase even though only the final phase output is retained:

* 2x: `2 * 17 = 34` FIR MACs per source sample.
* 4x: `4 * 33 = 132` FIR MACs per source sample.

The history must be updated at every phase, but the dot product only needs to
be evaluated once per decimated source output. Splitting `push(sample)` from
`evaluate()` reduces this to 17 and 33 MACs respectively: a theoretical 50%
and 75% reduction in FIR MACs, before accounting for waveshaper and surrounding
stage costs. Wall-clock improvement must be measured separately.

## Design A: Delay dry and keep filtering the complete wet signal

```text
output = delayed_dry + mix * (filtered_wet - delayed_dry)
```

* Corrects dry/wet phase alignment.
* Keeps the current wet frequency response.
* Below-threshold output is still the decimation filter's approximation rather
  than exact delayed identity, especially at full wet.
* Adds a four-sample delay ring per channel (32 bytes/channel plus index/state).
* FIR CPU is unchanged unless combined with push/evaluate-once.

## Design B: Filter both dry and wet paths

```text
output = filtered_dry + mix * (filtered_wet - filtered_dry)
```

* Gives exact dry/wet phase and magnitude matching.
* Always colors the nominal dry signal with the oversampling reconstruction
  filter.
* Roughly doubles FIR dot-product work and filter-history memory.
* It is dominated by Design C for this thresholded effect and is not
  recommended for a CPU-sensitive callback.

## Design C: Filter only the nonlinear delta (recommended)

For each oversampled phase:

```text
delta_os = shaped(sample_os) - sample_os
delta = decimate_lowpass(delta_os)
output = delayed_dry + mix * delta
```

For high-pass exciter mode, form the nonlinear delta from the high-pass branch
and add the filtered delta to the delayed full-band input.

Benefits:

* Below threshold, `delta_os` is exactly zero, so output is exact delayed dry
  independent of FIR passband ripple.
* Partial mix cannot comb-filter the linear program component.
* The FIR processes only distortion products that require antialias filtering.
* It reuses the existing 33-sample maximum history; persistent memory only adds
  the small dry-delay ring.
* Combined with push/evaluate-once, active 4x cost falls from 132 to 33 FIR MACs
  per source sample.
* When delta and FIR history are both exactly inactive, a bounded activity
  counter can skip the dot product entirely. Any sparse fast path must be
  bit-stable across chunk sizes and must not discard a pending FIR tail.

Risks:

* It intentionally changes the oversampled transfer from filtering the complete
  wet signal to filtering the nonlinear residual. Independent quality oracles
  must validate fundamental gain, harmonic spectrum, and alias reduction.
* Existing FIR coefficients may need revalidation for delta bandwidth and
  stopband goals.

## Latency policy options

### Mode-dependent latency

* Direct reports zero; 2x/4x report four source frames.
* Lowest latency for Direct and disabled modes.
* Runtime quality changes alter stage latency and require an explicit reset,
  crossfade, or host reconfiguration contract to avoid a time jump.

### Fixed maximum latency while the stage is active

* Direct, 2x, and 4x all use a four-frame dry delay.
* Quality automation does not shift the timeline.
* Adds one ring load/store per sample in Direct mode and 0.083 ms at 48 kHz.
* Callback chains already carry a much larger limiter lookahead, so the added
  end-to-end delay is usually negligible, but standalone processor users will
  observe the change.

Recommendation: prefer fixed four-frame active-stage latency if runtime quality
switching is supported as ordinary parameter automation. Prefer mode-dependent
latency only if quality is explicitly setup-only or a quality switch resets the
stream.

## Non-CPU and non-memory impacts

### Timeline and parallel-path phase

Mode-dependent latency changes the stage and chain timeline between Direct and
oversampled modes. Offline compensation can remove the difference exactly, but
a realtime host or parallel bus must know the active latency. Fixed latency
keeps enabled quality modes aligned and avoids a four-frame shift between
parallel routes. Four frames are about 0.083 ms at 48 kHz and 0.091 ms at
44.1 kHz: usually not perceived as delay, but enough to change phase when mixed
with an undelayed copy.

### Runtime parameter semantics

Setup/reset-only quality makes the requested value pending until a new stream.
This is deterministic and click-free, but UI/control telemetry must distinguish
requested from applied quality or callers may believe a change happened
immediately. Fixed latency permits immediate timing-safe changes, but changing
FIR geometry still needs a state transition; constant latency alone does not
prevent a reset transient. A dual-path transition gives immediate response but
must define fade length and applied-state reporting.

The current adapter reads `AtomicSaturationParams` once before processing a
callback block. That mechanism cannot identify an arbitrary frame offset inside
the block. Calling this sample-accurate would be incorrect unless processing
also receives offset events or consumes them from a preallocated audio-thread
queue.

### Sample-accurate event transport options

1. **Borrowed per-block event slice**: the caller passes a sorted bounded slice
   of `{ frame_offset, quality }` events with the audio block. The callback
   performs no allocation or atomic queue traffic, and event cost is paid only
   when events exist. This is the lowest CPU/memory true sample-accurate option,
   but requires the host/caller to schedule offsets.
2. **Preallocated SPSC event queue**: control writes absolute-frame events and
   audio consumes them. It supports asynchronous UI/control producers, but
   needs a stream timebase, bounded overflow policy, atomics, and fixed ring
   memory. Late events need explicit handling.
3. **Expanded atomic snapshot**: retain one load per callback and apply at frame
   zero. This is block-accurate, not sample-accurate, and does not meet the
   selected requirement unless that requirement is relaxed.

### Switching sound

* Setup/reset-only: no mid-program transition sound; the complete next stream
  uses one stable algorithm.
* Fixed latency with a direct state reset: no timeline jump, but FIR history can
  create a short startup/settling transient unless histories are migrated or
  paths are crossfaded.
* Dual-path crossfade: usually the smoothest automation, but transition samples
  are a blend rather than either exact quality mode. A poorly aligned fade can
  introduce short combing or transient smearing.

### Host latency reporting

Most hosts cannot change plugin delay compensation sample-by-sample. A runtime
crossfade between zero- and four-frame paths therefore normally reports the
maximum latency and keeps a delay active, which converges operationally toward
the fixed-latency design. Otherwise the host must rebuild/re-align its graph at
the quality switch.

### End-of-stream and output length

Direct zero-latency mode needs no delay drain. A fixed four-frame Direct path
must expose those final four delayed frames through finish; stopping at input
EOS would otherwise truncate program audio. Oversampled modes require measured
FIR/interpolation drain under every policy. A dual-path switch near EOS must
finish both active transition branches before it can become terminal.

### Public compatibility

Mode-dependent latency preserves the existing zero-delay Direct transfer.
Fixed latency changes standalone Direct timing and requires callers to honor
finish even when no nonlinear oversampling is selected. Disabled saturation is
currently specified as bit-exact zero-latency bypass, so enabled/disabled
switches still change timing unless that public bypass contract is deliberately
redefined.

### Determinism and verification risk

Setup/reset-only has the smallest state machine and strongest chunk-independent
reproducibility. Fixed latency adds simple bounded delay state. Dual-path
transition adds two algorithm states, fade progress, EOS interaction, and more
block-boundary cases; it has the highest risk of a bug that only appears for a
specific automation position or block size.

## Finish and timing accounting

The corrected implementation must derive raw drain, algorithmic latency, and
semantic tail from an impulse oracle rather than assuming the FIR alone defines
the complete support. Linear interpolation contributes state across the next
source interval. Required assertions for every quality/mode combination:

* measured first/peak impulse position agrees with `latency()`;
* raw finish emits the exact remaining non-zero support;
* `latency + finite_tail` bounds total finalize frames;
* compensated and raw output match after one timeline shift;
* finish is allocation-free and terminal-idempotent;
* random chunking is equivalent to one-shot processing.

## Memory model

Current maximum FIR history is 264 bytes/channel. A four-frame `f64` dry delay
adds 32 bytes/channel plus a small index. For eight channels, the additional
sample storage is 256 bytes. This is preferable to a second 33-tap matched FIR,
which adds another 2,112 bytes for eight channels and doubles active FIR work.
