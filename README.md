# audio-engine-core

**English** | [简体中文](README.zh-CN.md)

[![CI](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml/badge.svg)](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml)

> **A realtime-safe, measurable audio processing core for Rust.**

`audio-engine-core` is an application-agnostic Rust audio core for building high-quality music players and realtime audio applications.

It provides the processing infrastructure between decoded audio and the output device:

**Decode → Resample → Loudness → DSP → Analyze → Stream**

The core deliberately does **not** own your UI, audio device, playback runtime, or media library. It is designed to sit underneath an application as a dedicated audio-processing layer.

> **Production origin:** `audio-engine-core` was extracted from the Lyne audio engine as its application-agnostic core layer.

> **Status:** `1.0.0` — stable public API, documented and SemVer-guarded against breaking changes. Requires Rust 1.87+.

---

## Why audio-engine-core?

A serious music player is not just a collection of DSP algorithms.

The difficult part is making those algorithms work together under the constraints of a realtime audio callback while preserving audio quality, deterministic behavior, and predictable ownership.

`audio-engine-core` is built around three first-class design goals:

|                     |                                                                                                                   |
| ------------------- | ----------------------------------------------------------------------------------------------------------------- |
| **Realtime safety** | No allocation, blocking locks, I/O, logging, or uncontrolled failure in the audio callback                        |
| **Audio quality**   | High-quality resampling, loudness normalization, true-peak handling, antialiased DSP, and gapless-aware streaming |
| **Measurability**   | Public-API benchmarks, reference comparisons, objective quality metrics, and CI-enforced invariants               |

The goal is not to provide the largest collection of audio effects.

The goal is to provide a **composable audio processing core that can be trusted inside a realtime playback system.**

---

## What makes it different?

### Realtime-safe by construction

The audio callback is treated as a hard realtime boundary.

Processors are prepared before entering the callback. Runtime control is transferred through lock-free atomic parameter snapshots rather than synchronizing directly with UI or control threads.

```text
Control / UI thread                       Audio callback
       │                                      │
       │ publish parameters                   │ snapshot
       ▼                                      ▼
┌─────────────────────────────────────────────────────────┐
│        generation-based atomic parameter snapshot       │
│        no locks · no allocation · coherent versions     │
└─────────────────────────────────────────────────────────┘
```

The callback path is designed to avoid:

* heap allocation
* mutexes and blocking synchronization
* file or network I/O
* logging
* panic-based control flow

The generation-based parameter snapshot path (benchmarked as
`audio_lockfree_params_perf`) measures approximately **7 ns per callback** on
the benchmark machine, compared with approximately **50 ns** for naive
split-atomic reads and **83 ns** for an unconditional `ArcSwap` guard load.

These are single-machine benchmark results, not universal hardware guarantees.

---

### More than a DSP collection

The project combines decoding, resampling, loudness processing, DSP, analysis, and streaming primitives into a single processing architecture.

```text
                         audio-engine-core

┌──────────────┐
│    Decode    │
│  Symphonia   │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│   Resample   │
│ SoX VHQ /    │
│ pure Rust    │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│   Loudness   │
│ EBU R128 /   │
│  True Peak   │
└──────┬───────┘
       │
       ▼
┌──────────────────────────────────────────────┐
│                    DSP                       │
│                                              │
│ EQ · FIR EQ · Crossfeed · Convolution        │
│ Saturation · Dynamic Loudness · Limiter      │
│ Volume · Noise Shaping                       │
└──────────────────────┬───────────────────────┘
                       │
                       ▼
┌──────────────┐
│   Analysis   │
│ Spectrum /   │
│ AutoMix /    │
│ Measurements │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│  Streaming   │
│ Ring buffers │
│ Pipeline     │
└──────────────┘
```

The application remains responsible for playback state, UI, library management, networking, and device output.

---

### Audio quality is measured, not just claimed

Audio quality is treated as an engineering property that should be measurable.

The project includes objective validation for quality-sensitive components, including:

* EBU R128 loudness parity
* true-peak / intersample peak detection
* resampler passband behavior
* resampler alias attenuation
* resampler THD+N
* EQ response accuracy
* saturation alias energy
* convolution correctness
* realtime parameter continuity
* full-chain output true-peak limits

