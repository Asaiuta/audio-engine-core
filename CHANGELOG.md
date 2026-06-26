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
- Typed `PipelineError` returned by `AudioPipeline::new`.
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
- Translated remaining non-English source comments to English.
- Decoder probe/seek failures now map unsupported or unseekable inputs to typed
  `DecoderError::UnsupportedFormat` where Symphonia exposes that boundary.
- The README performance and quality sections now distinguish enforced gates,
  report-only probes, missing optional EBU corpus data, and known true-peak
  limitations.

### Fixed
- `StreamingDecoder::seek` no longer double-applies encoder-delay trimming
  after non-zero seeks.
- Crossfeed mix updates preserve filter history instead of resetting state.
- The callback performance benchmark now uses the shared output-chain builder
  and includes the noise-shaper stage it configures.

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
