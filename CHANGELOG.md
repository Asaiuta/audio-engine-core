# Changelog

All notable changes to `audio-engine-core` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While the crate is in the `0.x` series the public API is considered
experimental: minor version bumps may contain breaking changes, as permitted by
SemVer for pre-1.0 releases.

## [Unreleased]

### Added
- Public typed failure boundaries for pre-1.0 callers:
  `LoudnessDatabaseError` preserves directory-I/O and SQLite sources while
  naming poisoned locks, `AutomixError` separates cancellation, decoder, and
  invalid tail-seek failures, and `ResamplerError` now exposes structured
  geometry, capacity, initialization, timing, progress, stall, and processing
  classes.
- Lock-free playback lifecycle command channel, so a control thread can drive
  stream transitions while the pipeline lives in an audio callback (its
  `&mut self` methods are unreachable from elsewhere):
  `PlaybackController::request_reset`, `request_drain`, and
  `request_stop_with_fade` publish one coalescing request that
  `PlaybackPipeline::process` consumes at the next block boundary, and
  `lifecycle_status` / `PlaybackPipeline::lifecycle_state` report the applied
  request generation and `PlaybackLifecycleState`. Request handling, the stop
  ramp, the in-callback drain, and the in-callback reset are allocation-free
  and lock-free.
- Facade idle semantics: after a terminal in-callback drain, `process` writes
  silence and returns `Ok` instead of `ProcessError::AlreadyFinished`, because a
  device callback keeps firing after a track ends. `request_reset` re-arms it.
  The typed `AlreadyFinished` contract still applies to the explicit
  `finish_into_with_policy` path.
- `PlaybackConfig::with_drain_policy` / `drain_policy`: the bounded
  `ChainFinishPolicy` used by a callback-side drain, fixed at build time so the
  request carries no payload.
- Runtime saturation control on an armed stage:
  `PlaybackParameters::set_saturation_enabled` (soft bypass, preserving fixed
  latency and history), `set_saturation_drive`, `set_saturation_threshold`,
  `set_saturation_mix`, `set_saturation_type`, `set_saturation_gains_db`,
  plus `saturation_armed()` and a `saturation()` reader. Arming remains a
  build-time decision because it establishes the stage's latency; calling these
  on a non-armed pipeline returns a typed `UnsupportedOperation`.
- Exported control-value range constants (`VOLUME_MIN`/`VOLUME_MAX`,
  `EQ_BAND_GAIN_DB_MIN`/`_MAX`, `LIMITER_THRESHOLD_DB_MIN`/`_MAX`,
  `CROSSFEED_*`, `SATURATION_*`, `DYNAMIC_LOUDNESS_*`, `NOISE_SHAPER_BITS_*`,
  `MAX_STOP_FADE_MS`) so a UI can bound its widgets against the same values the
  parameter layer clamps to.
- New typed error `ProcessError::InvalidParameter` for control-thread writes
  rejected before they can reach DSP state.
- `AtomicSaturationParams::set_gains_db(input_gain_db, output_gain_db)`
  publishes both makeup gains as one coherent snapshot; the single-gain setters
  remain for changing one gain.

- High-level playback facade for the canonical callback DSP chain:
  `CallbackSpec` (validated callback-domain geometry with a `max_frames`
  prepared-capacity contract), intent-level `PlaybackConfig` with per-stage
  configs (`PlaybackSaturationConfig`, `PlaybackCrossfeedConfig`,
  `PlaybackDynamicLoudnessConfig`, `PlaybackNoiseShapingConfig`),
  `PlaybackBuilder`, callback-owned non-cloneable `PlaybackPipeline`
  (allocation-free `process`, timing/tail reporting, explicit bounded
  `finish_into_with_policy`, `reset`), non-cloneable `PlaybackController`
  holding the private convolver lease, and its cloneable `PlaybackParameters`
  publisher for control/UI threads. The default profile is sample-transparent.
  Construction delegates to `OutputChainBuilder`; raw atomic parameter handles
  and `ConvolverControl` remain advanced APIs and are not exposed.
