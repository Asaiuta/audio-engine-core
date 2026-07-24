# Changelog

All notable changes to `audio-engine-core` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While the crate is in the `0.x` series the public API is considered
experimental: minor version bumps may contain breaking changes, as permitted by
SemVer for pre-1.0 releases.

## [Unreleased]

### Added
- Pluggable resampler backends behind unchanged public APIs: the new `soxr`
  feature (default) selects the native SoXR / SoX VHQ backend, and the new
  pure-Rust `rubato` feature selects quality-aware half-band/FFT/sinc/polyphase
  routing with no native dependency. Enabling neither is a compile error; when
  both are enabled, `soxr` wins. `default-features = false, features = ["rubato"]` now produces
  a fully pure-Rust build that does not link LGPL-2.1 libsoxr, and both
  backends satisfy the same streaming contract (arbitrary input granularity,
  duration-aligned drain, reset clearing history) and pass the same resampler
  test suite and 27 quick-run quality gates. Linear, Minimum, and Maximum phase
  routes are explicit and independently tested. A dedicated CI job builds and
  tests the pure-Rust path on a runner with no libsoxr installed.
- Public `RESAMPLER_BACKEND_NAME` constant naming the compile-time selected
  resampler backend (`"soxr"` or `"rubato"`; `soxr` wins when both features
  are enabled). Benchmark reports record the compiled backend in the
  environment `features` field (`resampler-soxr` / `resampler-rubato`) and in
  backend-derived resampler `algorithm` labels, so performance baselines
  recorded before backend labeling are incompatible with new reports.
- The pure-Rust resampler now uses rubato 4.0 plus a dedicated 127-tap
  symmetric half-band engine for exact 2x `Linear + High` upsampling. Other
  common Low-through-High ratios use the synchronous FFT engine, while
  UltraHigh and pathological ratios use windowed sinc. The shared adapter
  removes each linear engine's leading delay and preserves duration-aligned
  drain/reset/chunking and allocation-free processing. On the recorded
  2026-07-24 Windows/Alder Lake same-revision quick matrix, 48-to-96 kHz High
  `process_checked` cost fell from the retained FFT route's
  36.104/14.354/17.667 to 5.849/5.807/6.026 ns/input sample at
  128/256/512-frame blocks. The algorithm label and case key changed so older
  FFT baselines are rejected automatically. UltraHigh retains -216.24 dB
  THD+N and all 27 quick quality gates pass.
- Dual licensing under `MIT OR Apache-2.0` (`LICENSE-MIT`, `LICENSE-APACHE`).
- `NOTICE` file documenting the SoXR (libsoxr, LGPL-2.1) native dependency.
- Optional feature flags: `http` (network/streaming decode via `reqwest`) and
  `loudness-db` (SQLite loudness persistence via `rusqlite`). Both are enabled
  by default; disable with `default-features = false` for a lean DSP/decode
  build.
- `rustdoc` coverage for previously undocumented public configuration and
  pipeline types.
- Explicit channel-layout primitives, 5.1/7.1 downmixing, selectable
  `DownmixCoefficients`, and layout-aware EBU R128 channel weighting.
- `LimiterMode::TruePeak` as the default `PeakLimiter` detection mode, with
  `LimiterMode::SamplePeak` preserved for legacy sample-peak behavior.
- `SaturationQuality::Oversampled2x` and `SaturationQuality::Oversampled4x`
  quality modes plus matching lock-free saturation parameters.
- Partitioned long-IR routing for `FFTConvolver`, with public routing constants
  documenting the threshold and partition size.
- Canonical output-chain descriptors and builders for callback-safe DSP order
  and offline render-chain order.
- Objective listening-DSP benchmark rows for EQ target accuracy, crossfeed
  attenuation/continuity, and dynamic-loudness compensation.
- Compensated and raw-causal offline render policies, configurable unknown-tail
  energy/hold/maximum termination, and explicit render metadata for latency,
  finite tail length, rendered frames, and tail truncation.
- Versioned JSON evidence for the quality, callback-chain, and streaming-
  resampler benchmark entry points, including reproducible environment metadata,
  trial distributions, stable case keys, and compatible-baseline comparison.
- `AutomixKeyStatus` so versioned AutoMix results explicitly distinguish an
  unsupported key detector from a detector that ran without enough evidence.
- Versioned JSON evidence and compatible same-environment baseline comparison
  for FIR-EQ regeneration and apply performance.
- Ubuntu CI quick gates that upload all four quality/performance JSON reports.

### Changed
- Upgraded the decoder backend from Symphonia 0.5.5 to 0.6.0. The existing
  `StreamingDecoder` surface, supported codec matrix, source features, typed
  errors, and seek contract remain intact while the internal
  decoder/metadata/audio-buffer APIs follow the 0.6 model. Gapless ownership is
  now codec-aware: Symphonia owns MP3/Vorbis packet trim and reset preroll,
  while other codecs use the existing Track-level fallback exactly once.
- Raised the crate MSRV declaration to Rust 1.87. Symphonia 0.6 requires Rust
  1.85, while existing DSP code in this crate uses APIs stabilized in Rust 1.87.
- Realtime DSP adapters and `DspChain` now use the object-safe
  `StreamingProcessor` lifecycle with validated zero-copy interleaved blocks,
  explicit consumed/produced progress, backpressure, finish/reset, and typed
  errors. `DspChain::process`, `reset`, and `set_sample_rate` now return
  `Result` values, and fixed stages retain their in-place callback fast path.
- `StreamingResampler` now implements `StreamingProcessor` directly. Its
  out-of-place path reports exact SoXR input consumption/output production,
  `finish` uses native drain-to-zero semantics, and `reset` clears native state.