The benchmarks operate against the public API and analyze rendered audio buffers rather than private implementation shortcuts.

Detailed methodology and reproducible commands are documented in `docs/quality.md`.

---

## Architecture

`audio-engine-core` intentionally stops before the application and device layer.

```text
┌──────────────────────────────────────────────────────────┐
│                    Your Application                      │
│                                                          │
│       UI · Playback · Library · Network · Runtime       │
└──────────────────────────┬───────────────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────────────┐
│                  audio-engine-core                       │
│                                                          │
│ Decode → Resample → Loudness → DSP → Analyze → Stream   │
└──────────────────────────┬───────────────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────────────┐
│                    Device Layer                          │
│             CPAL · WASAPI · CoreAudio · ...             │
└──────────────────────────────────────────────────────────┘
```

This separation is intentional.

The core owns **audio processing**, not the application.

That makes it possible to reuse the same processing layer across different players, runtimes, and output backends without inheriting an entire application architecture.

---

## Core capabilities

| Area                  | Capability                                                                                                 |
| --------------------- | ---------------------------------------------------------------------------------------------------------- |
| **Decode**            | Streaming decode built on Symphonia 0.6, typed error handling, and per-codec gapless ownership             |
| **Resampling**        | SoX VHQ / SoXR backend and quality-aware pure-Rust resampling backends behind a common streaming interface |
| **Loudness**          | EBU R128 integrated loudness, True Peak measurement, offline analysis, and realtime normalization          |
| **EQ**                | 10-band IIR biquad EQ and linear-/minimum-phase FIR EQ                                                     |
| **Convolution**       | FFT convolution with partitioned routing for long impulse responses                                        |
| **Crossfeed**         | Bauer crossfeed                                                                                            |
| **Saturation**        | Oversampled saturation with antialiasing                                                                   |
| **Dynamic Loudness**  | Loudness compensation based on perceptual equal-loudness behavior                                          |
| **Limiter**           | True-peak limiting                                                                                         |
| **Volume**            | Smoothed realtime volume control                                                                           |
| **Noise**             | Noise shaping / dithering support                                                                          |
| **Realtime control**  | Generation-based lock-free atomic parameter snapshots                                                      |
| **Streaming**         | Ring-buffer and pipeline primitives                                                                        |
| **Analysis**          | Spectrum analysis, AutoMix analysis, and objective measurement benches                                     |
| **Offline rendering** | Latency-compensated rendering and configurable effect-tail policies                                        |

---

## Realtime processing model

The recommended high-level API is `PlaybackPipeline`.

```rust
use audio_engine_core::{
    CallbackSpec,
    PlaybackConfig,
    PlaybackCrossfeedConfig,
    PlaybackLifecycleState,
    PlaybackPipeline,
};

let spec = CallbackSpec::stereo(48_000, 512)?;

let (mut pipeline, controller) = PlaybackPipeline::builder(spec)
    .configure(
        PlaybackConfig::transparent()
            .with_crossfeed(
                PlaybackCrossfeedConfig::enabled(0.25, 800.0)
            ),
    )
    .build()?;

let parameters = controller.parameters();

parameters.set_volume(0.8)?;
parameters.set_eq_band_gain_db(3, 2.5)?;

let mut samples = [0.0_f64; 512 * 2];

// Audio callback.
let progress = pipeline.process(&mut samples)?;
# Ok::<(), audio_engine_core::ProcessError>(())
```

`CallbackSpec` describes already-converted device-domain audio and bounds the maximum callback block size.

`PlaybackConfig::transparent()` provides an identity-oriented default: non-identity stages such as the limiter are disabled, preserving samples and adding no limiter latency.

`PlaybackPipeline::process` is allocation-free for prepared-capacity blocks.

### Control and lifecycle

Playback lifecycle transitions are requested from the control side and applied by the audio callback at block boundaries.

```text
Control thread
     │
     │ request_stop_with_fade()
     ▼
┌─────────────────────────┐
│ Atomic lifecycle request│
└────────────┬────────────┘
             │
             ▼
      Audio callback
             │
             ├── ramp down
             ├── drain effect tail
             ├── enter terminal state
             └── output silence
```

This allows the pipeline to remain owned by the callback while still supporting:

