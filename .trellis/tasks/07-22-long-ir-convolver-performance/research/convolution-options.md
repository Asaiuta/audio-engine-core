# Long IR convolution options

## Scope and current path

The current partitioned engine uses a 1024-frame head and uniform tail. Each
channel stores `Vec<Vec<Complex<f64>>>` IR spectra and input-history spectra.
At each partition boundary it zeroes a full complex accumulator, multiplies
and accumulates every tail partition, performs one inverse FFT, and emits the
overlap-save block. A forward FFT is performed when each input partition is
committed.

## Options

### A. Half-spectrum real FFT (recommended first prototype)

Real audio and real IRs have conjugate-symmetric spectra. A real FFT stores
only DC through Nyquist and can reduce spectrum MACs and history/IR memory by
roughly half. RustFFT's `realfft` crate is already resolved in this repository's
dependency graph. The implementation must preserve the DC/Nyquist treatment,
inverse normalization, overlap-save tail, and f64 arithmetic.

Risk: the current direct complex path is the numerical oracle. The prototype
must compare output samples and long-tail energy against it, and must prove no
allocation after construction.

### B. Fixed partition-size sweep

Evaluate 512, 1024, and 2048 partitions across 64/128/256/512 callback blocks
and 8192+ IRs. Smaller partitions reduce per-boundary burst size but increase
FFT frequency and partition count; larger partitions reduce tail MACs but make
the zero-latency head overlap-save FFT larger on every callback. This is a
workload policy decision, not a universally faster constant.

### C. Non-uniform partitioning

Keep a short head partition for callback latency and use larger partitions for
the distant tail (for example 256/1024/4096). This is the likely long-term
throughput/latency compromise, but it requires multiple history clocks and
more difficult exact-tail/reset tests. It should follow the benchmark sweep,
not precede it.

### D. Spectrum-layout and loop cleanup

Flatten channel/partition/bin storage and split the circular history walk at
the cursor to remove nested vector indirection and the partition-loop modulo.
This is lower risk and can be combined with A, but it is unlikely to match the
gain from eliminating the redundant half-spectrum work.

## Required measurements

* Throughput: process-into, in-place, and allocating wrapper, in ns/sample.
* Callback distribution: per-buffer median, p95, p99, maximum, and deadline
  utilization for 64/128/256/512 frames.
* Correctness: direct convolution oracle, reset/finish/tail, irregular chunks,
  mono/stereo, and no-allocation checks.
* Regression: FIR EQ apply and full callback chain on the same machine.

## Recommendation

Start with the expanded benchmark and a real-FFT prototype. Accept a
partition-policy change only when it improves both steady-state long-IR cost
and callback burst metrics, with no short/medium regression beyond the existing
quality/performance gates.
