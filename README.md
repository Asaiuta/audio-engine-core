# audio-engine-core

[![CI](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml/badge.svg)](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml)

> A realtime-safe Rust audio processing core for building high-quality music players.

`audio-engine-core` provides decoding, resampling, loudness normalization, DSP
processing, and streaming pipeline primitives — without owning the audio
device, the UI, or the application runtime. Extracted from the Lyne audio
engine as its app-agnostic core layer, it leaves playback, device output, and
library management to your application.

> ✅ Status: 1.0.0 — stable; the public API is documented and SemVer-guarded
> against breaking changes. Requires Rust 1.87+.

```text
┌──────────────────────────────────────────────────────────┐
│        Your Application  (UI · playback · library)       │
└─────────────────────────────┬────────────────────────────┘
                              │
┌─────────────────────────────▼────────────────────────────┐
│                     audio-engine-core                    │
│   Decode → Resample → Loudness → DSP → Analyze → Stream  │
└─────────────────────────────┬────────────────────────────┘
                              │  (not owned by this crate)
┌─────────────────────────────▼────────────────────────────┐
│      Audio Device Layer  (CPAL / WASAPI / CoreAudio)     │
└──────────────────────────────────────────────────────────┘
```

## Why audio-engine-core?

Building a serious music player runs into engineering problems that have
little to do with UI or playlists:

- **Audio callbacks that cannot block or allocate** — a missed deadline is an audible glitch.
- **Parameter changes racing audio processing** — torn, cross-version parameter reads.
- **Resampling without unacceptable artifacts** when source and device rates differ.
- **Loudness normalization across masters**, so albums mastered decades apart play at comparable levels.
- **Intersample peaks surviving processing** even when every stored sample is below full scale.
- **Gapless and streaming boundaries**, where codec delay/padding and seek behavior must be handled exactly once.

These are provided as reusable, measurable, testable components.

## Capabilities

| Area | What you get |
| --- | --- |
| Decode | Streaming decode built on Symphonia 0.6, a typed error policy for unsupported/corrupt input, and per-codec gapless ownership |
| Resampling | SoX VHQ streaming resampler (native SoXR backend, default) or quality-aware pure-Rust half-band/FFT/sinc/polyphase routing, behind one `process_checked` interface |
| Loudness | EBU R128 integrated loudness + true-peak measurement, offline analysis plus realtime atomic gain application |
| DSP | 10-band IIR biquad `Equalizer`, linear- and minimum-phase `FirEq` (applied via `FFTConvolver`), Bauer crossfeed, saturation with oversampled antialiasing, FFT convolution with partitioned routing for long IRs, dynamic loudness compensation, volume smoothing, true-peak limiter, noise shaping |
| Realtime control | Lock-free generation-based parameter snapshots for pushing changes into the audio callback |
| Streaming | Ring-buffer and pipeline primitives |
| Analysis | Spectrum analyzer, AutoMix analysis, objective quality measurement benches |

## Quick Start

```toml
[dependencies]
audio-engine-core = "1"
```

Measure the integrated loudness of a file:

```rust
use std::path::Path;

use audio_engine_core::{LoudnessMeter, MediaLocation, StreamingDecoder};

fn analyze_file(path: &Path) -> Result<f64, Box<dyn std::error::Error>> {
    let location = MediaLocation::local(path.to_path_buf());
    let mut decoder = StreamingDecoder::open(location)?;
    let info = decoder.info();
    let mut meter = LoudnessMeter::new(info.channels, info.sample_rate)?;

    while let Some(samples) = decoder.decode_next()? {
        meter.process(&samples)?;
    }

    Ok(meter.integrated_loudness())
}
```

Two runnable examples need no audio files and no optional features:

- `resample_sine` — streams a synthetic 48 kHz sine through the SoX VHQ
  resampler to 44.1 kHz (exact cursor advancement, then `finish_checked`).