- New typed error `ProcessError::UnsupportedOperation` for operations that an
  API surface intentionally does not support (used by the facade to reject
  runtime saturation re-arming, which requires a rebuild).
- `PlaybackController::load_impulse_response` / `set_convolution_enabled` /
  `convolution_status` / `reclaim_retired_convolution_kernels`: high-level
  convolution path. IR geometry is validated against the callback spec and FFT
  preparation happens on the control thread; the raw `ConvolverControl` lease
  stays private.
- `PlaybackParameters::set_eq` publishes enablement plus all band gains as one
  coherent snapshot (preset-friendly), and every facade parameter now has a
  control-thread snapshot reader (`volume`, `muted`, `eq_enabled`,
  `eq_band_gains_db`, `limiter_enabled`, `limiter_threshold_db`, `crossfeed`,
  `dynamic_loudness`, `noise_shaping`).
- `PlaybackSaturationConfig` gains `enabled()` plus `with_*` builders.

### Removed
- **Breaking:** AutoMix schema version 3 removes the unsupported
  `AutomixKeyStatus` reservation and the always-empty `key_root`, `key_mode`,
  `key_confidence`, and `camelot_key` fields. A key result contract will be
  introduced only with a validated detector.
- **Breaking:** the legacy public surface has been given an explicit lifecycle
  ahead of 1.0. Every item below had no consumer in this crate's production
  code, and each is superseded by a live equivalent. They are removed rather
  than deprecated because the crate is still `0.x`: shipping 1.0 already
  carrying deprecated API would buy a migration window nothing needs.
  - `VolumeController` — use the `VolumeProcessor` + `AtomicVolumeParams` pair
    that the output chain already builds. It smooths over ~5 ms rather than
    ~20 ms, skips the buffer entirely once the gain has settled at unity, and
    participates in the lock-free parameter and lifecycle contracts.
  - `GainRamp` — the playback facade implements its own stop fade, which
    advances gain per frame (so both channels of a frame share a gain) and
    recomputes it from the remaining ramp rather than accumulating a per-sample
    step.
  - `AtomicDynamicLoudnessState` — superseded by the `lockfree_params` snapshot
    types and `PlaybackParameters`; nothing constructed it.
  - `DEFAULT_BROADCAST_TARGET_LUFS` — unused. `DEFAULT_STREAMING_TARGET_LUFS`
    is retained and now documents that no code path applies it by default.
  - `ConvolverControl::publish` and `DEFAULT_CONVOLVER_SAMPLE_RATE_HZ` — use
    `ConvolverControl::publish_at_rate`, which takes an explicit sample-rate
    domain instead of assuming 44,100 Hz and returns `Result` for a zero rate.
  - `BiquadSection` is no longer exported. It is an implementation detail of
    `Equalizer`, and exporting it froze the biquad representation. Its unused
    `copy_coefficients_from` is removed with it; `Equalizer` adopts a fully
    processed crossfade branch with `clone_from`, which carries delay state.

### Changed
- **Breaking:** `StreamingDecoder::info` and `StreamingDecoderBuilder::info` are
  now read-only accessors returning `&AudioInfo`; the `info` field is private.
  Replace `decoder.info.sample_rate` with `decoder.info().sample_rate`. The
  decoder relies on these same values for staging geometry, gapless counters,
  buffer sizing, seek arithmetic, and reported position, so a public mutable
  field made observation data an unvalidated control channel into decode state.
- **Breaking:** `DecodeCancelToken` owns its cancel protocol.
  `DecodeCancelToken::new()` now takes no argument and creates a fresh
  uncancelled token, `cancel()` signals every clone, and the previous
  `new(Arc<AtomicBool>)` constructor is available as `from_flag` for callers
  that must adopt an existing flag.
