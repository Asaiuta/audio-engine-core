# audio-engine-core

Reusable decoder, DSP, loudness, resampling, and streaming pipeline primitives
extracted from the Lyne audio engine.

This crate is the app-agnostic core layer. It is intended for experiments and
integration work around high-quality local audio processing, not as a stable
1.0 SDK yet. The public API is versioned as `0.1.x` and may change while the
larger player continues to evolve.

## What Is Included

- Streaming decode helpers built on Symphonia.
- SoX VHQ resampling wrappers and streaming resampler utilities.
- DSP processors such as EQ, crossfeed, saturation, FFT convolution, dynamic
  loudness, volume smoothing, noise shaping, and spectrum analysis.
- EBU R128 loudness and true-peak measurement helpers.
- Lock-free DSP parameter snapshots and processor adapters for realtime audio
  callback integration.
- A small streaming pipeline/ring-buffer primitive.

## What Is Not Included

- Audio device ownership or CPAL/WASAPI output stream management.
- HTTP/WebSocket server routes.
- Desktop UI, Tauri integration, media-library scanning, playback queue logic,
  WebDAV, NetEase integration, or application runtime directories.
- A stable compatibility layer for every internal Lyne use case.

Those layers remain in the root Lyne application crate.

## Decoding & Format Support

Decoding is built on [Symphonia](https://github.com/pdeljanov/Symphonia) with all
of its bundled codecs/containers compiled in. The crate does not add custom
codecs; instead it makes the support boundary explicit and tested.

- **Supported input** is whatever the bundled Symphonia build can probe and
  decode (e.g. WAV, FLAC, MP3, AAC/MP4, OGG/Vorbis). `StreamingDecoder` exposes
  the decoded sample rate, channel count, and (when known) total frame count and
  duration via `decoder.info`. Positional channel layout is not yet surfaced;
  only the channel *count* is reported.
- **Unsupported / unrecognized input** returns the typed
  `DecoderError::UnsupportedFormat` rather than a generic stringly error. A
  container that probes but has no decodable audio track returns
  `DecoderError::NoAudioTrack`.
- **Corrupt or truncated input** has a defined policy: the decoder either
  returns a typed error or yields the partial samples it could recover. It never
  panics and never silently reports a full successful decode of missing data.

### Seeking

`StreamingDecoder::seek` uses Symphonia's `SeekMode::Coarse` only; a sample-exact
(`Accurate`) mode is intentionally not exposed. A coarse seek lands on a
packet/frame boundary at or before the requested time, so the realized position
has bounded inaccuracy — treat it as "within roughly one packet of the target"
rather than sample-exact. The documented bound is
`StreamingDecoder::SEEK_COARSE_TOLERANCE_FRAMES`, and the realized position is
readable via `decoder.current_frame()`. Encoder-delay (gapless) trimming applies
only at the true start of the stream, never after a seek.

## Cargo Features

Both features below are enabled by default. Disable them with
`default-features = false` to drop the corresponding dependency.

- `http` (default): HTTP/HTTPS streaming decode via `reqwest`, including Range
  streaming and full-download fallback. With this off, `StreamingDecoder` only
  opens local files; passing an `http(s)://` path returns a decoder error, and
  the `reqwest` dependency and `NetworkError` type are not compiled.
- `loudness-db` (default): SQLite-backed loudness metadata persistence
  (`LoudnessDatabase`, `TrackLoudness`, `DatabaseStats`) via `rusqlite`. With
  this off, the EBU R128 measurement helpers (`LoudnessMeter`,
  `LoudnessNormalizer`, `TruePeakDetector`) still work; only the on-disk cache
  is removed.

DSP-only consumers can drop the network and SQLite dependency trees:

```toml
[dependencies]
audio-engine-core = { version = "0.1", default-features = false }
```

## Native Dependency: SoXR

The resampler depends on `soxr`, which requires the SoXR native library during
build/link. SoXR is part of the core crate today, so
`default-features = false` does **not** remove this native dependency.

On Windows, either install SoXR through vcpkg:

```powershell
git clone https://github.com/microsoft/vcpkg.git
cd vcpkg
.\bootstrap-vcpkg.bat
.\vcpkg install soxr:x64-windows-static-md
```

or through MSYS2/MinGW64, which is also the CI path:

```bash
pacman -S mingw-w64-x86_64-libsoxr mingw-w64-x86_64-pkgconf mingw-w64-x86_64-tools
```

On Unix-like systems, install SoXR through your system package manager and make
sure `pkg-config` can locate it.

## Quick Example

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

## Runnable Examples

The `examples/` directory contains self-contained programs that need no audio
files and no optional features:

- `resample_sine` — streams a synthetic 48 kHz sine through the SoX VHQ
  resampler down to 44.1 kHz, demonstrating the chunked feed-then-flush pattern.
- `equalizer_curve` — runs a stereo buffer through the 10-band `Equalizer`.

```bash
cargo run --example resample_sine
cargo run --example equalizer_curve
```

## Realtime Notes

The crate exposes lock-free parameter containers and processor adapters used by
Lyne's realtime callback path. Keep allocations, locks, file I/O, logging, and
network I/O out of an audio callback. Allocate and configure processors before
entering the realtime path, then update parameters through the provided atomic
snapshot types.

## Performance And Audio Quality

These numbers come from the benchmarks in `benches/`, which run entirely against
this crate's public API. They are evidence for one machine and one configuration,
not a universal claim. Reproduce them with `cargo bench`; the exact values will
differ by CPU, compiler version, and load.

```bash
cargo bench --bench audio_callback_chain_perf -- --quick
cargo bench --bench audio_resampler_streaming_perf -- --quick
cargo bench --bench audio_convolver_perf -- --quick
cargo bench --bench audio_lockfree_params_perf -- --quick
cargo bench --bench audio_fir_eq_perf -- --quick
cargo bench --bench audio_quality_measurements -- --quick
```

Drop `--quick` for longer multi-trial runs before citing numbers externally. The
table below records representative local runs; rows should be regenerated after
changing the relevant processing path.

### Realtime processing budget

Per-sample/per-buffer cost of the DSP and resampler paths at a 512-frame buffer.
These exclude the decoder and the OS audio device write; they measure only the
in-crate processing.

| Path | Per sample | Per 512-frame buffer | Bench |
| --- | ---: | ---: | --- |
| DSP chain, no convolver (EQ, `SaturationQuality::Oversampled4x`, crossfeed, convolver slot empty, volume, dynamic loudness, peak limiter) | 149.5 ns | 153.1 us | `audio_callback_chain_perf --quick` |
| DSP chain with convolver and `SaturationQuality::Oversampled4x` | 161.1 ns | 165.0 us | `audio_callback_chain_perf --quick` |
| Streaming resampler, 44.1 kHz to 48 kHz | 7.9 ns/input sample | 8.1 us/input buffer | `audio_resampler_streaming_perf` |
| `FFTConvolver` alone, 256-tap IR, stereo | 14.7 ns | n/a | `audio_convolver_perf --quick` |
| FIR EQ apply, 511-tap IR via `FFTConvolver`, stereo | 19.4 ns | 19.8 us | `audio_fir_eq_perf --quick` |

For a 512-frame buffer at 48 kHz (about 10.7 ms of audio), even the heaviest
chain measured here uses well under one callback period.

### Lock-free parameter reads

The atomic parameter snapshots (`AtomicEqParams`, `AtomicVolumeParams`, and the
rest) are the mechanism for pushing parameter changes into the audio callback
without locks. Reading the full set of cached parameters once per callback costs
about **7 ns** with the generation-based snapshot path, versus ~50 ns for a
naive split-atomic field-by-field read and ~83 ns for an unconditional
`ArcSwap` guard load — an ~86% to ~92% improvement (`audio_lockfree_params_perf`).

### FIR EQ IR generation

`FirEq` designs a linear- or minimum-phase impulse response from 10 band gains;
the IR is then convolved (typically with `FFTConvolver`) to apply the EQ.
Generation is an offline/control-thread cost, not a per-sample one. On this
machine a 511-tap linear-phase design regenerates in ~31 us; minimum-phase
designs cost roughly 3x more because of the extra cepstral phase shaping, and
cost scales with tap count (`audio_fir_eq_perf`).

### FFT convolution routing

`FFTConvolver` keeps the existing overlap-save path for impulse responses up to
4096 taps per channel, which covers the current FIR EQ tap counts. Longer IRs
route to a uniform 1024-frame partitioned tail with an overlap-save head so
room/reverb-length responses avoid one very large callback FFT. The routing and
partition size are exposed as `PARTITIONED_CONVOLUTION_IR_THRESHOLD` and
`PARTITIONED_CONVOLUTION_PARTITION_SIZE`; use `audio_convolver_perf` and
`audio_fir_eq_perf` before changing either value.

### Objective audio-quality measurements

`audio_quality_measurements` generates synthetic f64 signals, runs them through
this crate's processor modules, and analyzes the rendered buffers numerically.
This is native-rendered-buffer evidence, not analog output capture: no audio
device, OS mixer, DAC/ADC loopback, or microphone is involved, and it does not
replace listening tests.

| Metric | Result |
| --- | ---: |
| Resampler THD+N, 44.1 kHz to 48 kHz | -187.0 dB |
| Passband max deviation, 20 Hz to 18 kHz | 0.0013 dB |
| 20 kHz resampler gain | -0.0062 dB |
| Worst fitted alias attenuation, 96 kHz to 48 kHz | -297.0 dB |
| Saturation alias-energy reduction, Direct vs `Oversampled4x` Tube stress | +16.6 dB |
| Limiter output ceiling from a +5.11 dBFS transient | -1.00 dBFS |
| Limiter below-threshold THD+N | -253.9 dB |
| True-peak mode, intersample-stress output (input +0.10 dBTP / -3.01 dBFS) | -1.00 dBTP |
| Sample-peak mode, same input (never engages) | +0.10 dBTP |
| `LoudnessMeter` integrated parity vs direct `ebur128` | 0.000000 LU |
| 10-band EQ +6 dB target response error (62 Hz, 1 kHz, 8 kHz) | 0.0000 dB max |
| Crossfeed high-band level at 2 kHz | -9.18 dB |
| Crossfeed low-vs-high attenuation (80 Hz vs 2 kHz) | -37.63 dB |
| Crossfeed mix-change continuity delta | 0.000e0 (vs 7.992e-3 for a reset simulation) |
| Dynamic loudness low-volume compensation | +8.23 dB at 40 Hz, +2.83 dB at 3 kHz |

The saturation alias probe drives an 11 kHz Tube waveshaper and fits folded
above-Nyquist harmonics. In the current quick run, `Oversampled4x` reduced the
aggregate fitted alias energy from -15.10 dBFS to -31.66 dBFS at equivalent
drive/mix settings.

The listening-DSP rows are single-tone synthetic probes after filter settling.
They validate target response/effect size and parameter-change continuity; they
are not external listening-test or analog-output evidence.

The noise shapers (`NoiseShaper`) redistribute quantization error spectrally
rather than lowering broadband noise: the shaped curves strongly reduce the
2-6 kHz band while pushing energy into 14-18 kHz, for up to a +34.8 dB
ear-band advantage over flat TPDF dither.

The benchmark also includes an optional EBU Tech 3341/3342 expected-value corpus
check. It is skipped unless the `libebur128/test` reference vectors are present
(they are not bundled with this crate); the deterministic `LoudnessMeter` parity
fixtures above always run.

`PeakLimiter` defaults to 4x-oversampled intersample (true-peak) detection: on
an intersample-stress signal whose sample peak sits below the ceiling but whose
true peak is +0.10 dBTP, true-peak mode pulls the output to -1.00 dBTP while the
legacy `LimiterMode::SamplePeak` leaves it untouched at +0.10 dBTP. The limiter
runs at source rate, so one known limitation is kept visible: the full
output-chain true-peak probe is report-only, and resampling plus final
quantization downstream of the limiter can re-introduce intersample peaks. In
the current quick run the worst full-chain output true peak is -0.610 dBTP,
0.390 dB above the -1 dBTP limiter target, so this is still evidence to watch,
not a conformance gate.

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

This crate links the SoXR native library (libsoxr), which is distributed under
the LGPL-2.1. SoXR is currently required even with default features disabled.
The Rust source in this crate is MIT OR Apache-2.0, but binaries that statically
link libsoxr carry LGPL-2.1 relinking obligations. See [NOTICE](NOTICE) for
details.