- `equalizer_curve` — runs a stereo buffer through the 10-band `Equalizer`.

```bash
cargo run --example resample_sine
cargo run --example equalizer_curve
```

## Realtime-Safe by Design

The processing path is built around one invariant: no allocations, locks, file
I/O, logging, or network I/O in the audio callback. Allocate and configure
processors up front, then update parameters through atomic snapshot types:

```text
Control thread (UI, config)             Audio callback (once per buffer)
      │ set_* / publish                        │ snapshot read
      ▼                                        ▼
┌──────────────────── lock-free atomic snapshot ───────────────────┐
│  AtomicEqParams, AtomicVolumeParams, ...  (no locks, no allocs)  │
└──────────────────────────────────────────────────────────────────┘
```

Reading the full set of cached parameters once per callback costs about
**7 ns** with the generation-based snapshot path, versus ~50 ns for a naive
split-atomic field-by-field read and ~83 ns for an unconditional `ArcSwap`
guard load (`audio_lockfree_params_perf`; single-machine evidence).

Processors implement the object-safe `StreamingProcessor` lifecycle. For the
recommended canonical callback DSP order, build a caller-driven
`PlaybackPipeline`. The pipeline owns neither a decoder nor an audio device;
the callback retains ownership of its interleaved `f64` buffer:

```rust
use audio_engine_core::{
    CallbackSpec, PlaybackConfig, PlaybackCrossfeedConfig, PlaybackLifecycleState,
    PlaybackPipeline,
};

let spec = CallbackSpec::stereo(48_000, 512)?;
let (mut pipeline, controller) = PlaybackPipeline::builder(spec)
    .configure(
        PlaybackConfig::transparent()
            .with_crossfeed(PlaybackCrossfeedConfig::enabled(0.25, 800.0)),
    )
    .build()?;

// The controller is exclusive because it owns the pipeline's private
// single-consumer lease; it is also how impulse responses are loaded. Its
// parameter publisher is clonable for UI and remote-control threads. A
// non-finite value is refused; a finite out-of-range value is clamped.
let parameters = controller.parameters();
parameters.set_volume(0.8)?;
parameters.set_eq_band_gain_db(3, 2.5)?;

// Audio callback: handle the typed result without logging or panicking here.
let mut samples = [0.0_f64; 512 * 2];
let progress = pipeline.process(&mut samples)?;

// Track change from a control thread while the pipeline lives in the callback:
// ramp down, drain the tail at the block boundary, then re-arm.
controller.request_stop_with_fade(20)?;
while pipeline.lifecycle_state() != PlaybackLifecycleState::Idle {
    let _ = pipeline.process(&mut samples)?;
}
controller.request_reset();
# Ok::<(), audio_engine_core::ProcessError>(())
```

`CallbackSpec` describes already-converted device-domain audio and bounds the
maximum callback block size. `PlaybackConfig::transparent()` is the default:
it disables every non-identity stage, including the limiter, so it preserves
samples and adds no limiter latency. `PlaybackPipeline::process` is
allocation-free for prepared-capacity blocks.

Lifecycle transitions are requested from the control thread and applied by the
callback at a block boundary, because the pipeline is moved into the callback
and cannot be borrowed mutably from elsewhere: `request_reset`,
`request_drain`, and `request_stop_with_fade` are lock-free and allocation-free,
and `PlaybackController::lifecycle_status` reports the applied request
generation. While draining, the callback block is overwritten with the
remaining effect tail (bounded by `PlaybackConfig::with_drain_policy`); once
terminal, `process` writes silence and keeps succeeding, because a device
callback does not stop firing when a track ends. `finish_into_with_policy` and
`reset` remain the direct control-thread operations for integrations that own
the pipeline outside a callback.