* lock-free lifecycle requests
* block-boundary state transitions
* fade-out
* effect-tail draining
* reset and re-arm
* gapless playback integration

The callback continues to succeed after a track reaches its terminal state because an audio device callback does not stop firing merely because a track ended.

Because the pipeline is moved into the callback, it cannot be borrowed mutably from elsewhere: `request_reset`, `request_drain`, and `request_stop_with_fade` are lock-free and allocation-free, and `PlaybackController::lifecycle_status` reports the applied request generation. While draining, the callback block is overwritten with the remaining effect tail (bounded by `PlaybackConfig::with_drain_policy`); once terminal, `process` writes silence and keeps succeeding. `finish_into_with_policy` and `reset` remain the direct control-thread operations for integrations that own the pipeline outside a callback.

### Parameter safety

Runtime parameter changes are designed around explicit contracts.

Build-time configuration is validated strictly:

* non-finite values are rejected
* invalid ranges return typed `ProcessError` values
* runtime parameter writes reject non-finite values
* finite out-of-range values are clamped to documented limits
* parameter readers return the value actually in effect

Examples include the exported volume and EQ gain range constants (`VOLUME_MIN` / `VOLUME_MAX`, `EQ_BAND_GAIN_DB_MIN` / `_MAX`, and the other exported range constants). Callback volume attenuates only (0.0–1.0); apply positive gain upstream.

The parameter publisher can be cloned and used by UI or remote-control threads without requiring those threads to touch the realtime processing state directly.

`PlaybackController` is intentionally exclusive because it retains the private convolver lease. Convolution is loaded through it: `controller.load_impulse_response(&ir)?` validates the interleaved IR against the callback spec and prepares the FFT kernel on the control thread, and the audio callback adopts it without allocating. Saturation arming is a build-time decision because it fixes the stage's latency, but an armed stage accepts runtime drive/threshold/mix/type/gain changes and soft bypass. `PlaybackParameters` from `controller.parameters()` is the safe clonable UI/remote update handle. Dynamic-loudness telemetry is best-effort latest per-field reporting, not a coherent multi-value snapshot.

For custom processing graphs or lower-level atomic controls, use the `OutputChainBuilder` and `StreamingProcessor` APIs.

---

## Streaming processor model

Processors implement the object-safe `StreamingProcessor` lifecycle.

```rust
use audio_engine_core::processor::traits::{
    process_checked,
    AudioBlockMut,
    ProcessBuffers,
    ProcessError,
    ProcessProgress,
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

The streaming API explicitly represents partial input/output consumption through `ProcessProgress`.

This avoids hidden assumptions about block size and makes backpressure visible to the caller.

---

## Resampling

Resampling is exposed through a common streaming contract.

The project supports:

### SoX VHQ / SoXR

The opt-in native backend provides high-quality SoX resampling through libsoxr (SoX VHQ quality), enabled with `features = ["soxr"]`. It requires the libsoxr native library at build/link time (LGPL-2.1; see [License](#license)).

### Pure Rust

The default `rubato` backend provides quality-aware pure-Rust routing, including:

* half-band FIR paths for exact 2× conversion (`PhaseResponse::Linear` + High uses a dedicated 127-tap symmetric half-band FIR)
* FFT-based paths for common ratios (two sub-chunks through High, one longer sub-chunk for UltraHigh)
* windowed-sinc handling for pathological reduced ratios
* setup-designed rational FIRs with real-cepstrum spectral factorization for Minimum/Maximum phases, selecting spectral execution for small interpolation factors and contiguous polyphase execution otherwise
* reduced-rate components above 1024 are rejected rather than silently treated as linear phase
* no native dependency

The streaming contract explicitly tracks consumed and produced samples.

Finalization uses `finish_checked()` to drain native resampler state, while `reset()` clears the streaming history.

At least one resampler backend must be enabled — enabling neither is a compile error, and when both are enabled, `soxr` wins.

Detailed comparisons and methodology are documented in `docs/resampler-comparison.md`.

---

## Loudness normalization

The loudness subsystem separates offline analysis from realtime application.

```text
Offline analysis
      │
      ├── Integrated Loudness
      ├── True Peak
      └── Track metadata
              │
              ▼
       persistent metadata
              │
              ▼
