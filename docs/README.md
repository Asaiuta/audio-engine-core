# Documentation

Reference material for `audio-engine-core`. Every figure listed here is
machine-specific benchmark evidence from the recording runs, not a universal
performance or quality guarantee.

## Analysis and quality

* [`quality.md`](quality.md) — evidence model and summary, benchmark
  methodology, quality validation, realtime safety, performance, behavioral
  contracts and the regression/baseline policy.
* [`audio-algorithm-research.md`](audio-algorithm-research.md) — literature
  cross-reference for candidate algorithm changes (long-IR partitioned
  convolution, ADAA saturation, resampling, pitch/beat). It records a reading
  pass only; it adds no benchmark, quality test or production change.

## Resampler

The project-owned Rust backend is named **CoreSinc**; its stable machine
identifier remains `rust`, so existing feature selection and historical
measurements stay compatible.

* [`resampler/comparison.md`](resampler/comparison.md) — cross-engine quality and
  performance comparison, filter-response and lifecycle results, the direct
  polyphase screen, and the metric-coverage verdict.
* [`resampler/resources.md`](resampler/resources.md) — CPU, memory and FFT
  resource results.
* [`resampler/replacement.md`](resampler/replacement.md) — the replacement
  contract, recorded evidence and acceptance SHA-256 values.

## Convolution

* [`convolver-performance.md`](convolver-performance.md) — nonuniform partitioned
  convolution for long stereo impulse responses: the accepted routing and
  scheduling rules, the measurement scope, the rejected research
  configurations, and the CPU/memory trade-off. The late tier costs about
  766 KiB of extra retained heap per instance and only applies at or above
  65536 taps per channel on stereo.

## Setup

* [`installation.md`](installation.md) — native SoXR backend setup per platform.

## Machine-readable data

* [`data/`](data/) — frozen JSON records that back the tables above: raw trials,
  frequency/alias/image/channel probes, heap snapshots, hashes and scheduling
  identities. These files are evidence and are retained byte-for-byte; each
  document above names the run it draws from.