Value contract: build-time configuration is validated strictly — a non-finite
or out-of-range `PlaybackConfig` value fails `build()` with
`ProcessError::InvalidParameter`. Runtime parameter writes refuse non-finite
values with the same typed error and clamp finite values into their documented
ranges (`VOLUME_MIN`/`VOLUME_MAX`, `EQ_BAND_GAIN_DB_MIN`/`_MAX`, and the other
exported range constants), and every `PlaybackParameters` reader returns the
value actually in effect. Callback volume attenuates only (0.0–1.0); apply
positive gain upstream.

`PlaybackController` is intentionally exclusive because it retains the private
convolver lease. Convolution is loaded through it:
`controller.load_impulse_response(&ir)?` validates the interleaved IR against
the callback spec and prepares the FFT kernel on the control thread, and the
audio callback adopts it without allocating. Saturation arming is a build-time
decision because it fixes the stage's latency, but an armed stage accepts
runtime drive/threshold/mix/type/gain changes and soft bypass.
`PlaybackParameters` from `controller.parameters()`
is the safe clonable UI/remote update handle. Dynamic-loudness telemetry is
best-effort latest per-field reporting, not a coherent multi-value snapshot.
For custom stage order or raw atomic controls, use the lower-level
`OutputChainBuilder` / `StreamingProcessor` APIs directly.

```rust
use audio_engine_core::processor::traits::{
    process_checked, AudioBlockMut, ProcessBuffers, ProcessError, ProcessProgress,
    StreamingProcessor,
};

fn process_callback_block(
    processor: &mut dyn StreamingProcessor,
    samples: &mut [f64],
    channels: usize,
) -> Result<ProcessProgress, ProcessError> {
    let block = AudioBlockMut::new(samples, channels)?;
    process_checked(processor, ProcessBuffers::in_place(block))
}
```

### Migration notes

The former `AudioProcessor` / `ProcessResult` API was removed; adapters
implement `StreamingProcessor` directly. `DspChain::process` / `reset` /
`set_sample_rate` return typed results, and callback integrations must handle
failures without logging or panicking on the audio thread. Fixed processors
keep the zero-copy in-place fast path and implement `FixedInPlaceProcessor`,
the admission contract required by `DspChain::add`. `DspChain::new` and
`with_capacity` return `Result` and reject a zero sample rate; the chain has no
arbitrary `Default`. Enable/mute controls belong to concrete atomic parameter
handles and `ConvolverControl`, not the base streaming lifecycle. Out-of-place
calls use caller-provided output and report `NeedInput` / `NeedOutput`
backpressure explicitly.
`StreamingResampler` follows the same out-of-place contract: advance both
cursors from `ProcessProgress`, end the stream with native SoXR `drain()` via
`finish_checked`, and use `reset()` to clear the native history (the old
`process_chunk_*` / `flush_*` helpers could not represent partially consumed
input). Offline `OutputRenderChain::render` defaults to a compensated
timeline — accumulated algorithmic latency is removed once at the final output
rate while finite semantic effect tails are retained; `OfflineRenderPolicy::raw_causal()`
keeps the leading delay and all finalize output. Unknown/infinite tails use a
configurable pre-dither RMS threshold, continuous silence hold, and hard
maximum, with `RenderedOutput::tail_truncated` set when that maximum is hit.
`OutputChainParams` carries callback/output-domain configuration only; pass the
input rate to `build_render_chain(source_rate)` or
`build_render_chain_with_policy(source_rate, policy)` when constructing an
offline renderer.

## Quality & Validation

This project treats audio quality as something to measure, not only listen to.
The benches in `benches/` run against the public API and analyze rendered `f64` buffers:

| Domain | What is measured |
| --- | --- |
| Loudness | EBU R128 parity against a reference implementation |
| True peak | Oversampled intersample-peak detection |
| Resampling | Passband deviation, alias attenuation, THD+N |
| EQ | Target response accuracy |
| Saturation | Folded alias energy |
| Convolution | IR correctness vs an overlap-save reference |
| Realtime control | Parameter-change continuity |

Representative results from a single machine and configuration (reproduce with
`cargo bench`; values differ by CPU, compiler, and load):