Realtime playback
              │
              └── atomic gain application
```

This allows expensive analysis to happen outside the realtime callback while the callback only performs the minimal runtime work required to apply the result.

The implementation targets EBU R128 loudness measurement and includes True Peak analysis. The offline analysis, normalization helpers (`LoudnessMeter`, `LoudnessNormalizer`, `TruePeakDetector`), and realtime atomic gain application all work with the default features; optional SQLite persistence for track loudness is described under [Feature flags](#feature-flags).

---

## Offline rendering

The same processing architecture can be used for offline rendering.

`OutputRenderChain::render` supports latency-aware rendering policies and effect-tail handling.

The default compensated timeline removes accumulated algorithmic latency once at the final output rate while retaining finite semantic effect tails.

`OfflineRenderPolicy::raw_causal()` can instead preserve the leading causal delay.

For unknown or effectively infinite tails, rendering can use:

* configurable RMS thresholds
* continuous silence hold
* hard maximum limits

When the hard maximum is reached, `RenderedOutput::tail_truncated` records the truncation.

This keeps offline rendering deterministic without requiring every processor to expose an infinite or unbounded tail.

In the offline render chain the limiter runs in the output-rate domain, after any resampling, so only final quantization sits downstream of it — and the limiter's ceiling already reserves headroom for the quantizer's bounded error (the derived output-ceiling guard).

---

## Quality & validation

Representative results from the current benchmark suite (reproduce with `cargo bench`; values differ by CPU, compiler, and load) include:

| Measurement                                                    |                  Result |
| -------------------------------------------------------------- | ----------------------: |
| `LoudnessMeter` integrated loudness parity vs direct `ebur128` |         **0.000000 LU** |
| SoXR resampler THD+N, 44.1 → 48 kHz                            |           **−187.0 dB** |
| Pure-Rust Rubato UltraHigh THD+N                               |           **−204.9 dB** |
| Worst fitted alias attenuation, 96 → 48 kHz                    |           **−290.2 dB** |
| True-peak limiter ceiling                                      |          **−1.00 dBTP** |
| Dynamic loudness compensation at 40 Hz / 3 kHz                 | **+8.41 dB / +2.83 dB** |
| Generation-based parameter snapshot (`audio_lockfree_params_perf`) |            **~7 ns** |

True-peak limiter context: **−1.00 dBTP** is measured on a +0.10 dBTP intersample-stress signal; the legacy sample-peak mode never engages on it (+0.10 dBTP). The full output-chain true-peak probe is enforced as a CI gate: the current quick run measures a worst full-chain output true peak of **−1.000 dBTP** with zero over-limit points.

These values are benchmark evidence from specific machines, configurations, and compiler builds.

They are **not universal performance or quality guarantees**.

### Resampler performance

On the 2026-07-27 core-pinned heavy adapter controls:

```text
SoXR v2
44.1 → 48 kHz   : 8.569 ns/sample    (raw libsoxr: 8.632)
48 → 44.1 kHz   : 7.424 ns/sample    (raw libsoxr: 7.368)
  → statistical tie in both directions

Rubato v17 (same-geometry build)
44.1 → 48 kHz   : 8.182 ns/sample    (raw Rubato: 8.592)
48 → 44.1 kHz   : 7.025 ns/sample    (raw Rubato: 6.908)
  → 4.77% faster forward, tied reverse
