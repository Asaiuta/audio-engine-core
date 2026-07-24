# 2x Linear Resampler Route Options

## Scope of this research

Repository-source audit for a dedicated pure-Rust, linear-phase 2:1 upsampling
route. The performance target is the existing 48 kHz to 96 kHz High-quality
streaming benchmark at 128/256/512 caller frames.

## Existing routing and constraints

* `RubatoEngine::new` sends every linear Low/Standard/High common ratio through
  `rubato::Fft` using a 1024-frame input chunk and two FFT subchunks. The
  existing 48->96 High path therefore still pays for generic FFT setup and work.
* `MonoBackend` already owns interleaved fixed-capacity input/output rings and
  uses direct caller output for a duration-stable integer ratio when the caller
  has enough space. A new engine should reuse this adapter rather than create a
  second streaming lifecycle implementation.
* The nonlinear `PolyphaseResampler` provides reusable low-pass prototype
  design parameters (High = 256 taps/phase, 0.96 rolloff, Kaiser beta 14), but
  rejects Linear phase and its generic per-output modulo/MAC loop does not
  exploit 2x half-band structure.
* The callback path must have no allocation, locks, logging, I/O, panics, or
  unbounded work after construction. It must preserve typed progress, arbitrary
  caller chunking, exact duration-aligned finish, reset isolation, and initial
  delay semantics.
* Benchmark reports must use a new algorithm identifier and compare only a
  compatible same-machine baseline. Quality checks include all 27 quick gates,
  duration/impulse/chunking/reset/no-allocation coverage, and both relevant
  feature matrices.

## Feasible approaches

### A. Dedicated symmetric half-band 2x engine (recommended)

Route `PhaseResponse::Linear`, `ResampleQuality::High`, and an exact 2:1
upsampling ratio to a setup-designed symmetric half-band FIR engine. It emits
two output frames for each input frame and exploits the half-band zero-tap
pattern so the nontrivial phase evaluates only the required taps. Keep the
current `MonoBackend` rings, direct-output guard, delay skipping, and drain
accounting intact.

Pros:

* Directly targets the generic FFT overhead and expresses the exact 2x
  structure.
* Keeps the public API and current behavior for all unrelated geometry.
* Can be made rate-pair agnostic for any exact 2:1 High-quality upsampling
  pair, while validating 48->96 as the benchmark target.

Costs and risks:

* Needs a high-order, measured filter design to clear the current quality bar.
* Must report/crop the symmetric FIR delay consistently with the existing
  linear path and prove bit-stable streaming behavior across chunk boundaries.

### B. Extend the generic polyphase engine to Linear phase

Reuse `design_linear_prototype` without cepstrum conversion and allow Linear
phase in `PolyphaseResampler`.

Pros: smallest routing-level change and already has bounded history/state.

Costs and risks: the existing generic per-output phase selection, modulo
history indexing, and 256-tap-per-phase work are unlikely to beat the retained
FFT path. It also entangles a nonlinear-phase contract with an unrelated
performance route. This is useful as a correctness reference, not the primary
optimization.

### C. Retune the current FFT route

Change subchunks, chunk size, or window parameters while retaining FFT.

Pros: low implementation risk.

Costs and risks: the adjacent measured evidence already rejected one and four
FFT subchunks. A window change does not remove runtime FFT work. This does not
address the specialized 2x opportunity and should remain a separate
`CHUNK_IN` investigation.

## Recommendation

Adopt approach A as a narrow MVP: exact 2:1, linear, High-quality upsampling;
all other phase/quality/ratio combinations retain the existing routes. Route
all exact 2:1 High pairs rather than hard-code 48->96, but make 48->96 the
only initial performance acceptance target. This preserves a clean semantic
rule and prevents an absolute-rate special case while keeping the change small.

## Sources inspected

* `src/processor/resampler/rubato_backend.rs`
* `src/processor/resampler/polyphase_backend.rs`
* `src/processor/resampler/mod.rs`
* `benches/audio_resampler_streaming_perf.rs`
* `.trellis/spec/backend/realtime-safety.md`
* `.trellis/spec/backend/streaming-lifecycle.md`
* `.trellis/spec/backend/quality-guidelines.md`