- `LoudnessMeter` integrated loudness parity vs direct `ebur128`: **0.000000 LU**
- Resampler THD+N, 44.1 kHz to 48 kHz: **-187.0 dB** (default SoXR backend; pure-Rust rubato UltraHigh measures -204.9 dB, see [docs/quality.md](docs/quality.md))
- Worst fitted alias attenuation, 96 kHz to 48 kHz: **-290.2 dB** (near the analyzer's own numeric floor)
- True-peak limiter: **-1.00 dBTP** on a +0.10 dBTP intersample-stress signal (legacy sample-peak mode never engages: +0.10 dBTP)
- Dynamic loudness low-volume compensation: **+8.41 dB at 40 Hz / +2.83 dB at 3 kHz**

One structural caveat is kept visible: the limiter runs at source rate, so
resampling plus final quantization downstream can in principle re-introduce
intersample peaks, and the full output-chain true-peak probe stays report-only
rather than a conformance gate. In the current quick run the probe meets the
target — worst full-chain output true peak -1.000 dBTP with zero over-limit
points — and it is retained as regression evidence.

On the 2026-07-27 core-pinned heavy adapter controls, SoXR v2 measured
8.569 / 7.424 ns/input-sample for 44.1-to-48 / 48-to-44.1 kHz versus raw
libsoxr at 8.632 / 7.368, a statistical tie in both directions. In the
separate same-geometry Rubato build, v17 measured 8.182 / 7.025 versus raw
Rubato at 8.592 / 6.908: 4.77% faster forward and tied reverse. The wider
11-engine matrix is Pareto evidence across different recipes, lanes, and
latency policies, not a universal fastest ranking.

The full in-crate benchmark commands, JSON report/baseline machinery,
processing-budget tables, and complete measurement tables live in
[docs/quality.md](docs/quality.md). Raw-upstream and independent resampler
methodology and results live in
[docs/resampler-comparison.md](docs/resampler-comparison.md).

## Installation & Feature Flags

All four Cargo features below are independent; the first three are enabled by
default:

- `http` (default): HTTP/HTTPS streaming decode via `reqwest`, including Range
  streaming and full-download fallback. `MediaLocation` validates local versus
  HTTP identity independently of this feature. With `http` off, an HTTP
  location returns `DecoderError::FeatureUnavailable`; `reqwest` and the
  `NetworkError` type are not compiled.
- `loudness-db` (default): SQLite-backed loudness metadata persistence
  (`LoudnessDatabase`, `TrackLoudness`, `LoudnessSourceIdentity`,
  `DatabaseStats`) via `rusqlite`. Cache keys use namespaced SHA-256 identities;
  signed HTTP URLs are never stored in plaintext, and HTTP records without a
  validator are always stale. With this off, the EBU R128 helpers
  (`LoudnessMeter`, `LoudnessNormalizer`, `TruePeakDetector`) still work; only
  the on-disk cache is removed.
- `soxr` (default): native SoXR resampler backend (SoX VHQ). Requires the
  libsoxr native library at build/link time; libsoxr is LGPL-2.1 (see
  [License](#license)).
- `rubato`: quality-aware pure-Rust backend. Exact 2x upsampling at
  `PhaseResponse::Linear` + High uses a dedicated 127-tap symmetric half-band
  FIR; other common ratios use FFT, with two sub-chunks through High and one
  longer sub-chunk for UltraHigh, while only pathological reduced ratios use
  windowed sinc. `Minimum` and `Maximum` use setup-designed rational FIRs with
  real-cepstrum spectral factorization, selecting spectral execution for small
  interpolation factors and contiguous polyphase execution otherwise; reduced
  rate components above 1024 are rejected rather than silently treated as
  linear phase. No native dependency. At least one
  resampler backend must be enabled — enabling neither is a compile error, and
  when both are enabled, `soxr` wins.

A fully pure-Rust, DSP-only build with no native dependency:

```toml
audio-engine-core = { version = "1", default-features = false, features = ["rubato"] }
```

Windows (vcpkg or MSYS2) and Unix setup instructions for the default SoXR
backend are in [docs/installation.md](docs/installation.md).

## Scope

This crate owns the audio processing layer. It deliberately does not own
device management (CPAL/WASAPI output streams), desktop UI or Tauri
integration, playback queue logic, media-library scanning, HTTP/WebSocket
server routes, WebDAV or NetEase integration, or application runtime
directories — those stay in the Lyne application crate, with no stable
compatibility layer for every internal Lyne use case. This separation lets the
core be embedded under different applications and output backends.

## Who Is This For

Good fit if you are:

- building a Rust music player and want a processing core under it,
- assembling a custom realtime audio pipeline,
- experimenting with high-quality DSP (EQ, crossfeed, saturation, convolution),
- writing offline loudness-analysis tooling.

May not fit if you need: a complete player, a high-level playback API, an audio
device abstraction, or a stable 1.0 API today.

## Decoding & Format Support

Decoding is built on [Symphonia](https://github.com/pdeljanov/Symphonia) 0.6
with all of its bundled codecs/containers compiled in (e.g. WAV, FLAC, MP3,
AAC/MP4, OGG/Vorbis); the crate adds no custom codecs and makes the support
boundary explicit and tested. `StreamingDecoder` exposes the decoded sample
rate, channel count, and (when known) total frame count and duration via the
read-only `decoder.info()`, including the best-effort positional
`decoder.info().channel_layout`. It is observation only: the decoder relies on
the same values for staging, gapless trimming, and seek arithmetic, so they are
not a caller-writable control channel.

- **Unsupported / unrecognized input** returns the typed
  `DecoderError::UnsupportedFormat`; a container that probes but has no
  decodable audio track returns `DecoderError::NoAudioTrack`.
- **Corrupt or truncated input** has a defined policy: the decoder either
  returns a typed error or yields the partial samples it could recover — it
  never panics and never silently reports a full decode of missing data.
- **Gapless ownership** is an explicit per-codec split: Symphonia owns MP3 and
  Vorbis packet trim/reset behavior; other codecs retain the crate's Track-level
  delay/padding fallback. The two paths are mutually exclusive, so delay or
  padding cannot be trimmed twice.
- **Seeking** uses Symphonia's `SeekMode::Coarse` only; a sample-exact
  (`Accurate`) mode is intentionally not exposed. A coarse seek lands on a
  packet/frame boundary at or before the requested time — bounded inaccuracy
  documented as `StreamingDecoder::SEEK_COARSE_TOLERANCE_FRAMES`, with the
  realized position readable via `decoder.current_frame()`. Track-level encoder
  delay applies only at the true start of the stream; native MP3/Vorbis decoders
  consume their packet-local trim and reset preroll after a seek.

## Project Status

Stable `1.0.0` release: the public API is fully documented (`missing_docs` is
denied at compile time), frozen by committed surface snapshots, and guarded by
a SemVer gate in CI — any breaking change fails the build before it can ship.
Requires Rust 1.87+ (Symphonia 0.6 itself requires 1.85; the higher crate MSRV
reflects existing DSP code in this repository). Used in production as the
audio foundation of the Lyne player. Breaking changes are reserved for major
version bumps per the policy in [CONTRIBUTING.md](CONTRIBUTING.md); notable
changes are recorded in [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

### Native dependency licensing

With the default `soxr` feature, this crate links the SoXR native library
(libsoxr), which is distributed under the LGPL-2.1. The Rust source in this
crate is MIT OR Apache-2.0, but binaries that statically link libsoxr carry
LGPL-2.1 relinking obligations. Building with `default-features = false` and
the pure-Rust `rubato` backend does not link libsoxr and carries no LGPL
obligation. See [NOTICE](NOTICE) for details.