```

The project deliberately reports these as benchmark evidence rather than claiming a universal fastest backend. The wider 11-engine matrix is Pareto evidence across different recipes, lanes, and latency policies, not a universal fastest ranking.

See:

* `docs/quality.md`
* `docs/resampler-comparison.md`

for the full methodology, configurations, raw measurements, and reproducible commands.

---

## Installation

```toml
[dependencies]
audio-engine-core = "1"
```

For a minimal pure-Rust DSP-oriented build (no native dependency):

```toml
[dependencies]
audio-engine-core = {
    version = "1",
    default-features = false,
    features = ["rubato"]
}
```

At least one resampler backend must be enabled.

---

## Feature flags

The main Cargo features are:

| Feature       | Default | Purpose                                     |
| ------------- | :-----: | ------------------------------------------- |
| `http`        |    ✓    | HTTP/HTTPS streaming decode                 |
| `loudness-db` |    ✓    | SQLite-backed loudness metadata persistence |
| `rubato`      |    ✓    | Pure-Rust quality-aware resampling          |
| `soxr`        |         | Native SoXR / SoX VHQ resampling            |

All four features are independent; the first three are enabled by default. A
default build is pure Rust: it needs no native library, no vcpkg/pkg-config
probe, and carries no LGPL-2.1 linking obligation.

### `http`

Provides HTTP/HTTPS streaming decode through `reqwest`, including Range streaming and full-download fallback.

`MediaLocation` validates local versus HTTP identity independently of this feature. Without the feature, HTTP locations return `DecoderError::FeatureUnavailable`; `reqwest` and the `NetworkError` type are not compiled.

### `loudness-db`

Provides SQLite-backed persistence for loudness metadata through `rusqlite` (`LoudnessDatabase`, `TrackLoudness`, `LoudnessSourceIdentity`, `DatabaseStats`).

Cache identities use namespaced SHA-256 identities. Signed HTTP URLs are not stored in plaintext, and HTTP records without a validator are always stale.

The EBU R128 measurement, normalization, and true-peak APIs remain available when this feature is disabled; only persistent on-disk caching is removed.

### `soxr`

Enables the native SoXR backend. Opt-in; add it with `features = ["soxr"]`.

This requires the libsoxr native library at build/link time. libsoxr is LGPL-2.1; see [License](#license). Windows (vcpkg or MSYS2) and Unix setup instructions are in `docs/installation.md`.

### `rubato`

Enables the pure-Rust resampling backend. Enabled by default.

When both `soxr` and `rubato` are enabled, SoXR is selected, so adding `soxr` on
top of the default feature set is enough to get the native backend back.

`StreamingResampler` is `Send` under either backend, but it is **not `Sync`** on
the rubato backend, because rubato's `Async<f64>` holds a
`Box<dyn InnerResampler<f64>>` that does not declare `Sync`. This is rarely a
limitation: every method that does work takes `&mut self`, so the usable sharing
pattern is `Arc<Mutex<StreamingResampler>>` (which only needs `Send`) or moving
it to the audio thread. Only `Arc<StreamingResampler>` — which could call the
read-only accessors and nothing else — is rejected. Enable `soxr` if you need the
`Sync` impl itself.

---

## Quick start

Measure integrated loudness:

```rust
use std::path::Path;

use audio_engine_core::{
    LoudnessMeter,
    MediaLocation,
    StreamingDecoder,
};

