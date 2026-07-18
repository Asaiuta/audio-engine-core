# CPU, Memory, and Audio-Correctness Measurement Plan

## Principle

Do not call an implementation fastest or smallest from operation counts alone.
Record compatible before/after reports on the same compiler, target, CPU,
profile, features, mode, conditions, and case set. Keep raw trials and use
median plus nearest-rank p95.

## Existing evidence to preserve

* `cargo test --lib`: 293 tests currently pass.
* `audio_quality_measurements --quick --enforce`: 25/25 gates pass with no
  skipped EBU checks.
* Current documented 512-frame callback evidence is about 116.9 ns/sample
  without Convolver and 124.4 ns/sample with a 256-tap Convolver, though a new
  same-environment baseline must be captured before implementation.
* The existing callback report covers 64/128/256/512 frames, seven quick
  trials, median/p95/max, and deadline utilization.

## New saturation microbench cases

Measure mono, stereo, and 8-channel blocks for Direct, 2x, and 4x:

* below-threshold delta-inactive signal;
* continuously driven nonlinear signal;
* burst followed by zero input to exercise pending FIR state;
* high-pass exciter active;
* 64/128/256/512 frame blocks.

Report ns/input-sample, ns/buffer, raw trials, and complete-work checks. Keep
the callback-chain scenarios to capture whole-graph effects.

Expected algorithmic change, not an acceptance claim:

* evaluate-once reduces decimation FIR MACs from 34 to 17 at 2x;
* evaluate-once reduces decimation FIR MACs from 132 to 33 at 4x;
* sparse nonlinear-delta bypass may further reduce below-threshold work.

## New offline render benchmark

Add quick/full cases that exercise:

* equal-rate and 44.1 -> 48 kHz renders;
* transparent, active IIR, active saturation 4x, Convolver tail, and complete
  active chain;
* short blocks for scheduler overhead and 60-second streams for memory/cache
  behavior;
* finite tail and unknown decay that stops well before the safety cap.

Report:

* ns/input-sample and real-time factor;
* source/output/rendered frames and checksum/work validity;
* number of Rust allocations;
* peak Rust allocated bytes;
* final output capacity bytes separately from transient working bytes;
* configured block/pool bytes and native SoXR working bytes when available.

A bench-local counting allocator may measure memory in a separate, untimed
trial. Timing trials must not include allocator instrumentation overhead unless
the baseline uses the identical instrumentation.

## Selected performance budgets for convergence

The strict practical budget was selected. Net improvement is preferred, while
the hard limits below keep correctness work from hiding a material regression:

* No allocation/deallocation in callback `process` or callback-facing `finish`
  after setup, including first use on a new audio OS thread.
* No compatible 512-frame active-chain median regression greater than 3%; the
  project-wide hard fallback remains 10%, but this task targets improvement.
* No p95 deadline-utilization regression greater than 5% relative; retain a
  large absolute margin below one callback period.
* Saturation 4x isolated active median CPU must improve after evaluate-once. If
  it does not, investigate compiler/vectorization and benchmark work validity
  rather than claiming the operation-count reduction as a performance result.
* Seek net improvement in compatible callback-chain and long-render cases. A
  within-budget result that does not improve must retain raw evidence and name
  the dominant remaining cost; it is not reported as a speedup.
* Extra saturation persistent sample storage should be limited to one
  four-frame dry-delay ring per channel; avoid a second 33-tap FIR history.
* Block-streamed offline transient Rust memory should be bounded by final output
  plus a documented fixed number of block buffers, not by stage count or input
  duration.
* Unknown-tail generated frames must end at the threshold/hold point, not the
  maximum cap, unless energy actually persists.

## Audio oracles

### Saturation

* Below-threshold output equals the selected latency-shifted dry reference.
* Partial mix equals `delayed_dry + mix * filtered_nonlinear_delta`.
* Impulse peak/support agrees with latency/tail metadata.
* Threshold C1 continuity and alias-reduction gates remain green.
* Report fundamental gain and harmonic levels so alias improvement cannot hide
  excessive wanted-signal attenuation.

### IIR tails

* Last-frame impulse through active EQ, Crossfeed, and DynamicLoudness produces
  a non-empty finish response.
* One-shot and irregular finish block sizes retain equivalent samples and stop
  at equivalent frames.
* An independent zero-input direct processor oracle matches adapter/chain tail.
* Persistent-energy test reaches the exact safety cap and sets truncation.

### Chain composition

* Callback and offline fixed-stage intersections remain equivalent before
  render-only rate/output transforms.
* Each upstream tail passes through downstream limiter/resampler/noise shaper.
* Raw and compensated timelines differ only by the reported accumulated
  latency shift.
* Final Meter reads final quantized rendered samples and never transforms them.

### Sample-rate and geometry

* A processed Convolver followed by a rate change and zero/new-stream input
  matches a fresh reset reference.
* Invalid interleaved IR/audio geometry returns the selected explicit error and
  never differs between in-place and out-of-place paths.

## Full-output true peak decision

The source-rate limiter currently does not guarantee the final post-resample,
post-quantize ceiling. If this task adopts final `-1 dBTP` conformance, add a
separate output-rate limiter architecture comparison and quality gate. Do not
silently convert the current report-only metric into a gate without changing
the stage topology and accounting for its extra CPU/latency.