- Offline output rendering now finalizes each stage before passing its complete
  output to the next stage, so limiter delay and convolution/resampler output
  are propagated through every downstream transform.
- Translated remaining non-English source comments to English.
- Decoder probe/seek failures now map unsupported or unseekable inputs to typed
  `DecoderError::UnsupportedFormat` where Symphonia exposes that boundary.
- The README performance and quality sections now distinguish enforced gates,
  report-only probes, missing optional EBU corpus data, and known true-peak
  limitations.
- Callback and streaming-resampler benchmarks now report min/median/p95/max and
  raw trials instead of best-of-N. Quality full-output points now retain
  `RenderedOutput` latency, semantic-tail, rendered-frame, and truncation
  metadata; skipped corpus counts remain explicit in text and JSON.
- FIR-EQ performance now reports seven-trial quick distributions, stable case
  keys, work validation, and report-only timing unless a compatible baseline is
  supplied.

### Fixed
- Ogg/Vorbis coarse seek now uses Symphonia's native gapless reset behavior,
  eliminating the first post-seek overlap packet previously emitted by the
  crate-owned Track-only path.
- AutoMix tempo conversion now uses the spectral analyzer's actual
  `sample_rate / 512` cadence and tempo-derived lag bounds instead of applying
  the 50 Hz envelope cadence to spectral-flux samples.
- FIR EQ one-tap filters now produce a finite 1 kHz-reference scalar, uniform
  positive/negative gains retain their absolute magnitude, and the
  minimum-phase tail window fades in the correct direction.
- Equalizer crossfades now adopt the target branch's complete biquad state at
  transition completion instead of combining target coefficients with stale
  active-branch delay history.
- `LoudnessNormalizer` now publishes configured enabled state and all five
  normalization modes during construction and reconfiguration, while explicit
  setters keep the stored config and lock-free runtime state synchronized.
- Dynamic-loudness low/high shelves now follow the RBJ equations without an
  extra `sin(w0)` factor in the shelf term.
- Dynamic-loudness sample-rate changes now preserve strength, loudness factor,
  and smoother progress while rebuilding rate-dependent coefficients and
  explicitly resetting incompatible biquad delay history.
- `Resampler::resample_parallel` no longer silently truncates output for
  upsampling ratios above 1.5x (e.g. 44.1->96/192 kHz): per-chunk scratch is
  now sized by the actual conversion ratio and the flush drains SoXR in a loop
  until dry, matching the streaming path.
- Short and long 48->96 kHz streaming resamples now consume every input frame,
  produce the exact 2x duration after finish, and remain equivalent under
  irregular input chunking. Native reset no longer leaks the prior stream.
- Offline rendering preserves a last-frame impulse, drains limiter look-ahead,
  and carries finite convolution tails through downstream resampling before
  applying final timeline compensation.
- Unknown and infinite offline tails now stop generating as soon as the
  configured pre-dither RMS silence hold is satisfied instead of always
  computing to the hard maximum; reaching that maximum still reports explicit
  truncation.
- Realtime allocation regressions eliminated on the audio callback path: the
  FFT convolver and spectrum analyzer now reuse pre-sized SoXR/FFT scratch via
  `process_with_scratch` instead of allocating per call, and the convolver
  adapter no longer deep-clones or drops kernels on the audio thread (kernel
  adoption defers until the shared `Arc` is uniquely owned, retirement goes
  through a disposal slot). The crate's `assert_no_alloc` tests now register an
  `AllocDisabler` global allocator so these paths are actually verified.
- `VolumeProcessor` mute fade now decays once per frame instead of once per
  sample, so channels within a frame receive equal gain and the fade time
  constant no longer shortens with channel count.
- `StreamingDecoder::seek` no longer double-applies encoder-delay trimming
  after non-zero seeks.
- Crossfeed mix updates preserve filter history instead of resetting state.
- The callback performance benchmark now uses the shared output-chain builder
  and includes the noise-shaper stage it configures.
- `channels == 0` is rejected at construction for `StreamingResampler` and in
  `Resampler::resample_parallel`, and guarded in `Equalizer::process`, instead
  of panicking with a divide-by-zero at first use. `FFTConvolver::new` now
  documents its `channels > 0` panic contract.

### Removed
- The legacy `AudioProcessor` trait and `ProcessResult` enum. This is a direct
  breaking cutover; use `StreamingProcessor`, `ProcessBuffers`, and
  `process_checked` instead.
- `StreamingResampler`'s `process_chunk_borrowed`, `process_chunk_into`,
  `process_chunk_append`, `flush_borrowed`, `flush_into`, and `flush` methods;
  use the unified `process_checked` / `finish_checked` lifecycle instead.
- `AudioPipeline` and `PipelineError`: the background decode/resample worker
  had no backpressure (the ring filled and then dropped the oldest frames on
  every write) and its `read()` never released ring space, so any consumer
  slower than decode speed was pushed through the track at decode speed. No
  consumer ever constructed it. The `RingBuffer` primitive it wrapped remains
  public.

## [0.1.0] - 2026-06-11

### Added
- Initial extraction of the reusable audio engine core from the Lyne player:
  - Streaming Symphonia-based decoder with HTTP range-streaming support.
  - SoX VHQ resampling wrappers and a streaming resampler.
  - DSP processors: parametric EQ, FIR EQ, crossfeed, saturation, FFT
    convolution, dynamic loudness, volume smoothing, noise shaping, and
    spectrum analysis.
  - EBU R128 loudness and true-peak measurement helpers.
  - Lock-free DSP parameter snapshots and realtime processor adapters.
  - A streaming pipeline/ring-buffer primitive.

[Unreleased]: https://github.com/Asaiuta/audio-engine-core/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Asaiuta/audio-engine-core/releases/tag/v0.1.0
