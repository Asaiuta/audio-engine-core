# Changelog

All notable changes to `audio-engine-core` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While the crate is in the `0.x` series the public API is considered
experimental: minor version bumps may contain breaking changes, as permitted by
SemVer for pre-1.0 releases.

## [Unreleased]

### Added
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

### Changed
- Realtime DSP adapters and `DspChain` now use the object-safe
  `StreamingProcessor` lifecycle with validated zero-copy interleaved blocks,
  explicit consumed/produced progress, backpressure, finish/reset, and typed
  errors. `DspChain::process`, `reset`, and `set_sample_rate` now return
  `Result` values, and fixed stages retain their in-place callback fast path.
- Translated remaining non-English source comments to English.
- Decoder probe/seek failures now map unsupported or unseekable inputs to typed
  `DecoderError::UnsupportedFormat` where Symphonia exposes that boundary.
- The README performance and quality sections now distinguish enforced gates,
  report-only probes, missing optional EBU corpus data, and known true-peak
  limitations.

### Fixed
- `Resampler::resample_parallel` no longer silently truncates output for
  upsampling ratios above 1.5x (e.g. 44.1->96/192 kHz): per-chunk scratch is
  now sized by the actual conversion ratio and the flush drains SoXR in a loop
  until dry, matching the streaming path.
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