- **Breaking:** fallible `PlaybackParameters` / `PlaybackController` setters now
  return `Result<(), ProcessError>`: `set_volume`, `set_eq_band_gain_db`,
  `set_eq`, `set_limiter_threshold_db`, `set_crossfeed`, and
  `set_dynamic_loudness`. A non-finite value is refused with
  `InvalidParameter`; finite out-of-range values are still clamped.
- **Breaking:** `PlaybackBuilder::build` validates configuration strictly and
  fails with `InvalidParameter` when a `PlaybackConfig` value is non-finite or
  outside its documented range, instead of silently clamping it.
- **Breaking:** the raw `Equalizer::set_band_gain` and `Equalizer::set_all_bands`
  now return `Result<(), ProcessError>`. A band index at or above `EQ_BANDS` and
  a non-finite gain are refused with `InvalidParameter` instead of being
  silently ignored; a whole-bank write is rejected before any band is applied.
  Valid input still clamps to `EQ_BAND_GAIN_DB_MIN`/`_MAX`, which the equalizer
  now reads from the published constants rather than local literals.
- Non-finite control values can no longer reach DSP state through the
  lower-level parameter types either: every `Atomic*Params` setter and `write`
  now drops a `NaN`/infinite write and keeps the previously published snapshot.
  Previously `f64::clamp` passed `NaN` through, which permanently poisoned IIR
  history until a reset.
- `AtomicEqParams::write` clamps band gains like the per-band setter, so
  `PlaybackParameters::eq_band_gains_db` reports the gains the equalizer
  actually applies (previously a preset could read back ±40 dB while ±15 dB was
  applied).
- Dynamic-loudness listening volume is bounded in the dB domain
  (`DYNAMIC_LOUDNESS_VOLUME_DB_MIN`/`_MAX`), so the reader round-trips a finite
  dB value instead of `-inf`.
- Callback volume is documented as attenuation-only (0.0–1.0); apply positive
  gain upstream.

- `ProcessError` and the facade config/telemetry structs
  (`PlaybackSaturationConfig`, `PlaybackCrossfeedConfig`,
  `PlaybackDynamicLoudnessConfig`, `PlaybackNoiseShapingConfig`,
  `DynamicLoudnessTelemetry`, `PlaybackTiming`) are now `#[non_exhaustive]`:
  construct configs via their constructors/builders and match errors with a
  wildcard arm so future additions are not breaking changes.
- HTTP sources now feed the server `Content-Type` header into Symphonia format
  probing alongside the URL-extension hint, improving detection for
  extensionless stream URLs and signed CDN paths. Generic media types
  (`application/octet-stream`, `binary/octet-stream`, `text/plain`) and MIME
  parameters are ignored.
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
  common Low-through-High ratios use the synchronous 1024/2 FFT engine,
  UltraHigh common ratios use the longer 1024/1 FFT engine, and only
  pathological reduced ratios use windowed sinc. The shared adapter removes
  each linear engine's leading delay and preserves duration-aligned
  drain/reset/chunking and allocation-free processing. On the recorded
  2026-07-24 Windows/Alder Lake same-revision quick matrix, 48-to-96 kHz High
  `process_checked` cost fell from the retained FFT route's
  36.104/14.354/17.667 to 5.849/5.807/6.026 ns/input sample at
  128/256/512-frame blocks. The algorithm label and case key changed so older
  FFT baselines are rejected automatically. UltraHigh retains -216.24 dB
  THD+N and all 27 quick quality gates pass.
- Stereo SoXR streaming now owns one native interleaved `Soxr<Stereo<f64>>`
  state and uses caller buffers directly. Mono and non-stereo layouts retain
  the independent-stream fallback. Stereo output remains bit-identical to the
  mono reference, adapter PCM scratch is zero, and process/finish remain
  allocation-free after setup.
