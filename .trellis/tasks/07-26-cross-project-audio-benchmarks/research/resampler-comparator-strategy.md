# Resampler Comparator Strategy

> Superseded scope note (2026-07-26): the two-increment recommendation below
> was useful for validating the harness, but it is not the task completion
> boundary. See `../prd.md` and `native-comparator-provisioning.md` for the
> required 11-project matrix.

## Question

How should this repository add credible cross-project resampler benchmarks
without changing production behavior or making normal builds depend on every
native comparison library?

## Current Repository Constraints

* The production streaming API is interleaved `f64`, accepts arbitrary input
  chunks, reports consumed/produced frames, exposes latency/tail semantics, and
  requires bounded allocation-free processing after setup.
* `audio_resampler_streaming_perf` and `audio_resampler_matrix_perf` already
  provide workload identities, warmup/trial distributions, environment
  metadata, JSON persistence, baseline compatibility, and work-evidence gates.
* The default build selects SoXR. A Rubato-only build selects the project's
  custom staging/routing layer; enabling both still selects SoXR for the public
  API.
* SoXR 0.6 exposes raw mono, stereo, and const-channel interleaved `f32`/`f64`
  streams with `process`, `drain`, and `clear`.
* Rubato 4 exposes raw interleaved allocation-free processing, reset, explicit
  input/output sizing, and output delay. Its synchronous FFT engine is the
  appropriate upstream control for fixed 44.1/48 kHz ratios.
* The local Cargo cache already contains `libloading` 0.8.9, so optional native
  DLL/SO loading can be added without a compile-time link dependency.
* This Windows machine currently has no discoverable `samplerate`, `speexdsp`,
  or `libswresample` pkg-config package and no `ffmpeg` executable.
* Existing benchmark-coverage work already modifies `Cargo.toml` and
  `benches/support/mod.rs`; this task must use narrow hunks and preferably new
  support files to keep commits separable.

## Comparable Implementations

### Raw libsoxr

Use the same upstream crate as the default backend but instantiate one native
interleaved multichannel stream directly. This isolates the cost of the
project's per-channel wrapper, pacing, alignment, and lifecycle contract. It is
the lowest-risk control and supports the crate's native `f64` format.

### Raw Rubato

Instantiate Rubato 4's synchronous `Fft<f64>` path directly with preallocated
interleaved buffers. This isolates the project's staging, delay compensation,
half-band selection, nonlinear engines, and drain behavior. The raw engine has
fixed chunk requirements and a real output delay, so the report must preserve
those differences instead of hiding them.

### libsamplerate

The mature streaming C API provides stateful processing, converter identities,
reset, consumed/generated frame counts, and an end-of-input flag. It is a good
independent high-quality comparator. Its public processing format is
interleaved `f32`, while this crate's API is `f64`; reports therefore must carry
sample format and must not present unlike formats as a strict speed regression
gate. Runtime loading keeps the normal build portable and can turn a missing
library into an explicit unavailable result.

### FFmpeg libswresample

This is the strongest broad industry reference and supports double-precision
sample formats, but its safe FFI setup includes channel layouts, sample formats,
options, delay accounting, conversion, and flush semantics across several
FFmpeg libraries. It is a worthwhile second adapter after the report and
adapter contracts are proven. Measuring the `ffmpeg` command as a subprocess is
not acceptable for steady streaming throughput because process startup, demux,
and I/O would dominate.

### SpeexDSP

SpeexDSP supplies a useful low-CPU/low-latency point rather than a direct
high-quality winner. Like libsamplerate, it normally processes integer or
single-precision samples. It should be a separate quality class and must not be
ranked against VHQ filters solely by throughput.

## Fairness Contract

Each engine row must record:

* implementation, upstream version, adapter version, and build identity;
* sample format, layout, channels, input/output rates, input chunk schedule;
* quality recipe and phase behavior without pretending names are equivalent;
* reported/measured latency, complete drain behavior, consumed and produced
  frame totals;
* steady-state timing separately from setup, reset, and drain;
* passband deviation, stopband/alias attenuation, THD+N, and finite-output work
  evidence;
* unavailable/skipped status with a concrete reason when a runtime library is
  absent.

The suite should present a quality/latency/throughput Pareto table. Historical
regression gates remain per engine and compatible build identity; cross-engine
rows are report-only until measured quality and format are genuinely matched.

## Feasible Approaches

### A. Adapter Harness Plus Runtime-Native Plugins (Recommended)

Create a new comparison probe and benchmark-owned adapter trait. Compile raw
SoXR and raw Rubato when their Cargo features are present. Load independent C
libraries through `libloading`, with explicit `--require-engine` enforcement
for evidence runs and explicit unavailable rows otherwise.

Pros:

* Production code and backend selection stay untouched.
* Normal builds do not link every native project.
* One workload/report contract can grow from libsamplerate to FFmpeg and
  SpeexDSP.
* Missing libraries cannot be mistaken for passing comparisons.

Cons:

* Evidence hosts still need known native binaries.
* C ABI adapters require careful ownership and version checks.
* Different native sample formats need separate lanes.

### B. Compile-Time Feature Per Native Project

Add `bench-libsamplerate`, `bench-ffmpeg`, and `bench-speexdsp` Cargo features
that link corresponding `-sys` crates.

Pros:

* Link failures expose missing development packages early.
* Rust bindings may cover more ABI details.

Cons:

* Cargo feature and native build complexity leaks into every benchmark build.
* Windows CI must provision matching SDKs before even compiling the probe.
* Feature combinations multiply and are harder to keep reproducible.

### C. External Executable Orchestrator

Generate fixtures and invoke project CLIs such as `ffmpeg`, then compare files
and wall time.

Pros:

* Minimal FFI code.
* Useful later for whole-file/end-to-end comparisons.

Cons:

* Cannot measure callback-scale streaming, reset, allocation, or lifecycle.
* Process startup and file I/O invalidate engine-throughput comparisons.
* Version/config discovery is harder to gate.

## Recommendation

Use Approach A. Deliver it in two bounded increments within this task:

1. Establish the adapter/report contract with the selected project backend,
   raw libsoxr, and raw Rubato controls.
2. Add and execute libsamplerate as the first independent runtime-loaded
   adapter; retain extension points for FFmpeg and SpeexDSP rather than
   pretending an unexecuted adapter is coverage.

Add FFmpeg next after the first independent report is validated. This ordering
maximizes evidence early while preventing the FFI-heavy adapter from defining an
untested report contract.
