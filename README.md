# audio-engine-core

[![CI](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml/badge.svg)](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml)

> A realtime-safe Rust audio processing core for building high-quality music players.

`audio-engine-core` provides decoding, resampling, loudness normalization, DSP
processing, and streaming pipeline primitives — without owning the audio
device, the UI, or the application runtime. Extracted from the Lyne audio
engine as its app-agnostic core layer, it leaves playback, device output, and
library management to your application.

> ⚠️ Status: 0.1.x — actively evolving; the API may change before 1.0. Requires Rust 1.87+.

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
| Resampling | SoX VHQ streaming resampler (native SoXR backend, default) or quality-aware pure-Rust rubato FFT/sinc routing, behind one `process_checked` interface |
| Loudness | EBU R128 integrated loudness + true-peak measurement, offline analysis plus realtime atomic gain application |
| DSP | 10-band IIR biquad `Equalizer`, linear- and minimum-phase `FirEq` (applied via `FFTConvolver`), Bauer crossfeed, saturation with oversampled antialiasing, FFT convolution with partitioned routing for long IRs, dynamic loudness compensation, volume smoothing, true-peak limiter, noise shaping |
| Realtime control | Lock-free generation-based parameter snapshots for pushing changes into the audio callback |
| Streaming | Ring-buffer and pipeline primitives |
| Analysis | Spectrum analyzer, AutoMix analysis, objective quality measurement benches |

## Quick Start

```toml
[dependencies]
audio-engine-core = "0.1"
```

Measure the integrated loudness of a file:

```rust
use audio_engine_core::{LoudnessMeter, StreamingDecoder};

fn analyze_file(path: &str) -> Result<f64, Box<dyn std::error::Error>> {
    let mut decoder = StreamingDecoder::open(path)?;
    let mut meter = LoudnessMeter::new(decoder.info.channels, decoder.info.sample_rate);

    while let Some(samples) = decoder.decode_next()? {
        meter.process(&samples);
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

Processors implement the object-safe `StreamingProcessor` lifecycle. Wrap
caller-owned interleaved `f64` storage in an `AudioBlockMut` and drive it
through `process_checked`, which centrally validates consumed/produced counts
and in-place 1:1 progress:

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
keep the zero-copy in-place fast path; out-of-place calls use caller-provided
output and report `NeedInput` / `NeedOutput` backpressure explicitly.
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
- Resampler THD+N, 44.1 kHz to 48 kHz: **-187.0 dB** (default SoXR backend; pure-Rust rubato UltraHigh measures -216.2 dB, see [docs/quality.md](docs/quality.md))
- Worst fitted alias attenuation, 96 kHz to 48 kHz: **-290.2 dB** (near the analyzer's own numeric floor)
- True-peak limiter: **-1.00 dBTP** on a +0.10 dBTP intersample-stress signal (legacy sample-peak mode never engages: +0.10 dBTP)
- Dynamic loudness low-volume compensation: **+8.41 dB at 40 Hz / +2.83 dB at 3 kHz**

One structural caveat is kept visible: the limiter runs at source rate, so
resampling plus final quantization downstream can in principle re-introduce
intersample peaks, and the full output-chain true-peak probe stays report-only
rather than a conformance gate. In the current quick run the probe meets the
target — worst full-chain output true peak -1.000 dBTP with zero over-limit
points — and it is retained as regression evidence.

The full benchmark commands, JSON report/baseline machinery, processing-budget
tables, and complete measurement tables live in [docs/quality.md](docs/quality.md).

## Installation & Feature Flags

All four Cargo features below are independent; the first three are enabled by
default:

- `http` (default): HTTP/HTTPS streaming decode via `reqwest`, including Range
  streaming and full-download fallback. With this off, `StreamingDecoder` only
  opens local files, an `http(s)://` path returns a decoder error, and `reqwest`
  and the `NetworkError` type are not compiled.
- `loudness-db` (default): SQLite-backed loudness metadata persistence
  (`LoudnessDatabase`, `TrackLoudness`, `DatabaseStats`) via `rusqlite`. With
  this off, the EBU R128 helpers (`LoudnessMeter`, `LoudnessNormalizer`,
  `TruePeakDetector`) still work; only the on-disk cache is removed.
- `soxr` (default): native SoXR resampler backend (SoX VHQ). Requires the
  libsoxr native library at build/link time; libsoxr is LGPL-2.1 (see
  [License](#license)).
- `rubato`: quality-aware pure-Rust backend. `PhaseResponse::Linear` uses FFT
  for common ratios through High quality, while UltraHigh and pathological
  ratios use windowed sinc. `Minimum` and `Maximum` use a setup-designed
  rational polyphase FIR with real-cepstrum spectral factorization; reduced
  rate components above 1024 are rejected rather than silently treated as
  linear phase. No native dependency. Exactly one resampler backend must be
  enabled — enabling neither is a compile error, and when both are enabled,
  `soxr` wins.

A fully pure-Rust, DSP-only build with no native dependency:

```toml
audio-engine-core = { version = "0.1", default-features = false, features = ["rubato"] }
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
rate, channel count, and (when known) total frame count and duration via
`decoder.info`, including the best-effort positional `decoder.info.channel_layout`.

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

Experimental `0.1.x`, actively used as the audio foundation of the Lyne player,
with an evolving API. Requires Rust 1.87+ (Symphonia 0.6 itself requires 1.85;
the higher crate MSRV reflects existing DSP code in this repository). Stable
enough for experimentation, personal applications, and integration testing —
not yet for 1.0-level API compatibility guarantees.

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