- The Rubato 1024/2 adapter now bulk-copies channels, joins a staged FIFO prefix
  with the caller suffix without restaging it, asks Rubato to supply terminal
  zero padding, and discards delay/native suffix frames directly on a
  duration-completing drain. Constrained output retains the fixed-ring
  split/spill path. Test-only oracle switches prove every fast path bit-exact;
  they are not production runtime selectors.
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
- **Breaking:** every `LoudnessDatabase` operation and both AutoMix analysis
  entry points now return their module-owned typed error instead of `String`.
  `ResamplerError::InitializationFailed(String)` and
  `ProcessFailed(String)` are replaced by structured variants; callers should
  match error classes and retain a wildcard arm for these non-exhaustive enums.
- `NetworkError` is now `#[non_exhaustive]`: future transport classifications
  can be added without a breaking release. Downstream `match` expressions need
  a wildcard arm; treat unknown variants as non-retriable.
- `NetworkError` transport classification now prefers structured
  `std::io::ErrorKind` values found by walking the reqwest error source chain;
  error-message text matching is only a last-resort fallback. `ConnectionAborted`,
  `BrokenPipe`, and `UnexpectedEof` now classify as the retriable
  `ConnectionReset` instead of the non-retried `Other`. Retry semantics no
  longer depend on dependency error wording or localized OS messages, and the
  `ErrorKind`-to-retry mapping is locked by a contract test.
- Decode cancellation inside HTTP source paths is now the dedicated
  `NetworkError::Cancelled` variant instead of a `NetworkError::Other` value
  carrying magic message text. Matching on `NetworkError::Other("Decode
  cancelled")` no longer identifies cancellation; `DecoderError::Canceled`
  mapping and `is_retriable()` (`false`) are unchanged.
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
- AutoMix no longer sizes its whole-track `energy_profile` from an unbounded
  container-declared duration. That vector holds one slot per 100 ms of declared
  track length, so a file claiming an absurd duration requested a proportional
  allocation, and `vec![0.0; n]` aborts the process rather than returning an
  error. A declared duration above 24 hours is now discarded rather than
  clamped, falling back to the duration measured from decoded head evidence, and
  `build_energy_profile` enforces the same ceiling at the allocation site.
- The infallible standalone DSP setters now drop a non-finite write instead of
  storing it. `f64::clamp` returns `NaN` unchanged, so clamping alone let `NaN`
  reach filter, smoother, and coefficient state and poison that stage for the
  rest of the stream. This covers `Saturation::set_drive`/`set_threshold`/
  `set_mix`/`set_input_gain`/`set_output_gain`/`set_highpass_cutoff`,
  `VolumeController::set_target`, `DynamicLoudness::set_volume`/
  `set_volume_percent`/`set_volume_db`/`set_strength`/`set_reference_volume_db`/
  `set_transition_db`, `PeakLimiter::set_threshold`/`set_threshold_db`/
  `set_release_ms`, and `FirEq::set_sample_rate`/`set_band`/`set_bands`
  (`set_bands` is now all-or-nothing). `lockfree_params::sanitized` is the one
  shared policy for both parameter layers. The limiter deliberately keeps no
  published-range clamp, because the intersample-peak guard drives it below the
  user-facing minimum on purpose.
- `LoudnessDatabase::needs_scan` no longer reports a cached measurement fresh
  when there is no evidence for it. A local file that cannot be stat-ed
  (deleted, renamed, unmounted, permission denied) now needs a rescan instead of
  serving a stale gain, scanner-version matching is exact rather than `<` so a
  row written by a newer scanner is not trusted either, and remote identities
  are recognized case-insensitively so `HTTPS://…` is no longer treated as a
  local path. `get_outdated_tracks` propagates a row-decoding failure instead of
  dropping the row, because a silently short list reads as "nothing left to
  rescan".