fn analyze_file(
    path: &Path,
) -> Result<f64, Box<dyn std::error::Error>> {
    let location = MediaLocation::local(path.to_path_buf());
    let mut decoder = StreamingDecoder::open(location)?;

    let info = decoder.info();

    let mut meter =
        LoudnessMeter::new(info.channels, info.sample_rate)?;

    while let Some(samples) = decoder.decode_next()? {
        meter.process(&samples)?;
    }

    Ok(meter.integrated_loudness())
}
```

Runnable examples:

```bash
cargo run --example resample_sine
cargo run --example equalizer_curve
```

The examples do not require external audio files or optional features.

---

## Decoding & format support

Decoding is built on [Symphonia](https://github.com/pdeljanov/Symphonia) 0.6 with all of its bundled codecs/containers compiled in (e.g. WAV, FLAC, MP3, AAC/MP4, OGG/Vorbis); the crate adds no custom codecs and makes the support boundary explicit and tested.

`StreamingDecoder` exposes the decoded sample rate, channel count, and (when known) total frame count and duration via the read-only `decoder.info()`, including the best-effort positional `decoder.info().channel_layout`. It is observation only: the decoder relies on the same values for staging, gapless trimming, and seek arithmetic, so they are not a caller-writable control channel.

* **Unsupported / unrecognized input** returns the typed `DecoderError::UnsupportedFormat`; a container that probes but has no decodable audio track returns `DecoderError::NoAudioTrack`.
* **Corrupt or truncated input** has a defined policy: the decoder either returns a typed error or yields the partial samples it could recover — it never panics and never silently reports a full decode of missing data.
* **Gapless ownership** is an explicit per-codec split: Symphonia owns MP3 and Vorbis packet trim/reset behavior; other codecs retain the crate's Track-level delay/padding fallback. The two paths are mutually exclusive, so delay or padding cannot be trimmed twice. The fallback can only trim what the container declares: Symphonia 0.6's MP4 demuxer does not surface AAC priming/padding metadata (such as `iTunSMPB`), so M4A/AAC currently plays untrimmed, while CAF declares both and is trimmed exactly.
* **Seeking** uses Symphonia's `SeekMode::Coarse` only; a sample-exact (`Accurate`) mode is intentionally not exposed. A coarse seek lands on a packet/frame boundary at or before the requested time — bounded inaccuracy documented as `StreamingDecoder::SEEK_COARSE_TOLERANCE_FRAMES`, with the realized position readable via `decoder.current_frame()`. Track-level encoder delay applies only at the true start of the stream; native MP3/Vorbis decoders consume their packet-local trim and reset preroll after a seek.

---

## Design principles

### Realtime first

The audio callback is a hard realtime boundary.

Prepare before the callback. Communicate through bounded, lock-free mechanisms.

### Explicit ownership

Processors, buffers, lifecycle state, and external resources have explicit ownership.

Hidden global state is avoided.

### Measure before optimizing

Performance and audio-quality claims should be reproducible.

Benchmarks operate against public APIs wherever practical.

### Application agnostic

The core owns processing.

The application owns playback, devices, UI, library state, and runtime policy.

### Typed failure

Errors are represented explicitly instead of relying on logging, silent fallback, or panic-based recovery inside realtime code.

### Composable processing

The high-level pipeline provides a canonical playback path while lower-level processor APIs remain available when applications need custom stage ordering.

---

## Project scope

`audio-engine-core` intentionally does **not** provide:

* UI
* playlist management
* music library management
* audio device ownership
* application lifecycle
* network service orchestration
* player-specific state management

It also deliberately does not own device management (CPAL/WASAPI output streams), desktop UI or Tauri integration, playback queue logic, media-library scanning, HTTP/WebSocket server routes, WebDAV or NetEase integration, or application runtime directories — those stay in the Lyne application crate, with no stable compatibility layer for every internal Lyne use case.

The purpose of this project is narrower:

> **Provide a high-quality, realtime-safe audio processing foundation that applications can build upon.**

---

## Who is this for

Good fit if you are:

* building a Rust music player and want a processing core under it,
* assembling a custom realtime audio pipeline,
* experimenting with high-quality DSP (EQ, crossfeed, saturation, convolution),
* writing offline loudness-analysis tooling.

May not fit if you need: a complete player, a high-level playback API, or an audio device abstraction.

---

## Project status

Stable `1.0.0` release: the public API is fully documented (`missing_docs` is denied at compile time), frozen by committed surface snapshots, and guarded by a SemVer gate in CI — any breaking change fails the build before it can ship. Requires Rust 1.87+ (Symphonia 0.6 itself requires 1.85; the higher crate MSRV reflects existing DSP code in this repository). Used in production as the audio foundation of the Lyne player. Breaking changes are reserved for major version bumps per the policy in `CONTRIBUTING.md`; notable changes are recorded in `CHANGELOG.md`.

---

## Documentation

* `docs/` — architecture and API documentation
* `docs/quality.md` — benchmark methodology and quality validation
* `docs/resampler-comparison.md` — resampler methodology and comparisons
* `docs/installation.md` — native SoXR backend setup per platform
* `examples/` — runnable examples
* `benches/` — public-API benchmarks
* `tests/` — integration and behavioral validation

API documentation:

```bash
cargo doc --open
```

---

## Stability

`audio-engine-core` follows Semantic Versioning for its public API.

The `1.x` series is intended for applications that want a stable audio-processing foundation while retaining control over their own playback architecture.

Breaking API changes require a major version.

---

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

### Native dependency licensing

With the opt-in `soxr` feature, this crate links the SoXR native library (libsoxr), which is distributed under the LGPL-2.1. The Rust source in this crate is MIT OR Apache-2.0, but binaries that statically link libsoxr carry LGPL-2.1 relinking obligations. The default feature set uses the pure-Rust `rubato` backend, does not link libsoxr, and carries no LGPL obligation. See [NOTICE](NOTICE) for details.