- `PlaybackBuilder::build` now rejects a `ChainFinishPolicy` that cannot bound a
  tail (non-finite or positive energy threshold, zero silence hold, or a maximum
  tail below the hold) with `InvalidRenderPolicy`. The policy is fixed at build
  time, but it was previously first validated by the callback thread's initial
  drain, so a deterministic preset error surfaced on the realtime lifecycle path.
  `DspChain::finish_with_policy` still validates, because a chain can also be
  driven directly.
- The standalone `Saturation` setters now honour the published control ranges
  instead of re-encoding them as literals, and `set_input_gain` /
  `set_output_gain` clamp to `SATURATION_GAIN_DB_MIN`/`_MAX`. They previously
  applied no bound at all, so a direct core user could reach a makeup gain that
  `AtomicSaturationParams::set_gains_db` refuses to publish.
  `VolumeController::set_target`, `NoiseShaper`'s bit-depth clamps,
  `Crossfeed`'s mix sanitizer, and `DynamicLoudness`/`AtomicDynamicLoudnessState`
  strength and volume setters likewise read the published constants, so a range
  change can no longer be silently re-clamped by stale core code.
- `PlaybackCrossfeedConfig::disabled` now reports the crossfeed core's own
  starting mix and cutoff instead of an unrelated `0.5`/`700.0` pair, so
  enabling a previously bypassed config cannot change the profile it describes.
- `audio_gapless_comparison_perf --enforce` now fails when an attempted fixture's
  correctness probe could not produce a verdict. Such a fixture was recorded as
  `skipped` and excluded from `validations`, so a single passing fixture could
  turn a failed correctness probe into a green enforcement run. Probe failures
  are now a distinct `probe_failures` report field; `skipped` keeps its original
  meaning of work the run never owed.
- `PlaybackParameters::set_saturation_gains_db` now reaches the callback as one
  coherent snapshot. It previously published the input and output makeup gains
  separately, so a block could run the new input gain against the old output
  gain despite the facade's complete-snapshot contract. The new
  `AtomicSaturationParams::set_gains_db` patches both fields in a single guarded
  publication.
- `AtomicDynamicLoudnessParams::set_ref_volume_db` no longer loses a concurrent
  update. It read the whole snapshot outside the writer lock and republished the
  mutated copy, so a `set_strength`/`set_enabled`/`set_volume` landing in between
  was silently overwritten.
- `PlaybackParameters::set_eq_band_gain_db` no longer returns `Ok(())` for a
  band index at or above `EQ_BANDS`. The parameter layer silently dropped such a
  write, so an integration could persist or display an equalizer edit that never
  reached the callback; it now returns `InvalidParameter` and publishes nothing.
- A non-finite gain passed to the raw `Equalizer` no longer poisons that band's
  biquad history for the rest of the stream. `f64::clamp` passes `NaN` through,
  so the clamped value previously reached the coefficient design directly. The
  playback facade path was already protected by `AtomicEqParams`.
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
- **Breaking:** `PlaybackController::set_volume`, `set_muted`, and
  `dynamic_loudness_telemetry`. They proxied an arbitrary subset of
  `PlaybackParameters` onto the lifecycle handle, giving those three controls
  two apparent owners. Use `controller.parameters()`, which exposes the complete
  and cloneable control surface. The controller now owns only what cannot be
  shared: the single-consumer convolver lease and the lifecycle channel.
- **Breaking:** `config::SaturationConfig`, `config::DynamicLoudnessConfig`,
  `config::CrossfeedConfig`, and `config::DitherConfig`. They duplicated the
  callback stages' configuration model with no engine consumer anywhere in the
  crate, and their defaults had already drifted from the stages they claimed to
  describe (`CrossfeedConfig::default().mix` was `0.3` while the crossfeed core
  starts at `0.35`). Use the `Playback*Config` records in `pipeline`, which own
  the validated ranges the audio thread actually sees. The
  `config::SaturationQuality` / `config::SaturationType` re-exports existed only
  for `SaturationConfig` and are also removed; the canonical paths
  `processor::SaturationQuality` / `processor::SaturationType` are unchanged.
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
